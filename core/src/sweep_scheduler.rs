use std::sync::Arc;

use alloy::consensus::{SignableTransaction, TxEip1559};
use alloy::eips::eip2718::Encodable2718;
use alloy::network::Ethereum;
use alloy::primitives::{Address, Bytes, TxKind, U256};
use alloy::providers::Provider;
use alloy::signers::Signer;
use rust_decimal::prelude::ToPrimitive;
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

        let min_keep_wei = match native_reserve_wei(
            self.config.sweep_min_keep_eth,
            self.config.min_worker_balance_eth,
        ) {
            Ok(value) => value,
            Err(e) => {
                warn!(error = %e, "Sweep: invalid native reserve configuration");
                return;
            }
        };
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

        let min_balance_wei = match decimal_eth_to_wei(self.config.min_worker_balance_eth) {
            Ok(value) => value,
            Err(e) => {
                warn!(error = %e, "Refund: invalid worker balance threshold");
                return;
            }
        };
        let fund_wei = match decimal_eth_to_wei(self.config.refund_topup_eth) {
            Ok(value) => value,
            Err(e) => {
                warn!(error = %e, "Refund: invalid top-up amount");
                return;
            }
        };

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

        if let Err(e) = treasury_signer
            .sync_from_chain(self.provider.as_ref())
            .await
        {
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

fn native_reserve_wei(
    sweep_min_keep_eth: Decimal,
    min_worker_balance_eth: Decimal,
) -> Result<U256, String> {
    let min_gas_budget_eth = Decimal::from(crate::executor::balance::MIN_GAS_BUDGET_WEI as u64)
        / Decimal::from(1_000_000_000_000_000_000u64);
    decimal_eth_to_wei(
        sweep_min_keep_eth
            .max(min_worker_balance_eth)
            .max(min_gas_budget_eth),
    )
}

fn decimal_eth_to_wei(eth: Decimal) -> Result<U256, String> {
    if eth < Decimal::ZERO {
        return Err(format!("ETH amount must not be negative: {eth}"));
    }
    let scaled = eth * Decimal::from(1_000_000_000_000_000_000u64);
    if scaled.fract() != Decimal::ZERO {
        return Err(format!("ETH amount has precision below one wei: {eth} ETH"));
    }
    scaled
        .to_u128()
        .map(U256::from)
        .ok_or_else(|| format!("ETH amount is outside the supported wei range: {eth} ETH"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn native_reserve_uses_worker_minimum_when_it_is_higher() {
        assert_eq!(
            native_reserve_wei(dec!(0.005), dec!(0.01)).unwrap(),
            U256::from(12_500_000_000_000_000u64)
        );
    }

    #[test]
    fn native_reserve_uses_sweep_minimum_when_it_is_higher() {
        assert_eq!(
            native_reserve_wei(dec!(0.02), dec!(0.01)).unwrap(),
            U256::from(20_000_000_000_000_000u64)
        );
    }

    #[test]
    fn decimal_eth_conversion_is_exact_and_rejects_subwei_precision() {
        assert_eq!(
            decimal_eth_to_wei(dec!(0.123456789012345678)).unwrap(),
            U256::from(123_456_789_012_345_678u64)
        );
        assert!(decimal_eth_to_wei(dec!(0.0000000000000000001)).is_err());
    }

    #[test]
    fn sweep_reserve_guarantees_min_gas_budget_floor() {
        let min_gas_budget_wei = crate::executor::balance::MIN_GAS_BUDGET_WEI as u64;
        let reserve = native_reserve_wei(dec!(0.005), dec!(0.01))
            .expect("default config values must produce a valid reserve");
        assert!(
            reserve >= U256::from(min_gas_budget_wei),
            "sweep reserve {reserve} wei must be >= MIN_GAS_BUDGET_WEI ({min_gas_budget_wei} wei / 0.0125 ETH)"
        );
        assert_eq!(
            reserve,
            U256::from(min_gas_budget_wei),
            "with default config (0.005 sweep / 0.01 worker), reserve should equal the MIN_GAS_BUDGET floor of 0.0125 ETH"
        );
    }
}
