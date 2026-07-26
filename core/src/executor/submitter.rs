//! RPC-backed transaction submitter.
//!
//! [`RpcSubmitter`] implements [`TransactionExecutor`] using any alloy
//! [`Provider`].  It handles gas estimation, nonce management, exponential-
//! backoff retries, dry-run simulation via `eth_call`, and receipt polling.

use crate::executor::{BuiltTransaction, SubmissionReceipt, TransactionExecutor};
use crate::ChimeraError;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, B256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use async_trait::async_trait;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

/// RPC-backed transaction submitter with retry logic, nonce management
/// and optional dry-run mode.
///
/// Generic over any alloy [`Provider`] so it can be used with HTTP, WS or
/// custom transport layers.
pub struct RpcSubmitter<P: Provider> {
    provider: P,
    chain_id: u64,
    max_retries: u32,
    retry_backoff_ms: u64,
    dry_run: bool,
    submit_attempts: std::sync::atomic::AtomicU64,
}

impl<P: Provider> RpcSubmitter<P> {
    /// Create a new submitter.
    pub fn new(provider: P, chain_id: u64) -> Self {
        Self {
            provider,
            chain_id,
            max_retries: 3,
            retry_backoff_ms: 500,
            dry_run: false,
            submit_attempts: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Set the maximum number of retry attempts.
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Set the initial retry backoff in milliseconds.
    pub fn with_retry_backoff_ms(mut self, ms: u64) -> Self {
        self.retry_backoff_ms = ms;
        self
    }

    /// Enable dry-run mode: transactions are only simulated via `eth_call`.
    pub fn with_dry_run(mut self, enabled: bool) -> Self {
        self.dry_run = enabled;
        self
    }

    /// Return the underlying provider reference.
    pub fn provider(&self) -> &P {
        &self.provider
    }

    /// Fetch the pending nonce for `from` address.
    ///
    /// Queries the node for the transaction count at the latest block so
    /// that the returned nonce reflects the current on-chain state.
    pub async fn fetch_nonce(&self, from: Address) -> Result<u64, ChimeraError> {
        let count = self
            .provider
            .get_transaction_count(from)
            .await
            .map_err(|e| ChimeraError::RpcError(format!("nonce fetch failed: {e}")))?;
        Ok(count)
    }

    /// Number of submit-path invocations (includes dry-run).
    /// Used by integration tests to verify shadow mode never reaches the submit seam.
    pub fn submit_attempt_count(&self) -> u64 {
        self.submit_attempts
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Simulate a transaction via `eth_call` without broadcasting.
    ///
    /// This is useful for validating that a transaction will not revert
    /// before spending gas on-chain.
    pub async fn dry_run(&self, tx: &BuiltTransaction) -> Result<Bytes, ChimeraError> {
        let req = Self::build_request(tx);
        let result = self
            .provider
            .call(req)
            .await
            .map_err(|e| ChimeraError::RpcError(format!("eth_call failed: {e}")))?;
        Ok(result)
    }

    /// Poll for a transaction receipt with exponential backoff.
    ///
    /// Retries up to [`Self::max_retries`] times, doubling the wait
    /// interval after each attempt.  If no receipt is found after all
    /// retries, returns a [`ChimeraError::TimeoutError`].
    ///
    /// Public because the orchestrator's live path signs and broadcasts its own
    /// raw transaction (it needs worker-signer nonce control that [`Self::submit`]
    /// does not provide) and must still confirm inclusion through this same
    /// backoff logic rather than a second implementation.
    pub async fn poll_receipt(&self, tx_hash: B256) -> Result<SubmissionReceipt, ChimeraError> {
        let mut backoff = self.retry_backoff_ms;
        let mut attempts = 0u32;

        loop {
            match self
                .provider
                .get_transaction_receipt(tx_hash)
                .await
                .map_err(|e| ChimeraError::RpcError(format!("receipt fetch failed: {e}")))?
            {
                Some(receipt) => {
                    return Ok(SubmissionReceipt {
                        tx_hash,
                        block_number: receipt.block_number,
                        gas_used: Some(receipt.gas_used),
                        status: receipt.status(),
                    });
                }
                None if attempts < self.max_retries => {
                    sleep(Duration::from_millis(backoff)).await;
                    backoff = backoff.saturating_mul(2);
                    attempts += 1;
                }
                None => {
                    return Err(ChimeraError::TimeoutError(format!(
                        "receipt not found after {attempts} attempts for tx {tx_hash}"
                    )));
                }
            }
        }
    }

    /// Convert a [`BuiltTransaction`] into an alloy [`TransactionRequest`].
    fn build_request(tx: &BuiltTransaction) -> TransactionRequest {
        TransactionRequest::default()
            .with_to(tx.to)
            .with_value(tx.value)
            .with_input(tx.data.clone())
            .with_gas_limit(tx.gas_limit)
            .with_max_fee_per_gas(tx.max_fee_per_gas)
            .with_max_priority_fee_per_gas(tx.max_priority_fee_per_gas)
            .with_nonce(tx.nonce)
    }
}

#[async_trait]
impl<P: Provider + Send + Sync> TransactionExecutor for RpcSubmitter<P> {
    async fn submit(&self, tx: BuiltTransaction) -> Result<B256, ChimeraError> {
        self.submit_attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.dry_run {
            info!(target: "chimera::executor", "dry-run mode: simulating transaction");
            self.dry_run(&tx).await?;
            return Ok(B256::ZERO);
        }

        // 1. Estimate gas if not already set.
        let gas_limit = if tx.gas_limit == 0 {
            self.estimate_gas(&tx).await?
        } else {
            tx.gas_limit
        };

        let mut tx = tx;
        tx.gas_limit = gas_limit;

        // 2. Build the transaction request.
        let req = Self::build_request(&tx);

        // 3. Sign and send with retries.
        let mut backoff = self.retry_backoff_ms;
        let mut last_error: Option<ChimeraError> = None;

        for attempt in 0..=self.max_retries {
            match self.provider.send_transaction(req.clone()).await {
                Ok(pending) => {
                    let tx_hash = *pending.tx_hash();
                    info!(target: "chimera::executor", %tx_hash, "transaction broadcast");

                    // 4. Poll for receipt.
                    let receipt = self.poll_receipt(tx_hash).await?;
                    if receipt.status {
                        info!(
                            target: "chimera::executor",
                            %tx_hash,
                            block = ?receipt.block_number,
                            "transaction confirmed"
                        );
                        return Ok(tx_hash);
                    } else {
                        return Err(ChimeraError::RpcError(format!(
                            "transaction {tx_hash} reverted on-chain"
                        )));
                    }
                }
                Err(e) => {
                    warn!(
                        target: "chimera::executor",
                        attempt,
                        error = %e,
                        "send failed, retrying"
                    );
                    last_error = Some(ChimeraError::RpcError(format!("send failed: {e}")));
                    if attempt < self.max_retries {
                        sleep(Duration::from_millis(backoff)).await;
                        backoff = backoff.saturating_mul(2);
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            ChimeraError::RpcError("transaction submission failed after max retries".into())
        }))
    }

    async fn estimate_gas(&self, tx: &BuiltTransaction) -> Result<u64, ChimeraError> {
        let req = Self::build_request(tx);
        self.provider
            .estimate_gas(req)
            .await
            .map_err(|e| ChimeraError::RpcError(format!("gas estimation failed: {e}")))
    }

    fn chain_id(&self) -> u64 {
        self.chain_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    #[test]
    fn test_exponential_backoff_calculation() {
        let initial = 500u64;
        let mut backoff = initial;
        let mut delays = Vec::new();
        for _ in 0..3 {
            delays.push(backoff);
            backoff = backoff.saturating_mul(2);
        }
        assert_eq!(delays, vec![500, 1000, 2000]);
    }

    #[test]
    fn test_built_transaction_defaults() {
        let tx = BuiltTransaction {
            to: Address::ZERO,
            data: Bytes::from(vec![0xab, 0xcd]),
            value: U256::from(1_000_000),
            gas_limit: 200_000,
            max_fee_per_gas: 50_000_000_000,
            max_priority_fee_per_gas: 1_500_000_000,
            nonce: 42,
        };
        assert_eq!(tx.to, Address::ZERO);
        assert_eq!(tx.value, U256::from(1_000_000));
        assert_eq!(tx.gas_limit, 200_000);
        assert_eq!(tx.nonce, 42);
    }

    #[test]
    fn test_submission_receipt_status() {
        let receipt = SubmissionReceipt {
            tx_hash: B256::ZERO,
            block_number: Some(12345),
            gas_used: Some(21000),
            status: true,
        };
        assert!(receipt.status);
        assert_eq!(receipt.block_number, Some(12345));
        assert_eq!(receipt.gas_used, Some(21000));
    }
}
