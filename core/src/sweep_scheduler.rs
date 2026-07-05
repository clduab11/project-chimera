use std::str::FromStr;
use std::sync::Arc;

use alloy::consensus::{SignableTransaction, TxEip1559};
use alloy::eips::eip2718::Encodable2718;
use alloy::network::Ethereum;
use alloy::primitives::{Address, Bytes, TxKind, U256};
use alloy::providers::Provider;
use alloy::signers::Signer;
use rust_decimal::Decimal;
use tokio::time::{sleep, Duration, Instant};
use tracing::{info, warn};

use crate::{PacingConfig, PacingEngine, SignerRegistry};

pub struct SweepScheduler<P: Provider<Ethereum> + Clone + Send + Sync + 'static> {
    registry: Arc<SignerRegistry>,
    provider: Arc<P>,
    pacing: Arc<PacingEngine>,
    config: PacingConfig,
    chain_id: u64,
}

impl<P> SweepScheduler<P>
where
    P: Provider<Ethereum> + Clone + Send + Sync + 'static,
{
    pub fn new(
        registry: Arc<SignerRegistry>,
        provider: Arc<P>,
        pacing: Arc<PacingEngine>,
        config: PacingConfig,
        chain_id: u64,
    ) -> Self {
        Self {
            registry,
            provider,
            pacing,
            config,
            chain_id,
        }
    }

    pub async fn run(&self) {
        let sweep_dur = Duration::from_secs(self.config.sweep_interval_secs);
        let refund_dur = Duration::from_secs(self.config.refund_interval_secs);

        tokio::pin! {
            let sweep_timer = sleep(sweep_dur);
            let refund_timer = sleep(refund_dur);
        }

        loop {
            tokio::select! {
                _ = &mut sweep_timer => {
                    self.sweep_cycle().await;
                    sweep_timer.as_mut().reset(Instant::now() + sweep_dur);
                }
                _ = &mut refund_timer => {
                    self.refund_cycle().await;
                    refund_timer.as_mut().reset(Instant::now() + refund_dur);
                }
            }
        }
    }

    async fn sweep_cycle(&self) {
        if self.pacing.is_breaker_active() {
            info!("Sweep skipped: breaker active");
            return;
        }

        let treasury: Address = match &self.config.treasury_address {
            s if s.is_empty() => return,
            s => match s.parse() {
                Ok(a) => a,
                Err(_) => {
                    warn!(treasury = %s, "Invalid treasury_address in config; sweep skipped");
                    return;
                }
            },
        };

        let gas_price = match self.provider.get_gas_price().await {
            Ok(gp) => gp,
            Err(e) => {
                warn!(error = %e, "Sweep: failed to fetch gas price");
                return;
            }
        };

        let min_keep_wei = decimal_eth_to_wei(self.config.sweep_min_keep_eth);
        let gas_cost = U256::from(21000) * U256::from(gas_price);

        for worker in self.registry.worker_addresses() {
            let balance = match self.provider.get_balance(worker).await {
                Ok(b) => b,
                Err(e) => {
                    warn!(worker = %worker, error = %e, "Sweep: balance query failed");
                    continue;
                }
            };

            let spendable = match balance.checked_sub(min_keep_wei) {
                Some(v) => match v.checked_sub(gas_cost) {
                    Some(s) if s > U256::ZERO => s,
                    _ => continue,
                },
                None => continue,
            };

            if self.registry.is_shadow {
                info!(
                    worker = %worker,
                    treasury = %treasury,
                    spendable_wei = %spendable,
                    "[sweep-shadow] worker -> treasury: {} wei native",
                    spendable
                );
                continue;
            }

            let signer = match self.registry.get_signer(&worker) {
                Some(s) => s,
                None => {
                    warn!(worker = %worker, "Sweep: no signer for worker");
                    continue;
                }
            };

            if let Err(e) = signer.sync_from_chain(self.provider.as_ref()).await {
                warn!(worker = %worker, error = %e, "Sweep: nonce sync failed");
                continue;
            }

            let nonce = signer.next_nonce();

            let tx = TxEip1559 {
                chain_id: self.chain_id,
                nonce,
                max_fee_per_gas: gas_price,
                max_priority_fee_per_gas: 0,
                gas_limit: 21000,
                to: TxKind::Call(treasury),
                value: spendable,
                input: Bytes::new(),
                ..Default::default()
            };

            let sig_hash = tx.signature_hash();
            let sig = match signer.signer.sign_hash(&sig_hash).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(worker = %worker, error = %e, "Sweep: signing failed");
                    continue;
                }
            };
            let signed = tx.into_signed(sig);
            let raw_bytes = signed.encoded_2718();

            match self.provider.send_raw_transaction(&raw_bytes).await {
                Ok(_) => {
                    info!(
                        worker = %worker,
                        treasury = %treasury,
                        spendable_wei = %spendable,
                        nonce = nonce,
                        "[sweep-live] broadcast"
                    );
                }
                Err(e) => {
                    warn!(worker = %worker, error = %e, "Sweep: send_raw_transaction failed");
                }
            }
        }
    }

    async fn refund_cycle(&self) {
        if self.pacing.is_breaker_active() {
            info!("Refund skipped: breaker active");
            return;
        }

        let treasury_signer = match self.registry.treasury_signer() {
            Some(s) => s,
            None => return,
        };

        let treasury_addr = treasury_signer.address;

        let min_balance_wei = decimal_eth_to_wei(self.config.min_worker_balance_eth);
        let fund_wei = decimal_eth_to_wei(self.config.refund_topup_eth);

        let gas_price = match self.provider.get_gas_price().await {
            Ok(gp) => gp,
            Err(e) => {
                warn!(error = %e, "Refund: failed to fetch gas price");
                return;
            }
        };

        let mut underfunded: Vec<Address> = Vec::new();
        for worker in self.registry.worker_addresses() {
            let balance = match self.provider.get_balance(worker).await {
                Ok(b) => b,
                Err(e) => {
                    warn!(worker = %worker, error = %e, "Refund: balance query failed");
                    continue;
                }
            };
            if balance < min_balance_wei {
                underfunded.push(worker);
            }
        }

        if underfunded.is_empty() {
            return;
        }

        let needed_total = fund_wei * U256::from(underfunded.len() as u64) * U256::from(2);
        let treasury_balance = match self.provider.get_balance(treasury_addr).await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "Refund: treasury balance query failed");
                return;
            }
        };

        if treasury_balance < needed_total {
            warn!(
                treasury = %treasury_addr,
                treasury_balance_wei = %treasury_balance,
                needed_wei = %needed_total,
                underfunded_count = underfunded.len(),
                "Refund skipped: treasury balance too low"
            );
            return;
        }

        if self.registry.is_shadow {
            for worker in &underfunded {
                info!(
                    treasury = %treasury_addr,
                    worker = %worker,
                    fund_wei = %fund_wei,
                    "[refund-shadow] treasury -> worker: {} wei",
                    fund_wei
                );
            }
            return;
        }

        if let Err(e) = treasury_signer.sync_from_chain(self.provider.as_ref()).await {
            warn!(error = %e, "Refund: treasury nonce sync failed");
            return;
        }

        for worker in &underfunded {
            let nonce = treasury_signer.next_nonce();

            let tx = TxEip1559 {
                chain_id: self.chain_id,
                nonce,
                max_fee_per_gas: gas_price,
                max_priority_fee_per_gas: 0,
                gas_limit: 21000,
                to: TxKind::Call(*worker),
                value: fund_wei,
                input: Bytes::new(),
                ..Default::default()
            };

            let sig_hash = tx.signature_hash();
            let sig = match treasury_signer.signer.sign_hash(&sig_hash).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(worker = %worker, error = %e, "Refund: signing failed");
                    continue;
                }
            };
            let signed = tx.into_signed(sig);
            let raw_bytes = signed.encoded_2718();

            match self.provider.send_raw_transaction(&raw_bytes).await {
                Ok(_) => {
                    info!(
                        treasury = %treasury_addr,
                        worker = %worker,
                        fund_wei = %fund_wei,
                        nonce = nonce,
                        "[refund-live] broadcast"
                    );
                }
                Err(e) => {
                    warn!(worker = %worker, error = %e, "Refund: send_raw_transaction failed");
                }
            }
        }
    }
}

fn decimal_eth_to_wei(eth: Decimal) -> U256 {
    let wei_str = (eth * Decimal::from(1_000_000_000_000_000_000u64))
        .round()
        .to_string();
    U256::from_str(&wei_str).unwrap_or(U256::ZERO)
}
