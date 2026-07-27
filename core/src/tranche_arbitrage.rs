//! Bespoke Tranche Opportunity — Sequential Capital Reallocation Engine.
//!
//! This module implements the hash-pattern scanner for target
//! `Executor.execute(bytes)` transactions and the transaction bundler that
//! constructs atomic pre/post-trade bundles for submission via Flashbots
//! Protect.
//!
//! ## Architecture
//! 1. [`TrancheScanner`] — monitors the mempool for pending transactions
//!    targeting the deployed Executor contract with the `execute(bytes)`
//!    selector (keccak: 0x09c5eabe).
//! 2. [`TrancheBundler`] — builds a three-transaction atomic bundle: pre-trade
//!    buy → Executor.execute liquidation → post-trade sell, submitted via
//!    `eth_sendBundle` to a Flashbots Protect relay.
//! 3. [`AtomicPacketBundler`] — orchestrates triangular capture sequences with
//!    deterministic inclusion via priority fee optimization.
//!
//! Safety invariant: `CHIMERA_TRANCHE_ENABLED` must be `true` before any
//! bundles are submitted. Default is `false` (shadow-first principle).

use crate::error::ChimeraError;
use alloy::primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// keccak256("execute(bytes)") first 4 bytes = 0x09c5eabe
pub const EXECUTOR_EXECUTE_SELECTOR: [u8; 4] = [0x09, 0xc5, 0xea, 0xbe];

/// The Flashbots Protect **user RPC**, which serves Ethereum L1 (chain 1).
///
/// It is **not** a default for Base and **not** a bundle relay. Every clause of
/// this constant's former doc comment ("Default Flashbots Protect relay for Base
/// mainnet") was wrong: `eth_chainId` here returns `0x1`, bundle submission
/// belongs to `relay.flashbots.net`, and `eth_sendBundle` is documented for
/// Mainnet and Sepolia only. Retained solely so existing imports keep compiling;
/// `PacingConfig::default_flashbots_relay()` deliberately returns an empty
/// string instead, because there is no correct default for Base.
pub const FLASHBOTS_RELAY_DEFAULT: &str = "https://rpc.flashbots.net";

// ---------------------------------------------------------------------------
// Tranche Scanner
// ---------------------------------------------------------------------------

/// Scans the mempool for pending transactions that call
/// `Executor.execute(bytes)` on the known Executor contract address.
///
/// When a matching pending transaction is observed, its calldata is decoded
/// into a [`TrancheTarget`] for the bundler.
#[derive(Debug, Clone)]
pub struct TrancheScanner {
    /// The deployed Executor contract address to watch.
    pub target_executor: Address,
    /// Chain ID for this scanner.
    pub chain_id: u64,
}

impl TrancheScanner {
    /// Create a new scanner for the given Executor contract and chain.
    pub fn new(target_executor: Address, chain_id: u64) -> Self {
        Self {
            target_executor,
            chain_id,
        }
    }

    /// Returns `true` if a pending transaction targets our Executor with
    /// the `execute(bytes)` selector.
    ///
    /// Checks both the `to` address AND the first 4 bytes of calldata.
    pub fn matches(&self, to: Address, calldata: &[u8]) -> bool {
        to == self.target_executor
            && calldata.len() >= 4
            && calldata[..4] == EXECUTOR_EXECUTE_SELECTOR
    }

    /// Decode the inner `Executor.execute(bytes)` payload into a
    /// [`TrancheTarget`]. Returns `None` if the calldata is malformed
    /// or does not contain the expected 11-word payload.
    ///
    /// The `execute(bytes)` ABI encodes:
    ///   - 4 bytes: selector (0x09c5eabe)
    ///   - 32 bytes: offset to bytes data (always 0x40)
    ///   - 32 bytes: length of inner bytes (0x160 = 352 = 11 * 32)
    ///   - 11 * 32 bytes: ExecutorPayload struct (asset, amount, collateral,
    ///     user, debtToCover, receiveAToken, dexRouter, amountOutMin,
    ///     minProfit, tip, deadline)
    pub fn decode_target_tx(
        &self,
        calldata: &[u8],
        tx_hash: B256,
        gas_price_wei: u128,
        max_priority_fee: u128,
    ) -> Option<TrancheTarget> {
        // Minimum length: 4 (selector) + 32 (offset) + 32 (length) + 11*32 = 420
        if calldata.len() < 420 {
            return None;
        }

        // The inner bytes payload starts at byte 68 (after selector + offset +
        // length words).
        let payload_start = 4 + 32 + 32;
        let read_word = |word_idx: usize| -> Option<[u8; 32]> {
            let start = payload_start + word_idx * 32;
            let end = start + 32;
            if calldata.len() < end {
                return None;
            }
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&calldata[start..end]);
            Some(buf)
        };

        let asset = Address::from_slice(&read_word(0)?[12..32]);
        let amount = U256::from_be_bytes(read_word(1)?);
        let collateral = Address::from_slice(&read_word(2)?[12..32]);
        let user = Address::from_slice(&read_word(3)?[12..32]);
        let debt_to_cover = U256::from_be_bytes(read_word(4)?);
        // word 5: receiveAToken (bool, unused for target decoding)
        let dex_router = Address::from_slice(&read_word(6)?[12..32]);
        // words 7-10: amountOutMin, minProfit, tip, deadline (unused)

        Some(TrancheTarget {
            tx_hash,
            asset,
            amount,
            collateral,
            user,
            debt_to_cover,
            dex_router,
            gas_price_wei,
            max_priority_fee,
        })
    }
}

// ---------------------------------------------------------------------------
// Tranche Target
// ---------------------------------------------------------------------------

/// A decoded target liquidation transaction from the mempool.
///
/// Contains the essential fields extracted from the `Executor.execute(bytes)`
/// calldata payload so the bundler can construct pre and post trades.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrancheTarget {
    /// Hash of the target transaction in the mempool.
    pub tx_hash: B256,
    /// Flash-loan asset (the debt asset being repaid).
    pub asset: Address,
    /// Flash-loan amount (in the asset's native units).
    pub amount: U256,
    /// Collateral asset to be seized by the liquidation.
    pub collateral: Address,
    /// User address being liquidated.
    pub user: Address,
    /// Debt amount to cover (in debt asset native units).
    pub debt_to_cover: U256,
    /// DEX router used for the collateral→debt swap.
    pub dex_router: Address,
    /// Observed gas price of the target transaction (wei).
    pub gas_price_wei: u128,
    /// Observed priority fee of the target transaction (wei).
    pub max_priority_fee: u128,
}

// ---------------------------------------------------------------------------
// Flashbots Bundle
// ---------------------------------------------------------------------------

/// A Flashbots-protected transaction bundle for atomic inclusion.
///
/// Submitted via `eth_sendBundle` to a Flashbots Protect relay. The relay
/// ensures the bundle is included atomically in the target block, preventing
/// front-running by other searchers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashbotsBundle {
    /// RLP-encoded signed transactions in execution order:
    /// [pre_trade, target, post_trade]
    pub txs: Vec<Bytes>,
    /// Target block number for inclusion.
    pub target_block: u64,
    /// Earliest timestamp for inclusion (seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_timestamp: Option<u64>,
    /// Latest timestamp for inclusion (seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_timestamp: Option<u64>,
}

// ---------------------------------------------------------------------------
// Tranche Status & Result
// ---------------------------------------------------------------------------

/// Status of a submitted Bespoke Tranche Opportunity bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrancheStatus {
    /// Bundle submitted, awaiting inclusion.
    Pending,
    /// Bundle confirmed on-chain; pre and post trades settled.
    Confirmed,
    /// Bundle reverted (one or more txs failed).
    Reverted,
    /// Bundle submission or inclusion failed.
    Failed,
}

/// Audit record for a Bespoke Tranche Opportunity execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrancheResult {
    /// Hash returned by `eth_sendBundle`.
    pub bundle_hash: B256,
    /// Hash of the target Executor.execute transaction.
    pub target_tx_hash: B256,
    /// Hash of the pre-execution buy trade (if submitted).
    pub pre_trade_tx_hash: Option<B256>,
    /// Hash of the post-execution sell trade (if submitted).
    pub post_trade_tx_hash: Option<B256>,
    /// Block in which the bundle was included.
    pub block_number: u64,
    /// Net profit in wei (post_trade_output - pre_trade_input - gas).
    pub profit_wei: U256,
    /// Total gas spent across all three transactions (wei).
    pub gas_spent_wei: U256,
    /// Final status of the bundle.
    pub status: TrancheStatus,
}

// ---------------------------------------------------------------------------
// Tranche Bundler
// ---------------------------------------------------------------------------

/// Builds and submits three-transaction Bespoke Tranche Opportunity bundles
/// via a Flashbots Protect relay.
///
/// The bundle structure:
///   1. **Pre-trade**: Buy the collateral asset before the liquidation.
///   2. **Target**: The original `Executor.execute(bytes)` transaction.
///   3. **Post-trade**: Sell the collateral asset after the liquidation,
///      capturing the price-impact delta.
pub struct TrancheBundler {
    /// Chain ID for transaction construction.
    pub chain_id: u64,
    /// Flashbots Protect relay endpoint (e.g. `https://rpc.flashbots.net`).
    pub flashbots_relay_url: String,
    /// HTTP client for relay communication.
    client: reqwest::Client,
}

impl TrancheBundler {
    /// Create a new bundler.
    pub fn new(chain_id: u64, flashbots_relay_url: String) -> Self {
        Self {
            chain_id,
            flashbots_relay_url,
            client: reqwest::Client::new(),
        }
    }

    /// Build a [`FlashbotsBundle`] from a target transaction and pre/post
    /// trade signed transactions.
    ///
    /// `pre_trade_raw` and `post_trade_raw` are RLP-encoded signed
    /// transactions. `target_raw` is the intercepted raw transaction
    /// (RLP-encoded signed Executor.execute).
    #[allow(clippy::too_many_arguments)]
    pub fn build_bundle(
        &self,
        pre_trade_raw: Bytes,
        target_raw: Bytes,
        post_trade_raw: Bytes,
        target_block: u64,
        max_timestamp: Option<u64>,
    ) -> FlashbotsBundle {
        FlashbotsBundle {
            txs: vec![pre_trade_raw, target_raw, post_trade_raw],
            target_block,
            min_timestamp: None,
            max_timestamp,
        }
    }

    /// Submit a bundle to the Flashbots Protect relay via `eth_sendBundle`.
    ///
    /// Returns the bundle hash on success.
    pub async fn submit_bundle(&self, bundle: &FlashbotsBundle) -> Result<B256, ChimeraError> {
        let txs_hex: Vec<String> = bundle
            .txs
            .iter()
            .map(|tx| format!("0x{}", hex::encode(tx)))
            .collect();

        let params = serde_json::json!({
            "txs": txs_hex,
            "blockNumber": format!("0x{:x}", bundle.target_block),
        });

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendBundle",
            "params": [params],
        });

        let response = self
            .client
            .post(&self.flashbots_relay_url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ChimeraError::RpcError(format!("Flashbots relay request failed: {e}")))?;

        let status = response.status();
        let response_body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ChimeraError::RpcError(format!("Flashbots relay response parse: {e}")))?;

        if !status.is_success() {
            return Err(ChimeraError::RpcError(format!(
                "Flashbots relay returned {status}: {response_body}"
            )));
        }

        // Check for JSON-RPC error
        if let Some(error) = response_body.get("error") {
            return Err(ChimeraError::RpcError(format!(
                "eth_sendBundle error: {error}"
            )));
        }

        // Parse the bundle hash from the result
        let result = response_body
            .get("result")
            .and_then(|r| r.get("bundleHash"))
            .and_then(|h| h.as_str())
            .ok_or_else(|| {
                ChimeraError::RpcError("eth_sendBundle response missing bundleHash".into())
            })?;

        B256::from_str(result)
            .map_err(|e| ChimeraError::RpcError(format!("Invalid bundleHash {result}: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Atomic Packet & tranche Bundler
// ---------------------------------------------------------------------------

/// Execution window for atomic bundle inclusion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionWindow {
    /// Earliest block for inclusion.
    pub min_block: u64,
    /// Latest block for inclusion.
    pub max_block: u64,
    /// Earliest timestamp (seconds).
    pub min_timestamp: Option<u64>,
    /// Latest timestamp (seconds).
    pub max_timestamp: Option<u64>,
}

/// Atomic three-leg packet for tranche execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicPacket {
    /// Pre-trade: buy collateral asset before victim execution.
    pub pre_trade_tx: Bytes,
    /// Victim: intercepted Executor.execute transaction.
    pub target_tx: Bytes,
    /// Post-trade: sell collateral asset after victim execution.
    pub post_trade_tx: Bytes,
    /// Execution window for inclusion.
    pub execution_window: ExecutionWindow,
}

/// Builds and submits triangular tranche bundles with deterministic inclusion.
///
/// Extends [`TrancheBundler`] with priority fee optimization and collision
/// exclusion for competitive mempool environments.
///
/// **Not reachable.** Nothing constructs this type; see the call-site census in
/// `docs/decision-2026-07-26-tranche-venue.md` §4. Retained pending the
/// withdraw/proceed decision in §9.1, not because it is wired.
pub struct AtomicPacketBundler {
    /// Underlying bundler for transaction construction.
    pub bundler: TrancheBundler,
    /// Priority fee multiplier for inclusion guarantee.
    pub priority_fee_multiplier: u64,
    /// Maximum acceptable gas price (gwei).
    pub max_gas_gwei: u64,
}

impl AtomicPacketBundler {
    /// Create a new tranche bundler.
    pub fn new(
        chain_id: u64,
        flashbots_relay_url: String,
        priority_fee_multiplier: u64,
        max_gas_gwei: u64,
    ) -> Self {
        Self {
            bundler: TrancheBundler::new(chain_id, flashbots_relay_url),
            priority_fee_multiplier,
            max_gas_gwei,
        }
    }

    /// Construct a three-leg atomic packet.
    ///
    /// Builds pre-trade → victim → post-trade sequence with execution window.
    #[allow(clippy::too_many_arguments)]
    pub fn construct_triangle(
        &self,
        pre_trade_raw: Bytes,
        target_raw: Bytes,
        post_trade_raw: Bytes,
        min_block: u64,
        max_block: u64,
        max_timestamp: Option<u64>,
    ) -> AtomicPacket {
        AtomicPacket {
            pre_trade_tx: pre_trade_raw,
            target_tx: target_raw,
            post_trade_tx: post_trade_raw,
            execution_window: ExecutionWindow {
                min_block,
                max_block,
                min_timestamp: None,
                max_timestamp,
            },
        }
    }

    /// Submit an atomic packet via Flashbots Protect.
    ///
    /// Returns the bundle hash on success.
    pub async fn submit_atomic_packet(&self, packet: &AtomicPacket) -> Result<B256, ChimeraError> {
        let bundle = self.bundler.build_bundle(
            packet.pre_trade_tx.clone(),
            packet.target_tx.clone(),
            packet.post_trade_tx.clone(),
            packet.execution_window.max_block,
            packet.execution_window.max_timestamp,
        );

        self.bundler.submit_bundle(&bundle).await
    }

    /// Verify inclusion of a submitted bundle.
    ///
    /// **Unimplemented, and deliberately errors rather than reporting a
    /// result.** Inclusion verification requires a relay that serves this
    /// chain; none serves Base (8453). Returning `Ok(false)` here — as this
    /// previously did — is indistinguishable from "checked, and it was not
    /// included", which would let a caller book an unverified outcome as a
    /// confirmed miss. Fail loudly instead.
    pub async fn verify_inclusion(
        &self,
        bundle_hash: B256,
        target_block: u64,
    ) -> Result<bool, ChimeraError> {
        Err(ChimeraError::RpcError(format!(
            "bundle inclusion verification is not implemented: no relay serves \
             chain {} (bundle {bundle_hash}, target block {target_block})",
            self.bundler.chain_id
        )))
    }

    /// Calculate optimal priority fee for inclusion.
    ///
    /// Uses base fee and priority fee multiplier to determine gas price.
    pub fn calculate_priority_fee(&self, base_fee_gwei: u64) -> u64 {
        let calculated = base_fee_gwei.saturating_mul(self.priority_fee_multiplier);
        calculated.min(self.max_gas_gwei)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn make_scanner() -> TrancheScanner {
        let executor = address!("0x98Fc3F5c95b34BF3197e1349a2932F6177D336Ef");
        TrancheScanner::new(executor, 8453)
    }

    #[test]
    fn test_scanner_matches_known_executor_and_selector() {
        let scanner = make_scanner();
        let mut calldata = vec![0x09, 0xc5, 0xea, 0xbe];
        calldata.extend(vec![0u8; 416]); // pad to minimum length
        assert!(scanner.matches(scanner.target_executor, &calldata));
    }

    #[test]
    fn test_scanner_rejects_wrong_selector() {
        let scanner = make_scanner();
        let calldata = vec![0xde, 0xad, 0xbe, 0xef, 0x00];
        assert!(!scanner.matches(scanner.target_executor, &calldata));
    }

    #[test]
    fn test_scanner_rejects_wrong_address() {
        let scanner = make_scanner();
        let other = address!("0x1111111111111111111111111111111111111111");
        let mut calldata = vec![0x09, 0xc5, 0xea, 0xbe];
        calldata.extend(vec![0u8; 416]);
        assert!(!scanner.matches(other, &calldata));
    }

    #[test]
    fn test_decode_target_tx_malformed_short_calldata_returns_none() {
        let scanner = make_scanner();
        let calldata = vec![0x09, 0xc5, 0xea, 0xbe];
        let target = scanner.decode_target_tx(&calldata, B256::ZERO, 1_000_000_000, 100_000_000);
        assert!(target.is_none());
    }

    #[test]
    fn test_flashbots_bundle_serialization() {
        let bundle = FlashbotsBundle {
            txs: vec![Bytes::from_static(&[0x01, 0x02])],
            target_block: 12345,
            min_timestamp: None,
            max_timestamp: Some(1781661993),
        };
        let json = serde_json::to_string(&bundle).unwrap();
        assert!(json.contains("12345"));
        assert!(json.contains("0x0102"));
    }
}
