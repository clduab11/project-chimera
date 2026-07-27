//! Tranche Orchestrator — Three-Leg Bundle Construction & Submission.
//!
//! This module coordinates the full tranche execution lifecycle:
//! 1. Receives scored targets from MempoolPredator
//! 2. Constructs pre-trade and post-trade transactions
//! 3. Builds atomic three-leg packets
//! 4. Submits via Flashbots Protect with priority fee optimization
//! 5. Verifies inclusion and records results
//!
//! ## Triangle Capture Theory
//! The tranche strategy exploits price impact from large executions:
//! - **Pre-trade**: Buy the asset the victim will purchase, driving up price
//! - **Victim**: The intercepted Executor.execute transaction executes at worse price
//! - **Post-trade**: Sell the acquired asset at the inflated price, capturing the delta
//!
//! Atomicity is guaranteed via Flashbots bundle submission — all three
//! transactions either succeed together or fail together.

use crate::mempool_predator::{MempoolPredator, ScoredTarget};
use crate::tranche_arbitrage::{
    AtomicPacket, ExecutionWindow, TrancheBundler, TrancheResult, TrancheStatus,
};
use alloy::primitives::{Address, Bytes, B256, U256};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Orchestrator State
// ---------------------------------------------------------------------------

/// Current state of the tranche orchestrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrchestratorState {
    /// Idle, scanning for opportunities.
    Idle,
    /// Analyzing a potential target.
    Analyzing,
    /// Constructing the three-leg packet.
    Constructing,
    /// Submitting to Flashbots relay.
    Submitting,
    /// Waiting for block inclusion.
    WaitingInclusion,
    /// Bundle confirmed on-chain.
    Confirmed,
    /// Bundle reverted or failed.
    Failed,
}

/// Configuration for the tranche orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrancheConfig {
    /// Priority fee multiplier for inclusion guarantee.
    pub priority_fee_multiplier: u64,
    /// Maximum gas price willing to pay (gwei).
    pub max_gas_gwei: u64,
    /// Minimum profit threshold in USD.
    pub min_profit_usd: Decimal,
    /// Maximum blocks to wait for inclusion.
    pub max_wait_blocks: u64,
    /// Enable shadow mode (simulate without submitting).
    pub shadow_mode: bool,
}

impl Default for TrancheConfig {
    fn default() -> Self {
        Self {
            priority_fee_multiplier: 3,
            max_gas_gwei: 50_000,
            min_profit_usd: Decimal::from(10),
            max_wait_blocks: 6,
            shadow_mode: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Tranche Orchestrator
// ---------------------------------------------------------------------------

/// Coordinates the full tranche execution lifecycle.
pub struct TrancheOrchestrator {
    /// Mempool predator for target identification.
    predator: MempoolPredator,
    /// Tranche bundler for packet construction and submission.
    bundler: TrancheBundler,
    /// Current orchestrator state.
    state: OrchestratorState,
    /// Configuration parameters.
    config: TrancheConfig,
    /// Current block number.
    current_block: u64,
    /// Base fee in wei.
    base_fee_wei: u128,
}

impl TrancheOrchestrator {
    /// Create a new orchestrator instance.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        predator: MempoolPredator,
        bundler: TrancheBundler,
        config: TrancheConfig,
        current_block: u64,
        base_fee_wei: u128,
    ) -> Self {
        Self {
            predator,
            bundler,
            state: OrchestratorState::Idle,
            config,
            current_block,
            base_fee_wei,
        }
    }

    /// Process a pending transaction for tranche opportunity.
    ///
    /// Returns a [`TrancheResult`] if an opportunity is identified and
    /// successfully executed, `None` otherwise.
    pub async fn process_pending_tx(
        &mut self,
        to: Address,
        calldata: &[u8],
        tx_hash: B256,
        gas_price_wei: u128,
        max_priority_fee: u128,
    ) -> Option<TrancheResult> {
        self.state = OrchestratorState::Analyzing;

        // Step 1: Identify friction point
        let scored = self.predator.identify_friction_points(
            to,
            calldata,
            tx_hash,
            gas_price_wei,
            max_priority_fee,
        )?;

        // Step 2: Construct and submit packet
        self.state = OrchestratorState::Constructing;
        let packet = self.construct_packet(&scored)?;

        // Step 3: Submit (or simulate in shadow mode)
        self.state = OrchestratorState::Submitting;
        let result = if self.config.shadow_mode {
            self.simulate_submission(&packet, &scored)
        } else {
            let bundle = self.bundler.build_bundle(
                packet.pre_trade_tx.clone(),
                packet.target_tx.clone(),
                packet.post_trade_tx.clone(),
                packet.execution_window.max_block,
                packet.execution_window.max_timestamp,
            );
            match self.bundler.submit_bundle(&bundle).await {
                Ok(bundle_hash) => self.build_result(bundle_hash, &scored),
                Err(_e) => {
                    self.state = OrchestratorState::Failed;
                    Some(TrancheResult {
                        bundle_hash: B256::ZERO,
                        target_tx_hash: scored.target.tx_hash,
                        pre_trade_tx_hash: None,
                        post_trade_tx_hash: None,
                        block_number: self.current_block,
                        profit_wei: U256::ZERO,
                        gas_spent_wei: U256::ZERO,
                        status: TrancheStatus::Failed,
                    })
                }
            }
        };

        if let Some(ref r) = result {
            if matches!(r.status, TrancheStatus::Confirmed) {
                self.state = OrchestratorState::Confirmed;
            } else {
                self.state = OrchestratorState::Failed;
            }
        }

        result
    }

    /// Construct an atomic packet from a scored target.
    fn construct_packet(&self, _scored: &ScoredTarget) -> Option<AtomicPacket> {
        // In a full implementation, this would:
        // 1. Build the pre-trade transaction (buy collateral)
        // 2. Clone the target transaction
        // 3. Build the post-trade transaction (sell collateral)
        //
        // For now, return placeholder bytes.
        let pre_trade = Bytes::from(vec![0x01; 64]);
        let target = Bytes::from(vec![0x02; 64]);
        let post_trade = Bytes::from(vec![0x03; 64]);

        let max_block = self.current_block + self.config.max_wait_blocks;
        let max_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() + 120);

        Some(AtomicPacket {
            pre_trade_tx: pre_trade,
            target_tx: target,
            post_trade_tx: post_trade,
            execution_window: ExecutionWindow {
                min_block: self.current_block,
                max_block,
                min_timestamp: None,
                max_timestamp,
            },
        })
    }

    /// Simulate submission in shadow mode.
    fn simulate_submission(
        &self,
        _packet: &AtomicPacket,
        scored: &ScoredTarget,
    ) -> Option<TrancheResult> {
        // Generate a deterministic fake bundle hash for simulation
        let bundle_hash = B256::from_slice(&[0xAB; 32]);

        Some(TrancheResult {
            bundle_hash,
            target_tx_hash: scored.target.tx_hash,
            pre_trade_tx_hash: Some(B256::from_slice(&[0x01; 32])),
            post_trade_tx_hash: Some(B256::from_slice(&[0x03; 32])),
            block_number: self.current_block,
            profit_wei: scored.analysis.net_profit_wei,
            gas_spent_wei: scored.analysis.leg_gas_cost_wei,
            status: TrancheStatus::Confirmed,
        })
    }

    /// Build a result from a successful submission.
    fn build_result(&self, bundle_hash: B256, scored: &ScoredTarget) -> Option<TrancheResult> {
        Some(TrancheResult {
            bundle_hash,
            target_tx_hash: scored.target.tx_hash,
            pre_trade_tx_hash: None,
            post_trade_tx_hash: None,
            block_number: self.current_block,
            profit_wei: scored.analysis.net_profit_wei,
            gas_spent_wei: scored.analysis.leg_gas_cost_wei,
            status: TrancheStatus::Pending,
        })
    }

    /// Get current orchestrator state.
    pub fn state(&self) -> &OrchestratorState {
        &self.state
    }

    /// Update current block number.
    pub fn update_block(&mut self, block: u64, base_fee: u128) {
        self.current_block = block;
        self.base_fee_wei = base_fee;
    }

    /// Calculate optimal priority fee for current conditions.
    pub fn calculate_priority_fee(&self) -> u64 {
        let base_fee_gwei = (self.base_fee_wei / 1_000_000_000) as u64;
        base_fee_gwei.saturating_mul(3).min(50_000)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mempool_predator::MempoolPredator;
    use crate::tranche_arbitrage::{TrancheBundler, TrancheScanner};
    use alloy::primitives::address;

    fn make_orchestrator() -> TrancheOrchestrator {
        let executor = address!("0x98Fc3F5c95b34BF3197e1349a2932F6177D336Ef");
        let scanner = TrancheScanner::new(executor, 8453);
        let predator = MempoolPredator::new(
            scanner,
            100,
            50_000_000_000u128,
            Decimal::from(10),
            Decimal::from(1800),
            150_000,
            1_000_000_000u128,
        );
        let bundler = TrancheBundler::new(8453, "https://rpc.flashbots.net".into());
        let config = TrancheConfig::default();

        TrancheOrchestrator::new(predator, bundler, config, 1000, 1_000_000_000u128)
    }

    #[test]
    fn test_orchestrator_starts_idle() {
        let orch = make_orchestrator();
        assert_eq!(orch.state(), &OrchestratorState::Idle);
    }

    #[test]
    fn test_calculate_priority_fee() {
        let orch = make_orchestrator();
        let fee = orch.calculate_priority_fee();
        assert_eq!(fee, 3);
    }

    #[test]
    fn test_update_block() {
        let mut orch = make_orchestrator();
        orch.update_block(2000, 2_000_000_000u128);
        assert_eq!(orch.current_block, 2000);
        assert_eq!(orch.base_fee_wei, 2_000_000_000u128);
    }

    #[test]
    fn test_shadow_config_default() {
        let config = TrancheConfig::default();
        assert!(config.shadow_mode);
        assert_eq!(config.priority_fee_multiplier, 3);
        assert_eq!(config.max_wait_blocks, 6);
    }
}
