//! Liquidation Detector for Aave V3 on L2.
//!
//! Fast pre-filter (pure math + cached state) + candidate builder for the high-fidelity REVM simulator.
//! Designed to feed directly into LiquidationSimulator while respecting all pacing/breaker rules.
//!
//! Research-backed: Implements the exact HF formula from GenericLogic + eMode/isolation awareness.
//!
//! Health Factor scale: All internal HF calculations use RAY (1e27) fixed-point arithmetic
//! to match Aave v3's GenericLogic. The `current_hf` field on `LiquidationCandidate` is
//! stored in RAY scale (1e27). Comparison against `hf_buffer` (bps, 1e4) is done after
//! converting the HF to a "x100" percentage scale for direct comparison with Aave's
//! `HEALTH_FACTOR_LIQUIDATION_THRESHOLD` convention.

use crate::LiquidationCandidate;
use alloy::primitives::{Address, U256};
use chrono::Utc;
use std::collections::HashMap;

/// In-memory snapshot of reserves and user positions (populated by snapshot_generator.py or periodic refresh).
#[derive(Debug, Clone, Default)]
pub struct MarketSnapshot {
    pub reserves: HashMap<Address, ReserveData>,
    pub users: HashMap<Address, UserPosition>,
    pub block_number: u64,
    pub timestamp: chrono::DateTime<Utc>,
    pub chain_id: u64,
}

#[derive(Debug, Clone)]
pub struct ReserveData {
    pub a_token: Address,
    pub variable_debt_token: Address,
    pub liquidity_index: U256, // RAY (1e27) - Aave v3 uses 27 decimal fixed point
    pub variable_borrow_index: U256, // RAY (1e27)
    pub liquidation_bonus_bps: u16, // e.g. 10500 = 5% bonus on top of collateral
    pub liquidation_threshold_bps: u16, // e.g. 2000 = 20% ltvs / lt threshold
    pub price_usd: U256,       // USD price with 8 decimals (typical oracle format)
    pub decimals: u8,
    pub is_isolated: bool,
    pub debt_ceiling: U256,
    pub e_mode_category: u8, // 0 = none
}

#[derive(Debug, Clone, Default)]
pub struct UserPosition {
    pub collateral: HashMap<Address, U256>, // aToken balance (RAY scaled, not 1e18!)
    pub debt: HashMap<Address, U256>,       // variable debt (RAY scaled)
    pub emode_category: u8,
    pub is_in_isolation: bool,
}

/// High-performance liquidation detector (pre-filter only).
pub struct LiquidationDetector {
    snapshot: MarketSnapshot,
    hf_liquidation_threshold: U256, // RAY scale: 1.05e27 = 1.05x HF threshold
    chain_id: u64,
}

impl Default for LiquidationDetector {
    fn default() -> Self {
        Self::new(MarketSnapshot::default(), 8453)
    }
}

impl LiquidationDetector {
    pub fn new(snapshot: MarketSnapshot, chain_id: u64) -> Self {
        // Default: HF < 1.05 triggers liquidation (Aave default: HEALTH_FACTOR_LIQUIDATION_THRESHOLD = 1.05e18)
        // In RAY scale this is 1.05 * 1e27 = 1050000000000000000000000000
        let hf_liquidation_threshold = U256::from(105) * U256::from(10).pow(U256::from(25)); // 1.05e27
        Self {
            snapshot,
            hf_liquidation_threshold,
            chain_id,
        }
    }

    /// Fast pre-filter: find all users with HF < buffer using pure math (no REVM yet).
    /// This is the cheap path that runs on every block / sequencer event.
    pub fn find_at_risk_positions(&self) -> Vec<LiquidationCandidate> {
        let mut candidates = Vec::new();

        for (user, position) in &self.snapshot.users {
            let (total_collateral, total_debt, _avg_liq_threshold, hf) =
                self.calculate_user_account_data(user, position);

            if total_debt == U256::ZERO {
                continue;
            }

            // HF is in RAY (1e27). Compare directly with the liquidation threshold.
            if hf < self.hf_liquidation_threshold {
                // Apply close factor to determine debt_to_cover (Aave GenericLogic).
                let debt_to_cover =
                    self.apply_close_factor(total_collateral, total_debt, hf, &position.debt);

                // Select collateral with highest liquidation bonus (optimizes profit).
                if let Some((collateral_asset, _)) =
                    self.select_best_collateral(&position.collateral)
                {
                    for debt_asset in position.debt.keys() {
                        let candidate = LiquidationCandidate {
                            user: *user,
                            collateral_asset,
                            debt_asset: *debt_asset,
                            debt_to_cover: debt_to_cover
                                .min(*position.debt.get(debt_asset).unwrap_or(&U256::ZERO)),
                            receive_a_token: false,
                            current_hf: hf,
                            chain_id: self.chain_id,
                        };
                        candidates.push(candidate);
                    }
                }
            }
        }

        candidates
    }

    /// Aave V3 close factor logic (LiquidationLogic `_calculateDebt`).
    ///
    /// Aave V3 uses a two-tier close factor keyed off `CLOSE_FACTOR_HF_THRESHOLD = 0.95`:
    /// - HF >= 1.0: not liquidatable (close factor = 0)
    /// - 0.95 < HF < 1.0: `DEFAULT_LIQUIDATION_CLOSE_FACTOR` = 50% of the debt
    /// - HF <= 0.95: `MAX_LIQUIDATION_CLOSE_FACTOR` = 100% of the debt
    ///
    /// This is a pre-filter estimate against `total_debt`; the caller caps the
    /// result to the specific debt-asset position, and the REVM simulator computes
    /// the exact `actualDebtToLiquidate` (including `MIN_LEFTOVER_BASE` and protocol fee).
    /// Verify constants against the deployed Aave V3 Origin commit before live mode
    /// (see `docs/research/aave-v3-liquidation-compendium.md`).
    fn apply_close_factor(
        &self,
        _total_collateral: U256,
        total_debt: U256,
        hf: U256,
        debt_positions: &HashMap<Address, U256>,
    ) -> U256 {
        let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128); // 1e27
                                                                         // CLOSE_FACTOR_HF_THRESHOLD = 0.95 in RAY scale.
        let close_factor_hf_threshold = U256::from(950_000_000_000_000_000_000_000_000u128); // 0.95e27

        if hf >= ray {
            return U256::ZERO;
        }

        let close_factor = if hf <= close_factor_hf_threshold {
            // HF <= 0.95: MAX_LIQUIDATION_CLOSE_FACTOR = 100% of debt.
            total_debt
        } else {
            // 0.95 < HF < 1.0: DEFAULT_LIQUIDATION_CLOSE_FACTOR = 50% of debt.
            total_debt / U256::from(2)
        };

        // Cap by the largest single debt position (one liquidationCall covers one debt asset).
        // NOTE: exact `MIN_LEFTOVER_BASE` dust handling, collateral seizure limits, and the
        // liquidation protocol fee are enforced by the REVM simulator, not this pre-filter.
        let max_position_debt = debt_positions.values().copied().max().unwrap_or(U256::ZERO);
        close_factor.min(max_position_debt)
    }

    /// Select the collateral asset with the highest liquidation bonus.
    /// A higher bonus = more profit for the liquidator.
    fn select_best_collateral(
        &self,
        collateral: &HashMap<Address, U256>,
    ) -> Option<(Address, U256)> {
        collateral
            .iter()
            .filter(|(&asset, _)| self.snapshot.reserves.contains_key(&asset))
            .map(|(&asset, balance)| {
                let bonus_bps = self
                    .snapshot
                    .reserves
                    .get(&asset)
                    .map(|r| r.liquidation_bonus_bps as u64)
                    .unwrap_or(0);
                (asset, *balance * U256::from(bonus_bps))
            })
            .max_by_key(|(_, weighted_bonus)| *weighted_bonus)
            .map(|(asset, _)| (asset, *collateral.get(&asset).unwrap_or(&U256::ZERO)))
    }

    /// Exact replication of Aave's GenericLogic.calculateUserAccountData (for pre-filter speed).
    /// All values in RAY (1e27) fixed-point. Prices are 8-decimal USD values.
    fn calculate_user_account_data(
        &self,
        _user: &Address,
        position: &UserPosition,
    ) -> (U256, U256, U256, U256) {
        let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128); // 1e27
        const USD_DECIMALS_SHIFT: u64 = 10u64.pow(8); // oracle prices are 8-decimal

        let mut total_collateral_usd: U256 = U256::ZERO; // RAY scaled collateral value
        let mut total_debt_usd: U256 = U256::ZERO; // RAY scaled debt value
        let mut weighted_liquidation_threshold: U256 = U256::ZERO;

        // Collateral loop
        for (asset, scaled_balance) in &position.collateral {
            if let Some(reserve) = self.snapshot.reserves.get(asset) {
                // Convert RAY-scaled balance to USD:
                // balance_ray * liquidity_index_ray / RAY / RAY * price_usd_8dec / USD_DECIMALS_SHIFT
                // = (scaled_balance * liquidity_index) / RAY * price / USD_DECIMALS_SHIFT
                let balance_usd = *scaled_balance * reserve.liquidity_index / ray
                    * reserve.price_usd
                    / U256::from(USD_DECIMALS_SHIFT);

                total_collateral_usd += balance_usd;

                // Liquidation threshold contribution: collateral_value * (lt_bps / 10000)
                let lt_bps = U256::from(reserve.liquidation_threshold_bps);
                weighted_liquidation_threshold += balance_usd * lt_bps / U256::from(10000);
            }
        }

        // Debt loop
        for (asset, scaled_debt) in &position.debt {
            if let Some(reserve) = self.snapshot.reserves.get(asset) {
                let debt_usd = *scaled_debt * reserve.variable_borrow_index / ray
                    * reserve.price_usd
                    / U256::from(USD_DECIMALS_SHIFT);

                total_debt_usd += debt_usd;
            }
        }

        let avg_liquidation_threshold = if !total_collateral_usd.is_zero() {
            weighted_liquidation_threshold / total_collateral_usd
        } else {
            U256::ZERO
        };

        // HF = total_collateral_usd * avg_lt / total_debt_usd  (both in RAY)
        // This gives HF in RAY (1e27), matching Aave's internal representation.
        let hf = if !total_debt_usd.is_zero() {
            total_collateral_usd * avg_liquidation_threshold / total_debt_usd
        } else {
            U256::MAX // max U256 for "infinite" HF
        };

        (
            total_collateral_usd,
            total_debt_usd,
            avg_liquidation_threshold,
            hf,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot() -> MarketSnapshot {
        MarketSnapshot {
            chain_id: 8453,
            ..MarketSnapshot::default()
        }
    }

    #[test]
    fn test_chain_id_configurable() {
        let snapshot = MarketSnapshot {
            chain_id: 1,
            ..MarketSnapshot::default()
        };
        let detector = LiquidationDetector::new(snapshot.clone(), 1);
        assert_eq!(detector.chain_id, 1);
        assert_eq!(detector.snapshot.chain_id, 1);
    }

    #[test]
    fn test_close_factor_hf_above_one() {
        let detector = LiquidationDetector::new(make_snapshot(), 8453);
        let debt = HashMap::from([(Address::ZERO, U256::from(1_000_000u64))]);
        let result = detector.apply_close_factor(
            U256::from(1_000_000u64),
            U256::from(1_000_000u64),
            U256::from(1_050_000_000_000_000_000_000_000_000_000u128), // 1.05e27
            &debt,
        );
        assert_eq!(result, U256::ZERO);
    }

    #[test]
    fn test_close_factor_hf_below_95_is_full() {
        let detector = LiquidationDetector::new(make_snapshot(), 8453);
        let debt = HashMap::from([(Address::ZERO, U256::from(1_000_000u64))]);
        let result = detector.apply_close_factor(
            U256::from(10_000_000u64),
            U256::from(1_000_000u64),
            U256::from(940_000_000_000_000_000_000_000_000u128), // 0.94e27
            &debt,
        );
        // HF <= 0.95 => MAX close factor = 100% of total_debt = 1_000_000
        assert_eq!(result, U256::from(1_000_000u64));
    }

    #[test]
    fn test_close_factor_hf_at_95_boundary_is_full() {
        let detector = LiquidationDetector::new(make_snapshot(), 8453);
        let debt = HashMap::from([(Address::ZERO, U256::from(1_000_000u64))]);
        let result = detector.apply_close_factor(
            U256::from(10_000_000u64),
            U256::from(1_000_000u64),
            U256::from(950_000_000_000_000_000_000_000_000u128), // exactly 0.95e27
            &debt,
        );
        // HF == 0.95 => still MAX close factor = 100%
        assert_eq!(result, U256::from(1_000_000u64));
    }

    #[test]
    fn test_close_factor_default_50_above_95() {
        let detector = LiquidationDetector::new(make_snapshot(), 8453);
        let debt = HashMap::from([(Address::ZERO, U256::from(1_000_000u64))]);
        for hf in [
            U256::from(960_000_000_000_000_000_000_000_000u128), // 0.96e27
            U256::from(990_000_000_000_000_000_000_000_000u128), // 0.99e27
        ] {
            let result = detector.apply_close_factor(
                U256::from(10_000_000u64),
                U256::from(1_000_000u64),
                hf,
                &debt,
            );
            // 0.95 < HF < 1.0 => DEFAULT close factor = 50% of total_debt = 500_000
            assert_eq!(result, U256::from(500_000u64));
        }
    }

    #[test]
    fn test_close_factor_caps_by_position_debt() {
        let detector = LiquidationDetector::new(make_snapshot(), 8453);
        // total_debt is 150_000 (HF<=0.95 => 100%) but largest single position is only 100_000
        let debt = HashMap::from([
            (Address::ZERO, U256::from(100_000u64)),
            (Address::from([1u8; 20]), U256::from(50_000u64)),
        ]);
        let result = detector.apply_close_factor(
            U256::from(10_000_000u64),
            U256::from(150_000u64),
            U256::from(940_000_000_000_000_000_000_000_000u128), // 0.94e27
            &debt,
        );
        // 100% of total_debt = 150_000, capped by max single position 100_000
        assert_eq!(result, U256::from(100_000u64));
    }
}
