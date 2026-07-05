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

use crate::{ChimeraError, LiquidationCandidate};
use alloy::primitives::{Address, U256};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::str::FromStr;

/// In-memory snapshot of reserves and user positions (populated by snapshot_generator.py or periodic refresh).
#[derive(Debug, Clone, Default)]
pub struct MarketSnapshot {
    pub reserves: HashMap<Address, ReserveData>,
    pub users: HashMap<Address, UserPosition>,
    pub block_number: u64,
    pub timestamp: chrono::DateTime<Utc>,
    pub chain_id: u64,
}

// ---------------------------------------------------------------------------
// Snapshot hydration (detector-side)
//
// `scripts/snapshot_generator.py` emits the JSON documented in
// `docs/snapshot-schema.md`. The simulator consumes it via
// `simulator::prewarm::MarketSnapshot`; the *detector* needs the same file
// hydrated into THIS `MarketSnapshot` (keyed by parsed `Address`).
//
// Targeted JSON shape (see docs/snapshot-schema.md + core/snapshots/*.json):
// {
//   "chain": "base",                       // string -> chain_id (base=8453, arbitrum=42161)
//   "block_number": 0,
//   "pool": "0x..",                         // present, unused by the detector
//   "timestamp": 1781661993,                // unix seconds
//   "reserves": [                            // ARRAY, addresses are strings
//     { "address": "0x..", "symbol": "WETH", "decimals": 18, "ltv": 8000,
//       "liquidation_threshold": 8250, "liquidation_bonus": 10500,
//       "liquidity_rate": 0, "variable_borrow_rate": 0, "total_variable_debt": 0,
//       "price_usd": 3500.0, "a_token": "0x..", "variable_debt_token": "0x.." }
//   ],
//   "users": {                               // MAP, address string -> position
//     "0x..": { "collateral": {"0x..": "1000.."}, "debt": {"0x..": "2000.."},
//               "emode_category": 0 }
//   }
// }
// ---------------------------------------------------------------------------

/// Private serde mirror of the snapshot JSON. Unknown fields (e.g. rate fields the
/// detector does not need) are ignored. Optional fields default rather than error.
#[derive(Deserialize, Debug, Default)]
struct RawSnapshot {
    #[serde(default)]
    chain: String,
    #[serde(default)]
    block_number: u64,
    #[serde(default)]
    timestamp: i64,
    #[serde(default)]
    reserves: Vec<RawReserve>,
    #[serde(default)]
    users: HashMap<String, RawUser>,
}

/// Default for serde `active` field: existing snapshots have no `active` key but
/// imply an active reserve, so the absence of the field must deserialize to `true`.
fn default_true() -> bool {
    true
}

#[derive(Deserialize, Debug, Default)]
struct RawReserve {
    address: String,
    #[serde(default)]
    decimals: u8,
    #[serde(default)]
    liquidation_threshold: u16,
    #[serde(default)]
    liquidation_bonus: u16,
    #[serde(default)]
    price_usd: f64,
    #[serde(default)]
    a_token: String,
    #[serde(default)]
    variable_debt_token: String,
    // --- Aave V3 edge-case fields. ALL serde-default so existing snapshots,
    //     golden fixtures, and mock data still deserialize (backward compatible). ---
    /// Reserve active flag. Default = true because pre-edge-case snapshots imply active.
    #[serde(default = "default_true")]
    active: bool,
    /// Frozen reserves cannot be seized as collateral. Default false.
    #[serde(default)]
    frozen: bool,
    /// Paused reserves block all actions. Default false.
    #[serde(default)]
    paused: bool,
    /// Liquidation protocol fee in bps (on-chain bits 152-167). Default 0.
    #[serde(default)]
    liquidation_protocol_fee: u16,
    /// Reserve eMode category (0 = none).
    #[serde(default)]
    emode_category: u8,
    /// eMode-category liquidation threshold in bps (0 = none / use base LT).
    #[serde(default)]
    emode_liquidation_threshold: u16,
    /// eMode-category liquidation bonus in bps (0 = none).
    #[serde(default)]
    emode_liquidation_bonus: u16,
    /// Isolation-mode asset flag. Default false.
    #[serde(default)]
    is_isolated: bool,
    /// Isolation debt ceiling as a decimal string ("" / absent => 0).
    #[serde(default)]
    debt_ceiling: String,
    /// Siloed-borrowing flag (asset can only be borrowed alone). Default false.
    #[serde(default)]
    siloed_borrowing: bool,
    // --- Snapshot indices (u128 string contract; see docs/snapshot-schema.md). ---
    /// Liquidity index as a RAY decimal string. "" → 1e27 (default).
    #[serde(default)]
    liquidity_index: String,
    /// Variable borrow index as a RAY decimal string. "" → 1e27 (default).
    #[serde(default)]
    variable_borrow_index: String,
}

#[derive(Deserialize, Debug, Default)]
struct RawUser {
    #[serde(default)]
    collateral: HashMap<String, String>,
    #[serde(default)]
    debt: HashMap<String, String>,
    #[serde(default)]
    emode_category: u8,
    /// Whether the user is in Aave V3 isolation mode. Default false.
    #[serde(default)]
    is_in_isolation: bool,
}

/// Parse an address string, returning `Address::ZERO` for empty/invalid values.
/// Used for optional token fields that may be absent or zero in mock snapshots.
fn parse_addr_or_zero(s: &str) -> Address {
    if s.is_empty() {
        return Address::ZERO;
    }
    Address::from_str(s).unwrap_or(Address::ZERO)
}

/// Parse a `{ address_string -> decimal_amount_string }` map into `Address -> U256`.
fn parse_balance_map(
    raw: HashMap<String, String>,
) -> Result<HashMap<Address, U256>, ChimeraError> {
    let mut out = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        let asset = Address::from_str(&k).map_err(|e| {
            ChimeraError::ConfigError(format!("snapshot bad asset address {k}: {e}"))
        })?;
        let amount = U256::from_str(&v).map_err(|e| {
            ChimeraError::ConfigError(format!("snapshot bad amount {v} for {k}: {e}"))
        })?;
        out.insert(asset, amount);
    }
    Ok(out)
}

/// Parse an optional u128 index string from JSON. "" or absent → RAY (1e27).
fn parse_index_or_ray(s: &str) -> U256 {
    if s.is_empty() {
        // Default RAY value for backward compatibility with older snapshots.
        U256::from(1_000_000_000_000_000_000_000_000_000u128)
    } else {
        U256::from_str(s).unwrap_or(U256::from(1_000_000_000_000_000_000_000_000_000u128))
    }
}

fn chain_name_to_id(chain: &str) -> u64 {
    match chain {
        "base" => 8453,
        "arbitrum" => 42161,
        _ => 8453,
    }
}

impl RawSnapshot {
    fn into_market_snapshot(self) -> Result<MarketSnapshot, ChimeraError> {
            let chain_id = chain_name_to_id(&self.chain);
            // Index fields now sourced from the snapshot JSON; fall back to RAY for
            // pre-edge-case snapshots that lack the keys (backward compatible).

            let mut reserves = HashMap::with_capacity(self.reserves.len());
        for r in self.reserves {
            let addr = Address::from_str(&r.address).map_err(|e| {
                ChimeraError::ConfigError(format!("snapshot bad reserve address {}: {e}", r.address))
            })?;
            // Oracle USD price is 8-decimal fixed point (matches detector convention).
            let price_usd = if r.price_usd.is_finite() && r.price_usd > 0.0 {
                U256::from((r.price_usd * 1e8) as u128)
            } else {
                U256::ZERO
            };
            // Isolation debt ceiling: decimal string, empty/invalid => 0.
            let debt_ceiling = if r.debt_ceiling.is_empty() {
                U256::ZERO
            } else {
                U256::from_str(&r.debt_ceiling).unwrap_or(U256::ZERO)
            };
            reserves.insert(
                addr,
                ReserveData {
                    a_token: parse_addr_or_zero(&r.a_token),
                    variable_debt_token: parse_addr_or_zero(&r.variable_debt_token),
                    liquidity_index: parse_index_or_ray(&r.liquidity_index),
                    variable_borrow_index: parse_index_or_ray(&r.variable_borrow_index),
                    liquidation_bonus_bps: r.liquidation_bonus,
                    liquidation_threshold_bps: r.liquidation_threshold,
                    price_usd,
                    decimals: r.decimals,
                    // Edge case 5 (isolation) + 4 (eMode): hydrated, no longer hardcoded.
                    is_isolated: r.is_isolated,
                    debt_ceiling,
                    e_mode_category: r.emode_category,
                    // Edge case 1 (frozen/paused/inactive) + 2 (protocol fee) + 4/6.
                    active: r.active,
                    frozen: r.frozen,
                    paused: r.paused,
                    liquidation_protocol_fee_bps: r.liquidation_protocol_fee,
                    emode_liquidation_threshold_bps: r.emode_liquidation_threshold,
                    emode_liquidation_bonus_bps: r.emode_liquidation_bonus,
                    siloed_borrowing: r.siloed_borrowing,
                },
            );
        }

        let mut users = HashMap::with_capacity(self.users.len());
        for (k, u) in self.users {
            let user_addr = Address::from_str(&k).map_err(|e| {
                ChimeraError::ConfigError(format!("snapshot bad user address {k}: {e}"))
            })?;
            users.insert(
                user_addr,
                UserPosition {
                    collateral: parse_balance_map(u.collateral)?,
                    debt: parse_balance_map(u.debt)?,
                    emode_category: u.emode_category,
                    // Edge case 5 (isolation): hydrated, no longer hardcoded false.
                    is_in_isolation: u.is_in_isolation,
                },
            );
        }

        let timestamp = DateTime::<Utc>::from_timestamp(self.timestamp, 0).unwrap_or_else(Utc::now);

        Ok(MarketSnapshot {
            reserves,
            users,
            block_number: self.block_number,
            timestamp,
            chain_id,
        })
    }
}

impl MarketSnapshot {
    /// Hydrate a detector [`MarketSnapshot`] from the snapshot JSON produced by
    /// `scripts/snapshot_generator.py`.
    ///
    /// Returns a [`ChimeraError::ConfigError`] on read/parse/conversion failure so
    /// callers can decide whether to fall back to [`MarketSnapshot::default`]
    /// (empty = zero candidates = safe). This function never panics.
    pub fn load_from_file(path: &std::path::Path) -> Result<Self, ChimeraError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| ChimeraError::ConfigError(format!("snapshot read failed: {e}")))?;
        let parsed: RawSnapshot = serde_json::from_str(&raw)
            .map_err(|e| ChimeraError::ConfigError(format!("snapshot parse failed: {e}")))?;
        parsed.into_market_snapshot()
    }
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
    // --- Aave V3 edge-case fields ---
    /// Reserve active flag; inactive reserves cannot be seized as collateral.
    pub active: bool,
    /// Frozen reserves cannot be seized as collateral by a liquidator.
    pub frozen: bool,
    /// Paused reserves block all actions, including liquidation seizure.
    pub paused: bool,
    /// Liquidation protocol fee (bps) taken by the protocol on the bonus portion.
    pub liquidation_protocol_fee_bps: u16,
    /// eMode-category liquidation threshold (bps); 0 = none (use base LT).
    pub emode_liquidation_threshold_bps: u16,
    /// eMode-category liquidation bonus (bps); 0 = none.
    pub emode_liquidation_bonus_bps: u16,
    /// Siloed-borrowing flag; a siloed debt asset can only be borrowed alone.
    pub siloed_borrowing: bool,
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
                // Edge case 3 (bad debt): if the seizable collateral value is below the
                // debt value, the position cannot be profitably liquidated by a searcher.
                // Aave V3 still permits bad-debt liquidation that writes off the deficit,
                // but it is a guaranteed loss for us, so we do not emit it.
                // NOTE: `total_collateral` here is post-exclusion (frozen/paused/inactive
                // and non-isolated collateral already removed in calculate_user_account_data).
                if total_collateral < total_debt {
                    tracing::debug!(
                        target: "chimera::detector",
                        user = %user,
                        collateral_usd = %total_collateral,
                        debt_usd = %total_debt,
                        "Skipping bad-debt position (seizable collateral < debt)"
                    );
                    continue;
                }

                // Apply close factor to determine debt_to_cover (Aave GenericLogic).
                let debt_to_cover =
                    self.apply_close_factor(total_collateral, total_debt, hf, &position.debt);

                // Edge case 1 (frozen/paused/inactive): only seizable collateral can be the
                // liquidation target. `select_best_collateral` filters to seizable reserves;
                // a candidate whose only collateral is non-seizable is NOT emitted.
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
                            // Emitted candidates are never bad debt (filtered above).
                            bad_debt: false,
                        };
                        candidates.push(candidate);
                    }
                } else {
                    tracing::debug!(
                        target: "chimera::detector",
                        user = %user,
                        "At-risk user has no seizable collateral; skipping"
                    );
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
            // Edge case 1: only seizable reserves (active && !frozen && !paused) are
            // eligible liquidation targets. Non-seizable / unknown reserves are skipped.
            .filter(|(&asset, _)| {
                self.snapshot
                    .reserves
                    .get(&asset)
                    .map(is_seizable_collateral)
                    .unwrap_or(false)
            })
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
        user: &Address,
        position: &UserPosition,
    ) -> (U256, U256, U256, U256) {
        let _ = user; // retained for symmetry/logging; not used in the math itself
        let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128); // 1e27
        const USD_DECIMALS_SHIFT: u64 = 10u64.pow(8); // oracle prices are 8-decimal

        let mut total_collateral_usd: U256 = U256::ZERO; // RAY scaled collateral value
        let mut total_debt_usd: U256 = U256::ZERO; // RAY scaled debt value
        let mut weighted_liquidation_threshold: U256 = U256::ZERO;

        // Collateral loop
        for (asset, scaled_balance) in &position.collateral {
            if let Some(reserve) = self.snapshot.reserves.get(asset) {
                // Edge case 1 (frozen/paused/inactive): non-seizable collateral cannot be
                // taken in a liquidation, so it does not back the position from a
                // liquidator's perspective. Excluding it lowers HF (conservative) and
                // removes it from the bad-debt / seizure calculations.
                if !is_seizable_collateral(reserve) {
                    tracing::debug!(
                        target: "chimera::detector",
                        asset = %asset,
                        active = reserve.active,
                        frozen = reserve.frozen,
                        paused = reserve.paused,
                        "Excluding non-seizable collateral from account data"
                    );
                    continue;
                }

                // Edge case 5 (isolation mode): an isolated user's borrowing power derives
                // only from the isolated asset. Counting non-isolated collateral would
                // OVERSTATE the position; exclude it (conservative, correct direction).
                // Deferred: precise debt-ceiling enforcement on the borrow side and the
                // single-isolated-collateral invariant are handled by the REVM simulator.
                if position.is_in_isolation && !reserve.is_isolated {
                    tracing::debug!(
                        target: "chimera::detector",
                        asset = %asset,
                        "Skipping non-isolated collateral for isolation-mode user"
                    );
                    continue;
                }

                // Convert RAY-scaled balance to USD:
                // balance_ray * liquidity_index_ray / RAY / RAY * price_usd_8dec / USD_DECIMALS_SHIFT
                // = (scaled_balance * liquidity_index) / RAY * price / USD_DECIMALS_SHIFT
                let balance_usd = *scaled_balance * reserve.liquidity_index / ray
                    * reserve.price_usd
                    / U256::from(USD_DECIMALS_SHIFT);

                total_collateral_usd += balance_usd;

                // Liquidation threshold contribution: collateral_value * (lt_bps / 10000).
                // Edge case 4 (eMode): when the user is in an eMode category that matches
                // this reserve's category and the reserve defines an eMode LT, use the
                // (higher) eMode liquidation threshold instead of the base LT. eMode raises
                // LT, which raises HF and makes the position safer; correct HF math here is
                // essential to avoid false-positive liquidation candidates.
                let lt_bps = if position.emode_category != 0
                    && position.emode_category == reserve.e_mode_category
                    && reserve.emode_liquidation_threshold_bps != 0
                {
                    U256::from(reserve.emode_liquidation_threshold_bps)
                } else {
                    U256::from(reserve.liquidation_threshold_bps)
                };
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
            weighted_liquidation_threshold * ray / total_collateral_usd
        } else {
            U256::ZERO
        };

        // Aave V3 HF = (Σ collateral_i × lt_i) / totalDebt, expressed in RAY.
        // weighted_liquidation_threshold already contains Σ(collateral_usd_i × lt_bps_i / 10000).
        // Multiply by ray to preserve precision in the RAY-scaled result.
        let hf = if !total_debt_usd.is_zero() {
            weighted_liquidation_threshold * ray / total_debt_usd
        } else {
            U256::MAX
        };

        (
            total_collateral_usd,
            total_debt_usd,
            avg_liquidation_threshold,
            hf,
        )
    }
}

/// Edge case 1: a reserve's collateral can be seized by a liquidator only when it is
/// active, not frozen, and not paused. Aave V3 blocks `liquidationCall` collateral
/// seizure for inactive/paused reserves; a frozen reserve is excluded conservatively
/// (its collateral usage cannot be (re)enabled), so we never select it as a target and
/// never count it toward the position's seizable value.
fn is_seizable_collateral(reserve: &ReserveData) -> bool {
    reserve.active && !reserve.frozen && !reserve.paused
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
    fn test_load_from_file_hydrates_detector_snapshot() {
        let json = r#"{
          "chain": "base",
          "block_number": 42,
          "pool": "0x0000000000000000000000000000000000000000",
          "timestamp": 1781661993,
          "reserves": [
            {
              "address": "0x4200000000000000000000000000000000000006",
              "symbol": "WETH",
              "decimals": 18,
              "ltv": 8000,
              "liquidation_threshold": 8250,
              "liquidation_bonus": 10500,
              "liquidity_rate": 0,
              "variable_borrow_rate": 0,
              "total_variable_debt": 0,
              "price_usd": 3500.0,
              "a_token": "0x0000000000000000000000000000000000000000",
              "variable_debt_token": "0x0000000000000000000000000000000000000000"
            }
          ],
          "users": {
            "0x00000000000000000000000000000000000d3b7a": {
              "collateral": {"0x4200000000000000000000000000000000000006": "1000000000000000000"},
              "debt": {"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913": "2000000000"},
              "emode_category": 0
            }
          }
        }"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        std::fs::write(&path, json).unwrap();

        let snap = MarketSnapshot::load_from_file(&path).expect("snapshot must hydrate");
        assert_eq!(snap.chain_id, 8453);
        assert_eq!(snap.block_number, 42);
        assert_eq!(snap.reserves.len(), 1);
        assert_eq!(snap.users.len(), 1);

        let weth = Address::from_str("0x4200000000000000000000000000000000000006").unwrap();
        let reserve = snap.reserves.get(&weth).expect("WETH reserve present");
        assert_eq!(reserve.liquidation_bonus_bps, 10500);
        // 3500.0 USD * 1e8 (8-decimal oracle convention)
        assert_eq!(reserve.price_usd, U256::from(350_000_000_000u128));
    }

    #[test]
    fn test_load_from_file_missing_is_error_not_panic() {
        let path = std::path::Path::new("definitely/does/not/exist/snapshot.json");
        assert!(MarketSnapshot::load_from_file(path).is_err());
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
