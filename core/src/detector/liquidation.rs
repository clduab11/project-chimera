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
fn parse_balance_map(raw: HashMap<String, String>) -> Result<HashMap<Address, U256>, ChimeraError> {
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

/// Aave's RAY (1e27), the fixed-point scale for indices and health factors.
///
/// Single definition on purpose: this constant divides collateral and debt in
/// every profitability path, so independent copies that drift apart would
/// mis-scale liquidations rather than fail loudly.
fn ray() -> U256 {
    U256::from(1_000_000_000_000_000_000_000_000_000u128)
}

/// Parse an optional u128 index string from JSON. "" or absent → RAY (1e27).
fn parse_index_or_ray(s: &str) -> U256 {
    if s.is_empty() {
        // Default RAY value for backward compatibility with older snapshots.
        ray()
    } else {
        U256::from_str(s).unwrap_or_else(|_| ray())
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
                ChimeraError::ConfigError(format!(
                    "snapshot bad reserve address {}: {e}",
                    r.address
                ))
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

#[derive(Debug, Clone, Default)]
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
    /// RAY scale. Flag threshold, deliberately just ABOVE Aave's hard 1.0 cutoff
    /// so a position is seen shortly before it becomes liquidatable.
    hf_liquidation_threshold: U256,
    chain_id: u64,
}

impl Default for LiquidationDetector {
    fn default() -> Self {
        Self::new(MarketSnapshot::default(), 8453)
    }
}

impl LiquidationDetector {
    pub fn new(snapshot: MarketSnapshot, chain_id: u64) -> Self {
        // Aave's `HEALTH_FACTOR_LIQUIDATION_THRESHOLD` is **1.0e18**, not 1.05e18 —
        // `validateLiquidationCall` refuses anything at or above 1.0 with
        // `HealthFactorNotBelowThreshold()` (selector 0x930bb771). A 1.05 flag
        // threshold therefore emitted candidates the Pool could never accept, and
        // every one of them burned a full REVM simulation per block, forever.
        //
        // Observed live 2026-07-25: with a 1.05 threshold and a snapshot whose
        // median HF was 1.0550, the dead band between 1.0 and 1.05 produced a
        // continuous stream of 0x930bb771 reverts.
        //
        // 1.01 keeps a thin staleness margin so a position can be seen just
        // *before* it crosses — affordable now that live prices refresh every
        // block (`price_refresh_secs: 2`) instead of every fourth.
        // In RAY scale: 1.01 * 1e27 = 1010000000000000000000000000
        let hf_liquidation_threshold = U256::from(101) * U256::from(10).pow(U256::from(25)); // 1.01e27
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
        let ray = ray();

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

                // Edge case 1 (frozen/paused/inactive): only seizable collateral can be the
                // liquidation target. `select_best_collateral` filters to seizable reserves;
                // a candidate whose only collateral is non-seizable is NOT emitted.
                if let Some((collateral_asset, _)) =
                    self.select_best_collateral(&position.collateral)
                {
                    // Collateral reserve metadata for the simulator's USD profit
                    // accounting (selection already guarantees the reserve exists).
                    let (collateral_decimals, collateral_price_usd) = self
                        .snapshot
                        .reserves
                        .get(&collateral_asset)
                        .map(|r| (r.decimals, r.price_usd))
                        .unwrap_or((18, U256::ZERO));
                    for (debt_asset, scaled_debt) in &position.debt {
                        // Debt reserve metadata for the simulator's USD profit
                        // conversion. A debt asset missing from the reserves map
                        // gets price 0 = "unknown" (sim falls back with a warn).
                        let debt_reserve = self.snapshot.reserves.get(debt_asset);
                        let (debt_decimals, debt_price_usd) = debt_reserve
                            .map(|r| (r.decimals, r.price_usd))
                            .unwrap_or((18, U256::ZERO));

                        // Aave applies the close factor to THIS reserve's debt in the
                        // debt token's own native units. `scaled_debt` is pre-index, so
                        // it must be brought forward by the borrow index first — the same
                        // conversion calculate_user_account_data does before pricing. A
                        // reserve missing from the snapshot has no index; RAY (a no-op)
                        // is the parser's own default for an absent index.
                        let current_debt = match debt_reserve {
                            Some(r) => *scaled_debt * r.variable_borrow_index / ray,
                            None => *scaled_debt,
                        };
                        let debt_to_cover = self.apply_close_factor(hf, current_debt);
                        if debt_to_cover.is_zero() {
                            continue;
                        }
                        let candidate = LiquidationCandidate {
                            user: *user,
                            collateral_asset,
                            debt_asset: *debt_asset,
                            debt_to_cover,
                            receive_a_token: false,
                            current_hf: hf,
                            chain_id: self.chain_id,
                            // Emitted candidates are never bad debt (filtered above).
                            bad_debt: false,
                            debt_decimals,
                            debt_price_usd,
                            collateral_decimals,
                            collateral_price_usd,
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
    /// Aave V3 uses a **linear interpolation** between
    /// `CLOSE_FACTOR_HF_THRESHOLD` (0.95 RAY) and
    /// `HEALTH_FACTOR_LIQUIDATION_THRESHOLD` (1.0 RAY):
    ///
    /// ```
    /// closeFactor = DEFAULT + (MAX - DEFAULT) * (1.0 - HF) / (1.0 - 0.95)
    ///            = 0.5 + 0.5 * (1.0 - HF) / 0.05
    ///            = 0.5 + 10 * (1.0 - HF)          (all in RAY = 1e27)
    /// ```
    ///
    /// - HF >= 1.0: not liquidatable (close factor = 0)
    /// - HF <= 0.95: `MAX_LIQUIDATION_CLOSE_FACTOR` = 100% of the debt
    /// - 0.95 < HF < 1.0: linear from 50%→0% as HF approaches 1.0
    ///
    /// This replaces the original binary 50% approximation, ensuring that
    /// micro-liquidation positions receive proportionally correct debt coverage
    /// rather than being rounded to zero or a fixed half. The eight-decimal USD
    /// values are scaled through RAY (1e27) before the close factor is computed,
    /// preventing precision collapse for small positions.
    ///
    /// `user_reserve_debt` is a SINGLE reserve's current debt in that debt token's
    /// native units (post-index), and the return value is in those same units —
    /// which is what Aave's `liquidationCall(.., debtToCover, ..)` expects. Aave
    /// applies the close factor per reserve (`_calculateDebt` reads
    /// `userReserveDebt`), never to the position's aggregate USD value: mixing the
    /// two silently truncates `debtToCover` to dust whenever the USD figure is
    /// numerically smaller than the native one, which is every 18-decimal debt
    /// asset above roughly $1. The REVM simulator still computes the exact
    /// `actualDebtToLiquidate` (including `MIN_LEFTOVER_BASE` and protocol fee).
    /// Verify constants against the deployed Aave V3 Origin commit before live mode
    /// (see `docs/research/aave-v3-liquidation-compendium.md`).
    fn apply_close_factor(&self, hf: U256, user_reserve_debt: U256) -> U256 {
        let ray = ray();
        // CLOSE_FACTOR_HF_THRESHOLD = 0.95 in RAY scale.
        let close_factor_hf_threshold =
            U256::from(950_000_000_000_000_000_000_000_000u128); // 0.95e27

        // HF >= 1.0: not liquidatable.
        if hf >= ray {
            return U256::ZERO;
        }

        // HF <= 0.95: MAX_LIQUIDATION_CLOSE_FACTOR = 100%.
        if hf <= close_factor_hf_threshold {
            return user_reserve_debt;
        }

        // 0.95 < HF < 1.0: linear interpolation.
        // closeFactor_ray = 0.5e27 + 10 * (1.0e27 - hf)
        // (because 0.5e27 / 0.05e27 = 10)
        let default_close = ray / U256::from(2); // 0.5e27 (50%)
        let hf_above_threshold = ray - hf; // (1.0 - HF) in RAY
        // Extra close factor = 10 * (1.0 - HF) in RAY, clamped to [0, 0.5e27]
        let extra_factor = hf_above_threshold * U256::from(10);
        let close_factor_ray = default_close + extra_factor;

        // Apply close factor to native-unit debt:
        //   debt_to_cover = user_reserve_debt * close_factor_ray / ray
        // For safety, cap at the full user reserve debt (close_factor_ray ≤ ray).
        let debt_to_cover = user_reserve_debt * close_factor_ray / ray;
        if debt_to_cover > user_reserve_debt {
            user_reserve_debt
        } else {
            debt_to_cover
        }
    }

    /// Select the collateral asset with the highest USD-weighted liquidation
    /// bonus (seizable balance value × bonus). A higher bonus on more valuable
    /// collateral = more profit for the liquidator.
    fn select_best_collateral(
        &self,
        collateral: &HashMap<Address, U256>,
    ) -> Option<(Address, U256)> {
        let ray = ray();
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
                // Weight by the USD value of the balance, not raw token units:
                // raw units let an 18-dec asset dominate a 6/8-dec asset by
                // 10^10-10^12 regardless of actual value. Same normalization as
                // calculate_user_account_data (scaled × index / RAY, then price
                // over assetUnit).
                let weighted = self
                    .snapshot
                    .reserves
                    .get(&asset)
                    .map(|r| {
                        let current_balance = *balance * r.liquidity_index / ray;
                        let balance_usd = current_balance * r.price_usd / pow10(r.decimals);
                        balance_usd * U256::from(r.liquidation_bonus_bps)
                    })
                    .unwrap_or(U256::ZERO);
                (asset, weighted)
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
        let ray = ray();

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

                // Aave GenericLogic._getUserBalanceInBaseCurrency:
                //   balanceInBaseCurrency = (assetPrice * currentBalance) / assetUnit
                // where currentBalance = scaled * liquidity_index / RAY (native token
                // units) and assetUnit = 10^decimals. Dividing by assetUnit — NOT a
                // fixed 1e8 — is what converts native-unit balances to whole tokens
                // before pricing. Omitting it mis-scales HF by 10^(coll_dec - debt_dec)
                // for any mixed-decimal position (18-dec WETH collateral vs 6-dec USDC
                // debt is the dominant Base shape). Result is 8-decimal USD.
                let current_balance = *scaled_balance * reserve.liquidity_index / ray;
                let balance_usd = current_balance * reserve.price_usd / pow10(reserve.decimals);

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
                // Same assetUnit (10^decimals) normalization as collateral above.
                let current_debt = *scaled_debt * reserve.variable_borrow_index / ray;
                let debt_usd = current_debt * reserve.price_usd / pow10(reserve.decimals);

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

/// `10^exp` as [`U256`] — an asset's `assetUnit` (Aave decimals ≤ 18). Called
/// per collateral/debt asset in the per-block detection loop, so the decimals
/// real reserves use (6, 8, 18) take a constant fast path with no U256
/// exponentiation. Anything unusual falls back to `checked_pow`; a pathological
/// value above 10^77 overflows and yields `U256::MAX`, driving that asset's USD
/// value to ~0 (conservatively excluded) rather than panicking.
fn pow10(exp: u8) -> U256 {
    match exp {
        0 => U256::from(1u64),
        6 => U256::from(1_000_000u64),
        8 => U256::from(100_000_000u64),
        18 => U256::from(1_000_000_000_000_000_000u64),
        _ => U256::from(10u64)
            .checked_pow(U256::from(exp))
            .unwrap_or(U256::MAX),
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

    /// Build a minimal seizable reserve for HF tests.
    #[allow(clippy::too_many_arguments)]
    fn test_reserve(
        decimals: u8,
        price_8dec: u128,
        liquidity_index: U256,
        variable_borrow_index: U256,
        lt_bps: u16,
    ) -> ReserveData {
        ReserveData {
            a_token: Address::from_str("0x00000000000000000000000000000000000a1111").unwrap(),
            variable_debt_token: Address::from_str("0x00000000000000000000000000000000000d2222")
                .unwrap(),
            liquidity_index,
            variable_borrow_index,
            liquidation_bonus_bps: 10500,
            liquidation_threshold_bps: lt_bps,
            price_usd: U256::from(price_8dec),
            decimals,
            is_isolated: false,
            debt_ceiling: U256::ZERO,
            e_mode_category: 0,
            active: true,
            frozen: false,
            paused: false,
            liquidation_protocol_fee_bps: 1000,
            emode_liquidation_threshold_bps: 0,
            emode_liquidation_bonus_bps: 0,
            siloed_borrowing: false,
        }
    }

    /// Regression: `select_best_collateral` must weight by USD value × bonus,
    /// not raw token units. Raw units let an 18-dec asset with trivial value
    /// (here $10 of JUNK = 1e19 raw units) dominate a 6-dec asset worth far
    /// more ($5000 of USDC = 5e9 raw units) by ~10 orders of magnitude.
    #[test]
    fn test_best_collateral_weighted_by_usd_value_not_raw_units() {
        let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128);
        let junk = Address::from_str("0x000000000000000000000000000000000000aaaa").unwrap();
        let usdc = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap();
        let weth = Address::from_str("0x4200000000000000000000000000000000000006").unwrap();
        let user = Address::from_str("0x00000000000000000000000000000000000d3b7b").unwrap();

        let mut snap = make_snapshot();
        snap.reserves
            .insert(junk, test_reserve(18, 100_000_000, ray, ray, 8000)); // $1.00, 18-dec
        snap.reserves
            .insert(usdc, test_reserve(6, 100_000_000, ray, ray, 8000)); // $1.00, 6-dec
        snap.reserves
            .insert(weth, test_reserve(18, 250_000_000_000, ray, ray, 8250)); // $2500 debt asset
        snap.users.insert(
            user,
            UserPosition {
                collateral: HashMap::from([
                    // $10 of JUNK: 10e18 raw units.
                    (junk, U256::from(10_000_000_000_000_000_000u128)),
                    // $5000 of USDC: 5000e6 raw units.
                    (usdc, U256::from(5_000_000_000u128)),
                ]),
                // 1.7 WETH debt ($4250) → HF ≈ 0.943: liquidatable, not bad debt.
                // (1.6 WETH here gave HF 1.002 — above Aave's cutoff. It only
                // produced a candidate because the close factor returned 0 and the
                // old code emitted the position anyway with debt_to_cover = 0.)
                debt: HashMap::from([(weth, U256::from(1_700_000_000_000_000_000u128))]),
                emode_category: 0,
                is_in_isolation: false,
            },
        );

        let detector = LiquidationDetector::new(snap, 8453);
        let candidates = detector.find_at_risk_positions();
        assert!(!candidates.is_empty(), "position must be liquidatable");
        assert_eq!(
            candidates[0].collateral_asset, usdc,
            "must seize the $5000 USDC, not the $10 of 18-dec JUNK \
             (raw-unit weighting would pick JUNK by ~10 orders of magnitude)"
        );
    }

    /// Regression: a mixed-decimal position (18-dec WETH collateral, 6-dec USDC
    /// debt — the dominant Base shape) must compute a correct health factor. The
    /// prior code divided by a fixed 1e8 instead of each asset's 10^decimals, so
    /// HF was inflated by 10^(18-6)=10^12 and every such underwater position was
    /// silently dropped by the `hf < threshold` pre-filter. There was NO test that
    /// exercised calculate_user_account_data end-to-end, so the bug was dormant.
    #[test]
    fn test_hf_mixed_decimals_weth_collateral_usdc_debt() {
        let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128); // 1e27
        let weth = Address::from_str("0x4200000000000000000000000000000000000006").unwrap();
        let usdc = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap();
        let user = Address::from_str("0x00000000000000000000000000000000000d3b7a").unwrap();

        let mut snap = make_snapshot();
        // RAY indices → scaled balance == current balance, isolating the decimals math.
        snap.reserves
            .insert(weth, test_reserve(18, 250_000_000_000, ray, ray, 8250)); // $2500, LT 82.5%
        snap.reserves
            .insert(usdc, test_reserve(6, 100_000_000, ray, ray, 8000)); // $1.00
        snap.users.insert(
            user,
            UserPosition {
                // 2 WETH collateral ($5000), 4500 USDC debt ($4500).
                collateral: HashMap::from([(weth, U256::from(2_000_000_000_000_000_000u128))]),
                debt: HashMap::from([(usdc, U256::from(4_500_000_000u128))]),
                emode_category: 0,
                is_in_isolation: false,
            },
        );

        let detector = LiquidationDetector::new(snap, 8453);
        let candidates = detector.find_at_risk_positions();

        // True HF = (5000 * 0.825) / 4500 = 0.9167 → below the 1.01 threshold → flagged.
        assert_eq!(
            candidates.len(),
            1,
            "underwater mixed-decimal position must be flagged"
        );
        let c = &candidates[0];
        assert_eq!(c.user, user);
        // HF ≈ 0.9167e27; the pre-fix bug would yield ~0.9167e39 (10^12 too high).
        let lo = U256::from(915_000_000_000_000_000_000_000_000u128); // 0.915e27
        let hi = U256::from(918_000_000_000_000_000_000_000_000u128); // 0.918e27
        assert!(
            c.current_hf >= lo && c.current_hf <= hi,
            "HF must be ~0.9167e27, got {}",
            c.current_hf
        );
    }

    /// A healthy mixed-decimal position (HF well above threshold) must NOT be
    /// flagged — guards against the inverse error (HF scaled 10^12 too low, which
    /// would emit every healthy position as a false candidate).
    #[test]
    fn test_hf_mixed_decimals_healthy_not_flagged() {
        let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128);
        let weth = Address::from_str("0x4200000000000000000000000000000000000006").unwrap();
        let usdc = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap();
        let user = Address::from_str("0x00000000000000000000000000000000000d3b7a").unwrap();

        let mut snap = make_snapshot();
        snap.reserves
            .insert(weth, test_reserve(18, 250_000_000_000, ray, ray, 8250));
        snap.reserves
            .insert(usdc, test_reserve(6, 100_000_000, ray, ray, 8000));
        snap.users.insert(
            user,
            UserPosition {
                // 2 WETH collateral ($5000) vs only 1000 USDC debt ($1000) → HF ≈ 4.1.
                collateral: HashMap::from([(weth, U256::from(2_000_000_000_000_000_000u128))]),
                debt: HashMap::from([(usdc, U256::from(1_000_000_000u128))]),
                emode_category: 0,
                is_in_isolation: false,
            },
        );

        let detector = LiquidationDetector::new(snap, 8453);
        assert!(
            detector.find_at_risk_positions().is_empty(),
            "healthy position must not be flagged"
        );
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
    fn flag_threshold_sits_just_above_aaves_hard_cutoff() {
        // Aave's HEALTH_FACTOR_LIQUIDATION_THRESHOLD is 1.0 (1e18 WAD / 1e27 RAY).
        // `validateLiquidationCall` rejects anything >= 1.0 with
        // HealthFactorNotBelowThreshold() — selector 0x930bb771, observed live.
        //
        // The flag threshold must stay ABOVE 1.0 (so positions are seen shortly
        // before they cross) but close to it (so we do not simulate a wide band
        // of positions the Pool will always refuse). 1.05 cost a continuous
        // stream of guaranteed-revert simulations; the ceiling here is the
        // regression guard against drifting back.
        let detector = LiquidationDetector::default();
        let ray = U256::from(10).pow(U256::from(27));
        let ceiling = U256::from(102) * U256::from(10).pow(U256::from(25)); // 1.02e27

        assert!(
            detector.hf_liquidation_threshold > ray,
            "threshold must exceed 1.0 or positions are only seen after they cross"
        );
        assert!(
            detector.hf_liquidation_threshold <= ceiling,
            "threshold must stay near 1.0; a wide band simulates positions Aave \
             refuses with HealthFactorNotBelowThreshold()"
        );
    }

    /// 1 WETH in native units — deliberately NOT the same magnitude as any USD
    /// figure in these tests, so a units mix-up cannot pass by coincidence.
    const ONE_WETH: u128 = 1_000_000_000_000_000_000;

    #[test]
    fn test_close_factor_hf_above_one() {
        let detector = LiquidationDetector::new(make_snapshot(), 8453);
        let result = detector.apply_close_factor(
            U256::from(1_050_000_000_000_000_000_000_000_000_000u128), // 1.05e27
            U256::from(ONE_WETH),
        );
        assert_eq!(result, U256::ZERO);
    }

    #[test]
    fn test_close_factor_hf_below_95_is_full() {
        let detector = LiquidationDetector::new(make_snapshot(), 8453);
        let result = detector.apply_close_factor(
            U256::from(940_000_000_000_000_000_000_000_000u128), // 0.94e27
            U256::from(ONE_WETH),
        );
        // HF <= 0.95 => MAX close factor = 100% of this reserve's debt.
        assert_eq!(result, U256::from(ONE_WETH));
    }

    #[test]
    fn test_close_factor_hf_at_95_boundary_is_full() {
        let detector = LiquidationDetector::new(make_snapshot(), 8453);
        let result = detector.apply_close_factor(
            U256::from(950_000_000_000_000_000_000_000_000u128), // exactly 0.95e27
            U256::from(ONE_WETH),
        );
        assert_eq!(result, U256::from(ONE_WETH));
    }

    #[test]
    fn test_close_factor_default_50_above_95() {
        let detector = LiquidationDetector::new(make_snapshot(), 8453);
        // HF=0.96: close factor = 0.5 + 10*(1.0-0.96) = 0.5 + 0.4 = 0.9
        let result = detector.apply_close_factor(
            U256::from(960_000_000_000_000_000_000_000_000u128), // 0.96e27
            U256::from(ONE_WETH),
        );
        assert_eq!(result, U256::from(ONE_WETH * 9 / 10));

        // HF=0.99: close factor = 0.5 + 10*(1.0-0.99) = 0.5 + 0.1 = 0.6
        let result = detector.apply_close_factor(
            U256::from(990_000_000_000_000_000_000_000_000u128), // 0.99e27
            U256::from(ONE_WETH),
        );
        assert_eq!(result, U256::from(ONE_WETH * 6 / 10));
    }

    /// Regression: `debt_to_cover` must come out in the debt token's NATIVE units.
    ///
    /// It was previously derived from the position's aggregate 8-decimal USD debt
    /// and then `min`'d against the native balance. For any 18-decimal debt asset
    /// worth more than ~$1 the USD number is the smaller of the two, so the `min`
    /// silently returned it — here 2000e8, i.e. 0.0000002 WETH instead of 1 WETH,
    /// a 5-billion-fold under-liquidation that would burn gas for dust. USDC debt
    /// masked the bug because 6-decimal native units exceed the USD figure, so the
    /// `min` happened to pick the correct side.
    ///
    /// Position: 2100 USDC collateral (LT 8250) against 1 WETH debt at $2000.
    /// HF = 2100 × 0.825 / 2000 = 0.866 => below 0.95 => full close factor.
    #[test]
    fn test_debt_to_cover_is_native_units_not_usd_for_18_decimal_debt() {
        let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128);
        let usdc = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap();
        let weth = Address::from_str("0x4200000000000000000000000000000000000006").unwrap();
        let user = Address::from_str("0x00000000000000000000000000000000000d3b7c").unwrap();

        let mut snap = make_snapshot();
        snap.reserves
            .insert(usdc, test_reserve(6, 100_000_000, ray, ray, 8250)); // $1.00, 6-dec
        snap.reserves
            .insert(weth, test_reserve(18, 200_000_000_000, ray, ray, 8250)); // $2000, 18-dec
        snap.users.insert(
            user,
            UserPosition {
                collateral: HashMap::from([(usdc, U256::from(2_100_000_000u128))]), // 2100 USDC
                debt: HashMap::from([(weth, U256::from(ONE_WETH))]),                // 1 WETH
                ..UserPosition::default()
            },
        );

        let detector = LiquidationDetector::new(snap, 8453);
        let candidates = detector.find_at_risk_positions();

        let c = candidates
            .iter()
            .find(|c| c.debt_asset == weth)
            .expect("position must be flagged at HF 0.866");

        assert_eq!(
            c.debt_to_cover,
            U256::from(ONE_WETH),
            "full close factor on 1 WETH of debt must be 1 WETH in native units"
        );
        // The exact value the old USD-vs-native `min` produced. Pinned so a
        // regression cannot pass by landing on a merely plausible number.
        assert_ne!(
            c.debt_to_cover,
            U256::from(200_000_000_000u128),
            "debt_to_cover collapsed to the 8-decimal USD debt figure"
        );
    }

    /// The close factor applies to debt brought forward by the borrow index, not
    /// to the raw scaled balance. Index 1.2 on 1 WETH scaled => 1.2 WETH owed.
    #[test]
    fn test_debt_to_cover_applies_variable_borrow_index() {
        let ray = U256::from(1_000_000_000_000_000_000_000_000_000u128);
        let index_1_2 = U256::from(1_200_000_000_000_000_000_000_000_000u128); // 1.2e27
        let usdc = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap();
        let weth = Address::from_str("0x4200000000000000000000000000000000000006").unwrap();
        let user = Address::from_str("0x00000000000000000000000000000000000d3b7d").unwrap();

        let mut snap = make_snapshot();
        snap.reserves
            .insert(usdc, test_reserve(6, 100_000_000, ray, ray, 8250));
        snap.reserves.insert(
            weth,
            test_reserve(18, 200_000_000_000, ray, index_1_2, 8250),
        );
        snap.users.insert(
            user,
            UserPosition {
                // 2500 USDC collateral vs 1.2 WETH ($2400) owed => HF 0.859, full tier.
                collateral: HashMap::from([(usdc, U256::from(2_500_000_000u128))]),
                debt: HashMap::from([(weth, U256::from(ONE_WETH))]),
                ..UserPosition::default()
            },
        );

        let detector = LiquidationDetector::new(snap, 8453);
        let c = detector
            .find_at_risk_positions()
            .into_iter()
            .find(|c| c.debt_asset == weth)
            .expect("position must be flagged at HF 0.859");

        assert_eq!(
            c.debt_to_cover,
            U256::from(ONE_WETH * 12 / 10),
            "debt_to_cover must be index-adjusted (1 WETH scaled x 1.2 = 1.2 WETH)"
        );
    }

    /// A debt asset absent from the snapshot has no borrow index and no price, so
    /// `current_debt` falls back to the raw scaled balance (index defaults to RAY,
    /// matching `parse_index_or_ray`) and the candidate carries `debt_price_usd = 0`.
    /// Zero is the simulator's "unknown" sentinel, not "free": both
    /// `simulator::mod.rs` price guards fall back to a bonus-portion estimate rather
    /// than valuing the leg at $0. The position needs a second, priced debt asset —
    /// an unpriced one contributes nothing to `total_debt`, so on its own it would
    /// never reach the health-factor check at all.
    #[test]
    fn test_debt_asset_missing_from_snapshot_falls_back_to_scaled_balance() {
        let usdc = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap();
        let weth = Address::from_str("0x4200000000000000000000000000000000000006").unwrap();
        let unknown = Address::from_str("0x000000000000000000000000000000000000bbbb").unwrap();
        let user = Address::from_str("0x00000000000000000000000000000000000d3b7e").unwrap();

        let mut snap = make_snapshot();
        snap.reserves
            .insert(usdc, test_reserve(6, 100_000_000, ray(), ray(), 8250));
        snap.reserves
            .insert(weth, test_reserve(18, 200_000_000_000, ray(), ray(), 8250));
        // `unknown` is deliberately NOT inserted into reserves.
        snap.users.insert(
            user,
            UserPosition {
                collateral: HashMap::from([(usdc, U256::from(2_100_000_000u128))]),
                debt: HashMap::from([
                    (weth, U256::from(ONE_WETH)),
                    (unknown, U256::from(12_345u64)),
                ]),
                ..UserPosition::default()
            },
        );

        let detector = LiquidationDetector::new(snap, 8453);
        let candidates = detector.find_at_risk_positions();

        let c = candidates
            .iter()
            .find(|c| c.debt_asset == unknown)
            .expect("unpriced debt asset must still yield a candidate");
        assert_eq!(
            c.debt_to_cover,
            U256::from(12_345u64),
            "no reserve => no index => raw scaled balance, un-adjusted"
        );
        assert_eq!(
            c.debt_price_usd,
            U256::ZERO,
            "unknown price must stay 0 so the simulator takes its fallback path"
        );
        assert_eq!(
            c.debt_decimals, 18,
            "absent reserve defaults to 18 decimals"
        );
    }
}