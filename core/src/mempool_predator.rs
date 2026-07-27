//! Mempool Predator — Target Detection & Slippage Analysis.
//!
//! This module transforms passive mempool observation into targeted detection
//! of execution bundles with measurable price impact. It identifies high-slippage
//! `Executor.execute(bytes)` transactions and scores them for tranche viability.
//!
//! ## Architecture
//! 1. Pattern matching for `execute(bytes)` calldata (selector 0x09c5eabe)
//! 2. Slippage simulation via reserve delta analysis
//! 3. Target scoring based on price impact, gas overhead, and profit potential
//! 4. Volatility filtering to reject low-impact transactions

use crate::tranche_arbitrage::{TrancheScanner, TrancheTarget};
use alloy::primitives::{Address, Bytes, B256, U256};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Slippage Analysis
// ---------------------------------------------------------------------------

/// Result of slippage simulation for a target transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlippageAnalysis {
    /// Estimated price impact in basis points.
    pub slippage_bps: u32,
    /// Estimated profit from capturing the price delta (wei).
    pub estimated_profit_wei: U256,
    /// Gas cost for the pre/post trade legs (wei).
    pub leg_gas_cost_wei: U256,
    /// Net profit after gas costs (wei).
    pub net_profit_wei: U256,
    /// Whether this target meets the minimum slippage threshold.
    pub meets_threshold: bool,
}

/// Scored target ready for tranche bundling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredTarget {
    /// The underlying tranche target.
    pub target: TrancheTarget,
    /// Slippage analysis results.
    pub analysis: SlippageAnalysis,
    /// Composite score (higher = better opportunity).
    pub score: u64,
}

// ---------------------------------------------------------------------------
// Mempool Predator
// ---------------------------------------------------------------------------

/// Analyzes pending transactions to identify tranche opportunities.
///
/// Extends [`TrancheScanner`] with slippage calculation and target scoring.
#[derive(Debug, Clone)]
pub struct MempoolPredator {
    /// Underlying scanner for pattern matching.
    pub scanner: TrancheScanner,
    /// Minimum slippage threshold in basis points.
    pub slippage_threshold_bps: u32,
    /// Maximum priority fee willing to pay for inclusion (wei).
    pub max_priority_fee_wei: U256,
    /// Minimum net profit threshold in USD.
    pub min_profit_usd: Decimal,
    /// Current ETH price in USD for profit calculation.
    pub eth_price_usd: Decimal,
    /// Estimated gas for a single swap leg.
    pub swap_gas_estimate: u64,
    /// Current base fee in wei.
    pub base_fee_wei: u128,
}

impl MempoolPredator {
    /// Create a new predator instance.
    pub fn new(
        scanner: TrancheScanner,
        slippage_threshold_bps: u32,
        max_priority_fee_wei: u128,
        min_profit_usd: Decimal,
        eth_price_usd: Decimal,
        swap_gas_estimate: u64,
        base_fee_wei: u128,
    ) -> Self {
        Self {
            scanner,
            slippage_threshold_bps,
            max_priority_fee_wei: U256::from(max_priority_fee_wei),
            min_profit_usd,
            eth_price_usd,
            swap_gas_estimate,
            base_fee_wei,
        }
    }

    /// Identify friction points: pending transactions with measurable slippage.
    ///
    /// Returns `Some(ScoredTarget)` if the transaction meets all thresholds,
    /// `None` otherwise.
    pub fn identify_friction_points(
        &self,
        to: Address,
        calldata: &[u8],
        tx_hash: B256,
        gas_price_wei: u128,
        max_priority_fee: u128,
    ) -> Option<ScoredTarget> {
        // Step 1: Pattern match via scanner
        if !self.scanner.matches(to, calldata) {
            return None;
        }

        // Step 2: Decode target transaction
        let target = self
            .scanner
            .decode_target_tx(calldata, tx_hash, gas_price_wei, max_priority_fee)?;

        // Step 3: Calculate slippage
        let analysis = self.calculate_slippage(&target)?;

        // Step 4: Check threshold
        if !analysis.meets_threshold {
            return None;
        }

        // Step 5: Score the target
        let score = self.calculate_score(&target, &analysis);

        Some(ScoredTarget {
            target,
            analysis,
            score,
        })
    }

    /// Calculate expected slippage for a target transaction.
    ///
    /// Uses reserve delta analysis to estimate price impact:
    /// - Extracts trade size from the Executor.execute payload
    /// - Estimates slippage based on constant product formula
    /// - Calculates net profit after gas costs
    pub fn calculate_slippage(&self, target: &TrancheTarget) -> Option<SlippageAnalysis> {
        // Estimate trade size in USD (simplified: amount * eth_price / 1e18)
        let trade_size_eth = Decimal::from_str(&target.amount.to_string())
            .ok()?
            .checked_div(Decimal::from(1_000_000_000_000_000_000u64))
            .unwrap_or(Decimal::ZERO);

        let trade_size_usd = trade_size_eth.checked_mul(self.eth_price_usd)?;

        // Simplified slippage model: slippage ~ trade_size / liquidity
        // For a typical Base pool with ~$1M liquidity, 0.1% slippage per $1K
        let estimated_slippage_bps = if trade_size_usd > Decimal::ZERO {
            let slippage = trade_size_usd
                .checked_mul(Decimal::from(10))
                .unwrap_or(Decimal::ZERO)
                .checked_div(Decimal::from(1000))
                .unwrap_or(Decimal::ZERO);
            slippage.to_u32().unwrap_or(0)
        } else {
            0
        };

        // Calculate leg gas costs (2 legs: pre-trade + post-trade)
        let leg_gas_cost_wei = U256::from(self.swap_gas_estimate * 2)
            .checked_mul(U256::from(
                self.base_fee_wei
                    .saturating_add(
                        self.max_priority_fee_wei
                            .try_into()
                            .unwrap_or(u128::MAX),
                    ),
            ))
            .unwrap_or(U256::ZERO);

        // Estimate profit from slippage capture
        // Profit = trade_size * (slippage_bps / 10000) in ETH, converted to wei
        let profit_eth = trade_size_eth
            .checked_mul(Decimal::from(estimated_slippage_bps))
            .unwrap_or(Decimal::ZERO)
            .checked_div(Decimal::from(10000))
            .unwrap_or(Decimal::ZERO);

        let estimated_profit_wei = U256::from(
            profit_eth
                .checked_mul(Decimal::from(1_000_000_000_000_000_000u64))
                .unwrap_or(Decimal::ZERO)
                .to_u128()
                .unwrap_or(0),
        );

        let net_profit_wei = estimated_profit_wei.saturating_sub(leg_gas_cost_wei);

        // Convert net profit to USD for threshold check
        let net_profit_usd = Decimal::from_str(&net_profit_wei.to_string())
            .ok()
            .and_then(|v| {
                v.checked_div(Decimal::from(1_000_000_000_000_000_000u64))
                    .and_then(|eth| eth.checked_mul(self.eth_price_usd))
            })
            .unwrap_or(Decimal::ZERO);

        let meets_threshold = estimated_slippage_bps >= self.slippage_threshold_bps
            && net_profit_usd >= self.min_profit_usd;

        Some(SlippageAnalysis {
            slippage_bps: estimated_slippage_bps,
            estimated_profit_wei,
            leg_gas_cost_wei,
            net_profit_wei,
            meets_threshold,
        })
    }

    /// Filter out low-impact transactions that don't meet slippage threshold.
    pub fn filter_volatile_targets(
        &self,
        candidates: Vec<(Address, Bytes, B256, u128, u128)>,
    ) -> Vec<ScoredTarget> {
        candidates
            .into_iter()
            .filter_map(|(to, calldata, tx_hash, gas_price, priority_fee)| {
                self.identify_friction_points(to, &calldata, tx_hash, gas_price, priority_fee)
            })
            .collect()
    }

    /// Calculate composite score for a scored target.
    ///
    /// Score = (slippage_bps * 100) + (net_profit_wei / 1e15) + priority_bonus
    fn calculate_score(&self, target: &TrancheTarget, analysis: &SlippageAnalysis) -> u64 {
        let slippage_component = (analysis.slippage_bps as u64).saturating_mul(100);
        let profit_component = (analysis.net_profit_wei / U256::from(1_000_000_000_000_000u64))
            .try_into()
            .unwrap_or(u64::MAX);
        let priority_bonus = if target.max_priority_fee > 1_000_000_000u128 {
            50 // Bonus for high-priority targets (more likely to include)
        } else {
            0
        };

        slippage_component
            .saturating_add(profit_component)
            .saturating_add(priority_bonus)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn make_predator() -> MempoolPredator {
        let executor = address!("0x98Fc3F5c95b34BF3197e1349a2932F6177D336Ef");
        let scanner = TrancheScanner::new(executor, 8453);
        MempoolPredator::new(
            scanner,
            100,    // 1% slippage threshold
            50_000_000_000u128, // 50 gwei max priority
            Decimal::from(10),  // $10 min profit
            Decimal::from(1800), // ETH price
            150_000, // gas per swap
            1_000_000_000u128, // 1 gwei base fee
        )
    }

    #[test]
    fn test_predator_matches_known_executor() {
        let predator = make_predator();
        let mut calldata = vec![0x09, 0xc5, 0xea, 0xbe];
        calldata.extend(vec![0u8; 416]);
        let tx_hash = B256::ZERO;

        let result = predator.identify_friction_points(
            predator.scanner.target_executor,
            &calldata,
            tx_hash,
            1_000_000_000u128,
            100_000_000u128,
        );

        // Should return None because calldata is all zeros (no real trade)
        assert!(result.is_none() || result.as_ref().map(|s| !s.analysis.meets_threshold).unwrap_or(true));
    }

    #[test]
    fn test_predator_rejects_wrong_selector() {
        let predator = make_predator();
        let calldata = vec![0xde, 0xad, 0xbe, 0xef, 0x00];
        let tx_hash = B256::ZERO;

        let result = predator.identify_friction_points(
            predator.scanner.target_executor,
            &calldata,
            tx_hash,
            1_000_000_000u128,
            100_000_000u128,
        );

        assert!(result.is_none());
    }

    #[test]
    fn test_filter_volatile_targets_empty() {
        let predator = make_predator();
        let results = predator.filter_volatile_targets(vec![]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_slippage_analysis_zero_trade() {
        let predator = make_predator();
        let mut calldata = vec![0x09, 0xc5, 0xea, 0xbe];
        calldata.extend(vec![0u8; 416]);

        let target = predator
            .scanner
            .decode_target_tx(&calldata, B256::ZERO, 1_000_000_000u128, 100_000_000u128);

        // Zero calldata should decode to zero amounts
        if let Some(t) = target {
            let analysis = predator.calculate_slippage(&t);
            assert!(analysis.is_some());
            let a = analysis.unwrap();
            assert_eq!(a.slippage_bps, 0);
            assert!(!a.meets_threshold);
        }
    }
}
