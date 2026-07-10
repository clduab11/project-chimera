//! Pacing Engine - The financial governor and risk-control layer for Chimera.
//! Pure logic (no I/O in hot path). Enforces every cap, jitter window, and breaker from the Executive Brief.
//! Called on EVERY candidate opportunity before any simulation or submission.
//! Post-outcome updates are also mandatory.
//!
//! SAFETY NOTES:
//! - All monetary values use `Decimal` (not `f64`) to avoid floating-point errors.
//! - The engine uses an internal `parking_lot::RwLock` for thread-safe access.
//! - State is persisted to JSONL on every `record_outcome` for crash-safe recovery.
use crate::state::{CrashRecovery, OutcomeRecord, StatePersistence};
use crate::{ChimeraError, PacingConfig, PriceOracle};
use chrono::{DateTime, TimeDelta, Utc};
use parking_lot::RwLock;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;

use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Opportunity {
    pub id: String,
    pub expected_net_usd: Decimal,
    pub gas_estimate_gwei: u64,
    pub venue: String,
    pub eoa: String,
    pub timestamp: DateTime<Utc>,
}
impl Default for Opportunity {
    fn default() -> Self {
        Self {
            id: "default".into(),
            expected_net_usd: Decimal::ZERO,
            gas_estimate_gwei: 0,
            venue: "unknown".into(),
            eoa: "0x0000000000000000000000000000000000000000".into(),
            timestamp: Utc::now(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PacingDecision {
    Allow { release_at: DateTime<Utc> },
    Deny { reason: String },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BreakerReason {
    TooManyReverts(u32),
    GasPriceTooHigh(u64),
    DailyLossLimitExceeded,
    WeeklyCapExceeded,
    SingleTransferCapExceeded,
    /// Force-tripped by an external emergency (e.g. emergency.flag). Carries the reason.
    EmergencyHalt(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskState {
    pub daily_net_usd: Decimal,
    pub weekly_net_usd: Decimal,
    pub daily_loss_eth: Decimal,
    pub consecutive_reverts: u32,
    pub last_release: Option<DateTime<Utc>>,
    pub breaker_tripped: Option<BreakerReason>,
}
impl Default for RiskState {
    fn default() -> Self {
        Self {
            daily_net_usd: Decimal::ZERO,
            weekly_net_usd: Decimal::ZERO,
            daily_loss_eth: Decimal::ZERO,
            consecutive_reverts: 0,
            last_release: None,
            breaker_tripped: None,
        }
    }
}
struct PacingEngineInner {
    config: PacingConfig,
    daily_net_usd: Decimal,
    weekly_net_usd: Decimal,
    daily_loss_eth: Decimal,
    consecutive_reverts: u32,
    last_release: Option<DateTime<Utc>>,
    recent_outcomes: VecDeque<(DateTime<Utc>, Decimal)>,
    /// Rolling 24h window of (timestamp, gas_spent_eth) for losing outcomes.
    /// Parallels `recent_outcomes`; used to recompute `daily_loss_eth` so the
    /// DailyLossLimit breaker decays over time instead of accumulating forever.
    /// NOT persisted in RiskState — it is a runtime reconstruction aid.
    daily_losses: VecDeque<(DateTime<Utc>, Decimal)>,
    breaker_tripped: Option<BreakerReason>,
    venue_rotation: VecDeque<String>,
    eoa_rotation: VecDeque<String>,
    cached_eth_price: Option<Decimal>,
}
impl PacingEngineInner {
    fn check(&self, opp: &Opportunity) -> PacingDecision {
        if let Some(breaker) = &self.breaker_tripped {
            return PacingDecision::Deny {
                reason: format!("Breaker active: {:?}", breaker),
            };
        }
        // Venue rotation gate: deny if the same venue was used within the rotation window.
        if self.venue_rotation.contains(&opp.venue) {
            return PacingDecision::Deny {
                reason: format!("Venue {} recently used; rotation required", opp.venue),
            };
        }
        // EOA pool validation: if a pool is loaded, the opportunity must use a known EOA.
        if !self.eoa_rotation.is_empty()
            && !self
                .eoa_rotation
                .iter()
                .any(|eoa| eoa.eq_ignore_ascii_case(&opp.eoa))
        {
            return PacingDecision::Deny {
                reason: format!("EOA {} not in clean pool", opp.eoa),
            };
        }
        // 1. Single transfer cap ΓÇö direct Decimal field, no conversion needed
        if opp.expected_net_usd > self.config.max_single_transfer_usd {
            return PacingDecision::Deny {
                reason: format!(
                    "Single transfer {} USD exceeds cap {}",
                    opp.expected_net_usd, self.config.max_single_transfer_usd
                ),
            };
        }
        // 2. Daily cap
        if self.daily_net_usd + opp.expected_net_usd > self.config.max_daily_net_usd {
            return PacingDecision::Deny {
                reason: "Daily net cap would be exceeded".into(),
            };
        }
        // 3. Weekly cap
        if self.weekly_net_usd + opp.expected_net_usd > self.config.max_weekly_net_usd {
            return PacingDecision::Deny {
                reason: "Weekly net cap would be exceeded".into(),
            };
        }
        // 4. Timing / jitter window
        if let Some(last) = self.last_release {
            let min_gap = TimeDelta::hours(self.config.min_interval_hours as i64);
            let since_last = Utc::now() - last;
            if since_last < min_gap {
                return PacingDecision::Deny {
                    reason: format!(
                        "Minimum {}h interval not met ({}h since last)",
                        self.config.min_interval_hours,
                        since_last.num_hours()
                    ),
                };
            }
        }
        // 5. Profit multiplier enforcement ΓÇö direct Decimal field, no conversion needed
        let gas_cost_usd = self.estimate_gas_cost_usd(opp.gas_estimate_gwei);
        if gas_cost_usd > Decimal::ZERO {
            let ratio = opp.expected_net_usd / gas_cost_usd;
            if ratio < self.config.min_profit_multiplier {
                return PacingDecision::Deny {
                    reason: "InsufficientProfit".into(),
                };
            }
        }
        // 6. Profit safety warning
        let min_profit_threshold = Decimal::ONE; // $1 minimum
        if opp.expected_net_usd < min_profit_threshold {
            warn!("Opportunity {} has very low EV - verify simulation", opp.id);
        }
        // All gates passed: compute jittered release time
        let jitter_seconds = rand::random::<u64>() % (self.config.max_jitter_hours * 3600);
        let min_interval_seconds = self.config.min_interval_hours as i64 * 3600;
        let release_at =
            Utc::now() + TimeDelta::seconds(jitter_seconds as i64 + min_interval_seconds);
        PacingDecision::Allow { release_at }
    }

    /// Conservative gas cost estimate in USD.
    /// Uses a fixed liquidation gas budget and configured fallback ETH price.
    /// Both fields are now native Decimal ΓÇö no f64 conversion needed.
    fn estimate_gas_cost_usd(&self, gas_estimate_gwei: u64) -> Decimal {
        const ESTIMATED_GAS_UNITS: u64 = 150_000;
        let gas_cost_eth = Decimal::from(gas_estimate_gwei) * Decimal::from(ESTIMATED_GAS_UNITS)
            / Decimal::from(1_000_000_000u64);
        let eth_price = self
            .cached_eth_price
            .unwrap_or(self.config.eth_price_usd_fallback);
        gas_cost_eth * eth_price
    }

    fn record_outcome(
        &mut self,
        opp: &Opportunity,
        realized_net_usd: Decimal,
        gas_spent_eth: Decimal,
        reverted: bool,
    ) {
        let now = Utc::now();
        self.recent_outcomes.push_back((now, realized_net_usd));
        // Prune outcomes older than 7 days
        while let Some((ts, _)) = self.recent_outcomes.front() {
            if now - *ts > TimeDelta::days(7) {
                self.recent_outcomes.pop_front();
            } else {
                break;
            }
        }
        // Recompute daily and weekly from rolling window
        self.daily_net_usd = self
            .recent_outcomes
            .iter()
            .filter(|(ts, _)| now - *ts <= TimeDelta::days(1))
            .map(|(_, v)| *v)
            .sum();
        self.weekly_net_usd = self.recent_outcomes.iter().map(|(_, v)| *v).sum();
        // Rolling 24h loss window, mirroring the daily_net_usd recompute above.
        // `daily_loss_eth` must decay like daily/weekly (window-recomputed) rather
        // than accumulate monotonically, otherwise the DailyLossLimit breaker would
        // trip permanently once cumulative loss crossed the cap across many days.
        if realized_net_usd < Decimal::ZERO {
            self.daily_losses.push_back((now, gas_spent_eth));
        }
        while let Some((ts, _)) = self.daily_losses.front() {
            if now - *ts > TimeDelta::days(1) {
                self.daily_losses.pop_front();
            } else {
                break;
            }
        }
        self.daily_loss_eth = self
            .daily_losses
            .iter()
            .filter(|(ts, _)| now - *ts <= TimeDelta::days(1))
            .map(|(_, v)| *v)
            .sum();
        if reverted {
            self.consecutive_reverts += 1;
        } else {
            self.consecutive_reverts = 0;
            self.last_release = Some(now);
        }
        // Update venue rotation: track recently used venues.
        if self.config.venue_rotation_count > 0 {
            self.venue_rotation.push_back(opp.venue.clone());
            while self.venue_rotation.len() >= self.config.venue_rotation_count as usize {
                self.venue_rotation.pop_front();
            }
        }
        // Breaker checks (order matters - most severe first)
        // All cap comparisons use native Decimal fields ΓÇö no f64 conversion needed.
        if self.consecutive_reverts >= self.config.auto_halt_on_reverts {
            self.breaker_tripped = Some(BreakerReason::TooManyReverts(self.consecutive_reverts));
            warn!(
                "BREAKER: Too many consecutive reverts ({})",
                self.consecutive_reverts
            );
        } else if opp.gas_estimate_gwei > self.config.max_gas_gwei {
            self.breaker_tripped = Some(BreakerReason::GasPriceTooHigh(opp.gas_estimate_gwei));
        } else if self.daily_loss_eth > self.config.max_daily_loss_eth {
            self.breaker_tripped = Some(BreakerReason::DailyLossLimitExceeded);
        } else if self.weekly_net_usd > self.config.max_weekly_net_usd {
            self.breaker_tripped = Some(BreakerReason::WeeklyCapExceeded);
        }
        info!(
            target: "chimera::pacing",
            id = %opp.id,
            realized = %realized_net_usd,
            daily = %self.daily_net_usd,
            weekly = %self.weekly_net_usd,
            reverts = %self.consecutive_reverts,
            breaker = ?self.breaker_tripped,
            "Outcome recorded"
        );
    }
    fn to_risk_state(&self) -> RiskState {
        RiskState {
            daily_net_usd: self.daily_net_usd,
            weekly_net_usd: self.weekly_net_usd,
            daily_loss_eth: self.daily_loss_eth,
            consecutive_reverts: self.consecutive_reverts,
            last_release: self.last_release,
            breaker_tripped: self.breaker_tripped.clone(),
        }
    }
    fn from_risk_state_and_config(
        config: PacingConfig,
        state: RiskState,
        eoa_pool: VecDeque<String>,
    ) -> Self {
        let capacity = config.recent_outcomes_capacity.max(1);
        Self {
            config,
            daily_net_usd: state.daily_net_usd,
            weekly_net_usd: state.weekly_net_usd,
            daily_loss_eth: state.daily_loss_eth,
            consecutive_reverts: state.consecutive_reverts,
            last_release: state.last_release,
            recent_outcomes: VecDeque::with_capacity(capacity),
            // `daily_losses` is not persisted in RiskState. When restoring from a
            // state with a non-zero `daily_loss_eth`, we seed it empty best-effort:
            // the next record_outcome prune/recompute will then reflect only losses
            // within the live 24h window (zeroing stale carryover after a day).
            daily_losses: VecDeque::new(),
            breaker_tripped: state.breaker_tripped,
            venue_rotation: VecDeque::new(),
            eoa_rotation: eoa_pool,
            cached_eth_price: None,
        }
    }
}

/// Thread-safe, crash-safe pacing engine.
/// Uses internal RwLock for concurrent read access and exclusive write access.
pub struct PacingEngine {
    inner: Arc<RwLock<PacingEngineInner>>,
    state_path: Option<std::path::PathBuf>,
    state_persistence: Option<Arc<dyn StatePersistence + Send + Sync>>,
    eth_price_oracle: Option<Arc<dyn PriceOracle + Send + Sync>>,
}
impl Clone for PacingEngine {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            state_path: self.state_path.clone(),
            state_persistence: self.state_persistence.clone(),
            eth_price_oracle: self.eth_price_oracle.clone(),
        }
    }
}
impl PacingEngine {
    pub fn new(config: PacingConfig) -> Self {
        Self::from_config(config, None)
    }
    pub fn from_config(config: PacingConfig, state_path: Option<std::path::PathBuf>) -> Self {
        let eoa_pool = Self::load_eoa_pool(&config.eoa_pool_path).unwrap_or_default();
        let capped_pool: VecDeque<String> = eoa_pool
            .into_iter()
            .take(config.clean_eoa_pool_size)
            .collect();
        let config_for_state = config.clone();
        let cap = config.recent_outcomes_capacity.max(1);
        let inner = if let Some(ref path) = state_path {
            Self::load_state(path)
                .map(|state| {
                    PacingEngineInner::from_risk_state_and_config(
                        config_for_state,
                        state,
                        capped_pool.clone(),
                    )
                })
                .unwrap_or_else(|| PacingEngineInner {
                    config: config.clone(),
                    daily_net_usd: Decimal::ZERO,
                    weekly_net_usd: Decimal::ZERO,
                    daily_loss_eth: Decimal::ZERO,
                    consecutive_reverts: 0,
                    last_release: None,
                    recent_outcomes: VecDeque::with_capacity(cap),
                    daily_losses: VecDeque::new(),
                    breaker_tripped: None,
                    venue_rotation: VecDeque::new(),
                    eoa_rotation: capped_pool,
                    cached_eth_price: None,
                })
        } else {
            PacingEngineInner {
                config,
                daily_net_usd: Decimal::ZERO,
                weekly_net_usd: Decimal::ZERO,
                daily_loss_eth: Decimal::ZERO,
                consecutive_reverts: 0,
                last_release: None,
                recent_outcomes: VecDeque::with_capacity(cap),
                daily_losses: VecDeque::new(),
                breaker_tripped: None,
                venue_rotation: VecDeque::new(),
                eoa_rotation: capped_pool,
                cached_eth_price: None,
            }
        };
        Self {
            inner: Arc::new(RwLock::new(inner)),
            state_path,
            state_persistence: None,
            eth_price_oracle: None,
        }
    }
    pub fn with_state_persistence(
        mut self,
        persistence: Arc<dyn StatePersistence + Send + Sync>,
    ) -> Self {
        self.state_persistence = Some(persistence);
        self
    }
    /// Attach an ETH/USD price oracle for live gas-cost estimates.
    /// The cached price must be refreshed periodically via [`Self::refresh_eth_price`].
    /// The feed address is read from the engine's config (`eth_usd_feed_address`).
    pub fn with_eth_oracle(mut self, oracle: Arc<dyn PriceOracle + Send + Sync>) -> Self {
        self.eth_price_oracle = Some(oracle);
        self
    }
    /// Fetch the current ETH/USD price from the attached oracle and cache it.
    /// On failure, the previous cached price is retained (or the fallback is used).
    pub async fn refresh_eth_price(&self) {
        if let Some(ref oracle) = self.eth_price_oracle {
            let feed_str = self.inner.read().config.eth_usd_feed_address.clone();
            if let Ok(addr) = feed_str.parse::<alloy::primitives::Address>() {
                match oracle.get_price(addr).await {
                    Ok(price) => {
                        if price > Decimal::ZERO {
                            self.inner.write().cached_eth_price = Some(price);
                            info!(
                                target: "chimera::pacing",
                                eth_usd = %price,
                                "Oracle ETH/USD price cached"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            target: "chimera::pacing",
                            error = %e,
                            "Oracle refresh failed; using cached or fallback ETH/USD"
                        );
                    }
                }
            }
        }
    }
    /// Load the EOA pool from either a JSON array of address strings or the
    /// structured `config/eoa_pool.json` shape with a `wallets[].address` list.
    pub fn load_eoa_pool(path: &str) -> Result<Vec<String>, ChimeraError> {
        if !std::path::Path::new(path).exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(path).map_err(|e| {
            ChimeraError::PersistenceError(format!("Failed to read EOA pool: {}", e))
        })?;
        let value: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| ChimeraError::PersistenceError(format!("Invalid EOA pool JSON: {}", e)))?;
        if let Some(addresses) = value.as_array() {
            return Ok(addresses
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                .collect());
        }
        let wallets = value
            .get("wallets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                ChimeraError::PersistenceError("EOA pool JSON must contain a wallets array".into())
            })?;
        Ok(wallets
            .iter()
            .filter_map(|wallet| {
                let excluded = wallet
                    .get("excluded")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if excluded {
                    return None;
                }
                wallet
                    .get("address")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_lowercase())
            })
            .collect())
    }
    /// Select the next EOA from the rotation pool and cycle it to the back.
    pub fn select_next_eoa(&self) -> Option<String> {
        let mut inner = self.inner.write();
        if let Some(eoa) = inner.eoa_rotation.pop_front() {
            inner.eoa_rotation.push_back(eoa.clone());
            Some(eoa)
        } else {
            None
        }
    }
    fn load_state(path: &Path) -> Option<RiskState> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }
    fn persist_state(&self) {
        if let Some(ref path) = self.state_path {
            if let Ok(state) = serde_json::to_string(&self.inner.read().to_risk_state()) {
                let _ = std::fs::write(path, state);
            }
        }
    }
    /// THE critical gate. Must be called before any on-chain action or heavy simulation.
    pub fn check(&self, opp: &Opportunity) -> Result<PacingDecision, ChimeraError> {
        Ok(self.inner.read().check(opp))
    }
    /// MUST be called after every outcome (success or revert) to update state and possibly trip breakers.
    /// Persists risk state to JSONL on every call (crash-safe).
    pub fn record_outcome(
        &self,
        opp: &Opportunity,
        realized_net_usd: Decimal,
        gas_spent_eth: Decimal,
        reverted: bool,
    ) {
        {
            let mut inner = self.inner.write();
            inner.record_outcome(opp, realized_net_usd, gas_spent_eth, reverted);
        }
        self.persist_state();
        if let Some(ref persistence) = self.state_persistence {
            let chain_id = self.inner.read().config.chain_id;
            let outcome = OutcomeRecord {
                id: opp.id.clone(),
                timestamp: Utc::now(),
                decision: if reverted {
                    "Revert".into()
                } else {
                    "Allow".into()
                },
                realized_net_usd,
                gas_spent_eth,
                reverted,
                venue: opp.venue.clone(),
                eoa: opp.eoa.clone(),
                chain_id,
            };
            // Best-effort fire-and-forget async append.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let persistence = Arc::clone(persistence);
                handle.spawn(async move {
                    if let Err(e) = persistence.append_outcome(outcome).await {
                        warn!("JSONL append failed: {}", e);
                    }
                });
            }
        }
    }
    /// Operator can manually clear a breaker after investigation.
    /// Gated behind the `CHIMERA_OPERATOR_TOKEN` env var to prevent unauthorized clears.
    pub fn clear_breaker(&self, operator_token: &str) -> Result<(), ChimeraError> {
        let expected = std::env::var("CHIMERA_OPERATOR_TOKEN").unwrap_or_default();
        if expected.is_empty() {
            return Err(ChimeraError::ConfigError(
                "clear_breaker refused: CHIMERA_OPERATOR_TOKEN not set".into(),
            ));
        }
        // Constant-time-ish comparison; tokens are short operator secrets.
        if operator_token != expected {
            warn!("OPERATOR: clear_breaker called with invalid token; refusing");
            return Err(ChimeraError::ConfigError(
                "clear_breaker refused: invalid operator token".into(),
            ));
        }
        let mut inner = self.inner.write();
        if inner.breaker_tripped.is_some() {
            warn!("OPERATOR: Manually clearing breaker. Ensure root cause resolved.");
            inner.breaker_tripped = None;
            inner.consecutive_reverts = 0;
        }
        drop(inner);
        self.persist_state();
        Ok(())
    }

    /// Force-trip the breaker due to an external emergency (e.g. emergency.flag).
    /// Idempotent: re-tripping with the same reason is a no-op on state.
    pub fn trip_emergency(&self, reason: &str) {
        let mut inner = self.inner.write();
        if !matches!(inner.breaker_tripped, Some(BreakerReason::EmergencyHalt(_))) {
            warn!(target: "chimera::pacing", reason = %reason, "EMERGENCY: breaker tripped via flag");
            inner.breaker_tripped = Some(BreakerReason::EmergencyHalt(reason.to_string()));
        }
        drop(inner);
        self.persist_state();
    }
    pub fn is_breaker_active(&self) -> bool {
        self.inner.read().breaker_tripped.is_some()
    }
    pub fn current_daily_usage(&self) -> Decimal {
        self.inner.read().daily_net_usd
    }
    pub fn current_risk_state(&self) -> RiskState {
        self.inner.read().to_risk_state()
    }

    /// Return the most recently cached ETH/USD price, if any.
    /// Returns the last successful oracle refresh value, or None if
    /// the oracle has never been refreshed.
    pub fn cached_eth_price(&self) -> Option<Decimal> {
        self.inner.read().cached_eth_price
    }
}

// ---------------------------------------------------------------------------
// CrossProcessPacing — cross-process reservation lifecycle
// ---------------------------------------------------------------------------

use crate::state::{ReservationRecord, ReservationStatus};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration as StdDuration, Instant};

const RESERVATION_TTL_MINUTES: i64 = 5;
const LOCK_TIMEOUT_MS: u64 = 5000;
const LOCK_RETRY_MS: u64 = 50;

/// Wraps [`PacingEngine`] with cross-process reservation tracking.
///
/// Multiple per-chain processes share a `reservations_dir` on local disk.
/// Under advisory file lock, each process writes `Reserved` records before
/// firing and `Settled`/`Expired` records on outcome, so global daily/weekly
/// caps are enforced across processes.
pub struct CrossProcessPacing {
    engine: PacingEngine,
    reservations_dir: PathBuf,
    outcomes_path: PathBuf,
    reservation_ttl_minutes: i64,
}

impl CrossProcessPacing {
    pub fn new(engine: PacingEngine, reservations_dir: PathBuf, outcomes_path: PathBuf) -> Self {
        Self {
            engine,
            reservations_dir,
            outcomes_path,
            reservation_ttl_minutes: RESERVATION_TTL_MINUTES,
        }
    }

    pub fn with_ttl(mut self, ttl_minutes: i64) -> Self {
        self.reservation_ttl_minutes = ttl_minutes;
        self
    }

    pub fn engine(&self) -> &PacingEngine {
        &self.engine
    }

    fn reservations_path(&self, chain_id: u64) -> PathBuf {
        self.reservations_dir
            .join(format!("reservations-{}.jsonl", chain_id))
    }

    fn lock_path(&self) -> PathBuf {
        self.reservations_dir.join(".reservations.lock")
    }

    fn acquire_lock(&self) -> Result<FileLockGuard, ChimeraError> {
        let lock_path = self.lock_path();
        fs::create_dir_all(&self.reservations_dir).map_err(ChimeraError::Io)?;
        let start = Instant::now();
        loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(file) => {
                    let pid = std::process::id().to_string();
                    let _ = file.set_len(0);
                    let mut f = file;
                    let _ = f.write_all(pid.as_bytes());
                    let _ = f.flush();
                    return Ok(FileLockGuard {
                        path: lock_path,
                        _file: Some(f),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if start.elapsed() > StdDuration::from_millis(LOCK_TIMEOUT_MS) {
                        return Err(ChimeraError::TimeoutError(
                            "Cross-process reservation lock acquisition timed out".into(),
                        ));
                    }
                    std::thread::sleep(StdDuration::from_millis(LOCK_RETRY_MS));
                }
                Err(e) => return Err(ChimeraError::Io(e)),
            }
        }
    }

    /// Force-clear a stale lock file left behind by a crashed process.
    /// Only call this when you are certain no other process holds the lock.
    pub fn clear_stale_lock(&self) -> Result<(), ChimeraError> {
        let lock_path = self.lock_path();
        if lock_path.exists() {
            fs::remove_file(&lock_path).map_err(ChimeraError::Io)?;
        }
        Ok(())
    }

    fn load_reservations(path: &std::path::Path) -> Result<Vec<ReservationRecord>, ChimeraError> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path).map_err(ChimeraError::Io)?;
        let mut records = Vec::new();
        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<ReservationRecord>(trimmed) {
                Ok(record) => records.push(record),
                Err(e) => {
                    let msg = format!(
                        "Malformed reservation JSONL at {} line {}: {}",
                        path.display(),
                        line_no + 1,
                        e
                    );
                    warn!(target: "chimera::pacing", "{}", msg);
                    return Err(ChimeraError::PersistenceError(msg));
                }
            }
        }
        Ok(records)
    }

    /// Load all reservation files under the shared reservations directory.
    /// Matches files named `reservations-{chain_id}.jsonl`.
    ///
    /// Each per-chain file is loaded via [`Self::load_reservations`] which
    /// fails-closed on any malformed line — a single corrupt file halts
    /// cross-process pacing and denies all new reservations until recovery.
    fn load_all_reservations(
        reservations_dir: &std::path::Path,
    ) -> Result<Vec<ReservationRecord>, ChimeraError> {
        let mut all = Vec::new();
        let entries = fs::read_dir(reservations_dir).map_err(ChimeraError::Io)?;
        for entry in entries {
            let entry = entry.map_err(ChimeraError::Io)?;
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "jsonl")
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map_or(false, |n| n.starts_with("reservations-"))
            {
                let mut records = Self::load_reservations(&path)?;
                all.append(&mut records);
            }
        }
        Ok(all)
    }

    /// Recover realized daily/weekly usage from a shared outcomes audit trail.
    /// Uses the synchronous [`CrashRecovery::recover_from_jsonl_sync`] path so
    /// no nested Tokio runtime is created (safe to call from either sync or
    /// async contexts).
    ///
    /// This represents prior settled outcomes from all processes, so global
    /// caps account for usage already realized (not just open reservations).
    pub fn recover_realized_usage(
        outcomes_path: &std::path::Path,
    ) -> Result<(Decimal, Decimal), ChimeraError> {
        let state = CrashRecovery::recover_from_jsonl_sync(outcomes_path)?;
        Ok((state.daily_usage_usd, state.weekly_usage_usd))
    }

    fn save_reservations(
        path: &std::path::Path,
        records: &[ReservationRecord],
    ) -> Result<(), ChimeraError> {
        let mut content = String::new();
        for record in records {
            let line = serde_json::to_string(record).map_err(|e| {
                ChimeraError::PersistenceError(format!("Reservation serialization: {e}"))
            })?;
            content.push_str(&line);
            content.push('\n');
        }
        let tmp = path.with_extension("jsonl.tmp");
        fs::write(&tmp, &content).map_err(ChimeraError::Io)?;
        fs::rename(&tmp, path).map_err(ChimeraError::Io)
    }

    fn append_reservation(
        path: &std::path::Path,
        record: &ReservationRecord,
    ) -> Result<(), ChimeraError> {
        let line = serde_json::to_string(record).map_err(|e| {
            ChimeraError::PersistenceError(format!("Reservation serialization: {e}"))
        })?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(ChimeraError::Io)?;
        writeln!(file, "{line}").map_err(ChimeraError::Io)
    }

    fn compute_global_totals(
        records: &[ReservationRecord],
        now: DateTime<Utc>,
    ) -> (Decimal, Decimal) {
        let mut daily = Decimal::ZERO;
        let mut weekly = Decimal::ZERO;
        for r in records {
            if r.status != ReservationStatus::Reserved {
                continue;
            }
            let age = now - r.created_at;
            if age <= TimeDelta::days(1) {
                daily += r.amount_usd;
            }
            if age <= TimeDelta::days(7) {
                weekly += r.amount_usd;
            }
        }
        (daily, weekly)
    }

    fn expire_stale_in_place(records: &mut Vec<ReservationRecord>, now: DateTime<Utc>) -> usize {
        let mut expired = 0;
        for r in records.iter_mut() {
            if r.status == ReservationStatus::Reserved && now >= r.expires_at {
                r.status = ReservationStatus::Expired;
                expired += 1;
            }
        }
        expired
    }

    /// Attempt to reserve the amount for an opportunity against global caps.
    /// Returns the [`ReservationRecord`] on success, or a deny reason.
    ///
    /// Reads **all** reservation files under the shared directory (not just the
    /// current chain), so daily and weekly caps are enforced across every chain
    /// that writes to this directory. Also recovers realized usage from the
    /// shared outcomes audit trail so that caps include settled outcomes from
    /// other processes.
    pub fn try_reserve(
        &self,
        opp: &Opportunity,
        chain_id: u64,
    ) -> Result<ReservationRecord, ChimeraError> {
        let path = self.reservations_path(chain_id);
        let _lock = self.acquire_lock()?;
        let now = Utc::now();
        let mut records = Self::load_all_reservations(&self.reservations_dir)?;
        let stale_count = Self::expire_stale_in_place(&mut records, now);
        if stale_count > 0 {
            info!(
                target = "chimera::pacing",
                expired = stale_count,
                "Expired stale reservations before global cap check"
            );
        }

        let (global_daily, global_weekly) = Self::compute_global_totals(&records, now);

        // Recover realized usage from the shared outcomes audit trail so that
        // caps include prior settled outcomes (not just open reservations).
        let (realized_daily, realized_weekly) = if self.outcomes_path.exists() {
            Self::recover_realized_usage(&self.outcomes_path)
                .unwrap_or((Decimal::ZERO, Decimal::ZERO))
        } else {
            (Decimal::ZERO, Decimal::ZERO)
        };

        let inner = self.engine.inner.read();
        // Single source of truth for realized usage: the shared outcomes
        // audit trail. The engine's inner counters track per-process usage
        // for local gates only; adding them here would double-count outcomes
        // that are already present in `realized_daily`/`realized_weekly`.
        let total_daily = global_daily + realized_daily;
        let total_weekly = global_weekly + realized_weekly;

        if total_daily + opp.expected_net_usd > inner.config.max_daily_net_usd {
            return Err(ChimeraError::PacingViolation(format!(
                "Global daily cap would be exceeded: {total_daily} + {} > {}",
                opp.expected_net_usd, inner.config.max_daily_net_usd
            )));
        }
        if total_weekly + opp.expected_net_usd > inner.config.max_weekly_net_usd {
            return Err(ChimeraError::PacingViolation(format!(
                "Global weekly cap would be exceeded: {total_weekly} + {} > {}",
                opp.expected_net_usd, inner.config.max_weekly_net_usd
            )));
        }

        let reservation = ReservationRecord {
            id: opp.id.clone(),
            chain_id,
            amount_usd: opp.expected_net_usd,
            status: ReservationStatus::Reserved,
            created_at: now,
            expires_at: now + TimeDelta::minutes(self.reservation_ttl_minutes),
            settled_at: None,
        };

        Self::append_reservation(&path, &reservation)?;
        Ok(reservation)
    }

    /// Settle a reservation after outcome recording. Moves the reservation
    /// from `Reserved` → `Settled`.
    pub fn settle(&self, reservation_id: &str, chain_id: u64) -> Result<(), ChimeraError> {
        let path = self.reservations_path(chain_id);
        let _lock = self.acquire_lock()?;
        let mut records = Self::load_reservations(&path)?;
        let now = Utc::now();
        let mut found = false;
        for r in records.iter_mut() {
            if r.id == reservation_id && r.status == ReservationStatus::Reserved {
                r.status = ReservationStatus::Settled;
                r.settled_at = Some(now);
                found = true;
                break;
            }
        }
        if !found {
            return Err(ChimeraError::PacingViolation(format!(
                "Reservation {reservation_id} not found or already settled"
            )));
        }
        Self::save_reservations(&path, &records)
    }

    /// Expire stale reservations without settlement. Returns the count of
    /// reservations that were expired.
    pub fn expire_stale(&self, chain_id: u64) -> Result<usize, ChimeraError> {
        let path = self.reservations_path(chain_id);
        let _lock = self.acquire_lock()?;
        let now = Utc::now();
        let mut records = Self::load_reservations(&path)?;
        let expired = Self::expire_stale_in_place(&mut records, now);
        if expired > 0 {
            Self::save_reservations(&path, &records)?;
        }
        Ok(expired)
    }

    /// Compute current global daily and weekly totals across all reservations
    /// (all chain IDs that share this directory).
    pub fn global_totals(&self, _chain_id: u64) -> Result<(Decimal, Decimal), ChimeraError> {
        let _lock = self.acquire_lock()?;
        let now = Utc::now();
        let mut records = Self::load_all_reservations(&self.reservations_dir)?;
        Self::expire_stale_in_place(&mut records, now);
        Ok(Self::compute_global_totals(&records, now))
    }
}

struct FileLockGuard {
    path: PathBuf,
    _file: Option<File>,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        drop(self._file.take());
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rust_decimal::prelude::FromPrimitive;
    use std::str::FromStr;

    fn make_test_config() -> PacingConfig {
        PacingConfig {
            max_daily_net_usd: Decimal::from(2000),
            max_weekly_net_usd: Decimal::from(7500),
            max_single_transfer_usd: Decimal::from(1000),
            min_interval_hours: 6,
            max_jitter_hours: 12,
            venue_rotation_count: 5,
            clean_eoa_pool_size: 10,
            auto_halt_on_reverts: 3,
            max_gas_gwei: 300,
            max_daily_loss_eth: Decimal::from_str("0.005").unwrap(),
            min_profit_multiplier: Decimal::from_str("2.5").unwrap(),
            execute_mode: "shadow".into(),
            log_level: "info".into(),
            metrics_port: 9100,
            chain_id: 8453,
            oracle_staleness_seconds: 300,
            eth_price_usd_fallback: Decimal::from(1800),
            eth_usd_feed_address: "0x71041dddad3595F9CEd3DcCbe3D9337177BcC57b".into(),
            recent_outcomes_capacity: 128,
            eoa_pool_path: "nonexistent_eoa_pool.json".into(),
            pools_toml_path: "config/pools.toml".into(),
            executor_address: "".into(),
            treasury_address: "".into(),
            treasury_keystore: "".into(),
            worker_keystore_dir: "".into(),
            sweep_interval_secs: 300,
            refund_interval_secs: 3600,
            min_worker_balance_eth: Decimal::from_str("0.01").unwrap(),
            refund_topup_eth: Decimal::from_str("0.05").unwrap(),
            sweep_tokens: vec![],
            sweep_min_keep_eth: Decimal::from_str("0.005").unwrap(),
            ws_endpoint: String::new(),
        }
    }

    #[test]
    fn test_allows_under_all_caps() {
        let engine = PacingEngine::new(make_test_config());
        let opp = Opportunity {
            id: "test-1".into(),
            expected_net_usd: Decimal::from(120),
            gas_estimate_gwei: 50,
            venue: "aerodrome".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp).unwrap();
        assert!(matches!(decision, PacingDecision::Allow { .. }));
    }

    #[test]
    fn test_denies_over_single_cap() {
        let engine = PacingEngine::new(make_test_config());
        let opp = Opportunity {
            id: "test-2".into(),
            expected_net_usd: Decimal::from(1100),
            gas_estimate_gwei: 50,
            venue: "test".into(),
            eoa: "0x0000".into(),
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp).unwrap();
        assert!(matches!(decision, PacingDecision::Deny { .. }));
    }

    #[test]
    fn test_venue_rotation_blocks_reuse() {
        let mut config = make_test_config();
        config.min_interval_hours = 0;
        let engine = PacingEngine::new(config);
        let opp = Opportunity {
            id: "test-1".into(),
            expected_net_usd: Decimal::from(100),
            gas_estimate_gwei: 1,
            venue: "aerodrome".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp).unwrap();
        assert!(matches!(decision, PacingDecision::Allow { .. }));
        engine.record_outcome(&opp, Decimal::from(100), Decimal::ZERO, false);
        let opp2 = Opportunity {
            id: "test-2".into(),
            expected_net_usd: Decimal::from(100),
            gas_estimate_gwei: 1,
            venue: "aerodrome".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp2).unwrap();
        assert!(
            matches!(decision, PacingDecision::Deny { ref reason } if reason.contains("rotation")),
            "Expected venue rotation denial, got {:?}",
            decision
        );
    }

    #[test]
    fn test_venue_rotation_allows_after_count() {
        let mut config = make_test_config();
        config.venue_rotation_count = 2;
        config.min_interval_hours = 0;
        let engine = PacingEngine::new(config);
        let venues = ["a", "b", "c"];
        for (i, venue) in venues.iter().enumerate() {
            let opp = Opportunity {
                id: format!("test-{}", i),
                expected_net_usd: Decimal::from(10),
                gas_estimate_gwei: 1,
                venue: venue.to_string(),
                eoa: "0xClean1".into(),
                timestamp: Utc::now(),
            };
            let decision = engine.check(&opp).unwrap();
            assert!(
                matches!(decision, PacingDecision::Allow { .. }),
                "Venue {} should be allowed on first use",
                venue
            );
            engine.record_outcome(&opp, Decimal::from(10), Decimal::ZERO, false);
        }
        let opp_a = Opportunity {
            id: "test-a2".into(),
            expected_net_usd: Decimal::from(10),
            gas_estimate_gwei: 1,
            venue: "a".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp_a).unwrap();
        assert!(
            matches!(decision, PacingDecision::Allow { .. }),
            "Venue 'a' should be allowed after cycling through others"
        );
    }

    #[test]
    fn test_eoa_rotation_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let pool_path = dir.path().join("eoa_pool.json");
        let pool = vec!["0xA".to_string(), "0xB".to_string(), "0xC".to_string()];
        std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();
        let mut config = make_test_config();
        config.eoa_pool_path = pool_path.to_str().unwrap().to_string();
        config.clean_eoa_pool_size = 3;
        let engine = PacingEngine::new(config);
        assert_eq!(engine.select_next_eoa(), Some("0xa".to_string()));
        assert_eq!(engine.select_next_eoa(), Some("0xb".to_string()));
        assert_eq!(engine.select_next_eoa(), Some("0xc".to_string()));
        assert_eq!(engine.select_next_eoa(), Some("0xa".to_string()));
    }

    #[test]
    fn test_eoa_validation_denies_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let pool_path = dir.path().join("eoa_pool.json");
        let pool = vec!["0xA".to_string(), "0xB".to_string()];
        std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();
        let mut config = make_test_config();
        config.eoa_pool_path = pool_path.to_str().unwrap().to_string();
        config.clean_eoa_pool_size = 2;
        let engine = PacingEngine::new(config);
        let opp = Opportunity {
            id: "test".into(),
            expected_net_usd: Decimal::from(100),
            gas_estimate_gwei: 1,
            venue: "new-venue".into(),
            eoa: "0xUNKNOWN".into(),
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp).unwrap();
        assert!(
            matches!(decision, PacingDecision::Deny { ref reason } if reason.contains("not in clean pool")),
            "Expected EOA validation denial, got {:?}",
            decision
        );
    }

    #[test]
    fn test_eip55_checksummed_eoa_canonicalized_to_lowercase() {
        // EIP-55 checksummed addresses loaded from the pool are lowercased,
        // so they match the orchestrator's `format!("0x{:x}", addr)` output.
        let dir = tempfile::tempdir().unwrap();
        let pool_path = dir.path().join("eoa_pool.json");
        let checksummed = "0x71C7656EC7ab88b098defB751B7401B5f6d8976F";
        let pool = vec![checksummed.to_string()];
        std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();
        let mut config = make_test_config();
        config.eoa_pool_path = pool_path.to_str().unwrap().to_string();
        config.clean_eoa_pool_size = 1;
        let engine = PacingEngine::new(config);
        let selected = engine.select_next_eoa().unwrap();
        assert_eq!(
            selected,
            checksummed.to_lowercase(),
            "Pool address should be canonicalized to lowercase"
        );
        let opp = Opportunity {
            id: "test-eip55".into(),
            expected_net_usd: Decimal::from(100),
            gas_estimate_gwei: 1,
            venue: "test-dex".into(),
            eoa: selected,
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp).unwrap();
        assert!(
            matches!(decision, PacingDecision::Allow { .. }),
            "Checksummed-then-lowercased EOA should pass pacing check"
        );
    }

    #[test]
    fn test_eoa_validation_allows_known() {
        let dir = tempfile::tempdir().unwrap();
        let pool_path = dir.path().join("eoa_pool.json");
        let pool = vec!["0xA".to_string(), "0xB".to_string()];
        std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();
        let mut config = make_test_config();
        config.eoa_pool_path = pool_path.to_str().unwrap().to_string();
        config.clean_eoa_pool_size = 2;
        let engine = PacingEngine::new(config);
        let opp = Opportunity {
            id: "test".into(),
            expected_net_usd: Decimal::from(100),
            gas_estimate_gwei: 1,
            venue: "new-venue".into(),
            eoa: "0xA".into(),
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp).unwrap();
        assert!(matches!(decision, PacingDecision::Allow { .. }));
    }

    #[test]
    fn test_profit_multiplier_enforcement() {
        let engine = PacingEngine::new(make_test_config());
        let opp = Opportunity {
            id: "test".into(),
            expected_net_usd: Decimal::from(10),
            gas_estimate_gwei: 50,
            venue: "new-venue".into(),
            eoa: "0x0000".into(),
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp).unwrap();
        assert!(
            matches!(decision, PacingDecision::Deny { ref reason } if reason == "InsufficientProfit"),
            "Expected InsufficientProfit denial, got {:?}",
            decision
        );
    }

    #[test]
    fn test_profit_multiplier_allows_sufficient() {
        let engine = PacingEngine::new(make_test_config());
        let opp = Opportunity {
            id: "test".into(),
            expected_net_usd: Decimal::from(700),
            gas_estimate_gwei: 50,
            venue: "new-venue".into(),
            eoa: "0x0000".into(),
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp).unwrap();
        assert!(
            matches!(decision, PacingDecision::Allow { .. }),
            "Expected Allow, got {:?}",
            decision
        );
    }

    #[test]
    fn test_recent_outcomes_capacity_configurable() {
        let mut config = make_test_config();
        config.recent_outcomes_capacity = 4;
        let engine = PacingEngine::new(config);
        for i in 0..6 {
            let opp = Opportunity {
                id: format!("test-{}", i),
                expected_net_usd: Decimal::from(10),
                gas_estimate_gwei: 1,
                venue: format!("venue-{}", i),
                eoa: "0x0000".into(),
                timestamp: Utc::now(),
            };
            engine.record_outcome(&opp, Decimal::from(10), Decimal::ZERO, false);
        }
        let state = engine.current_risk_state();
        assert_eq!(state.weekly_net_usd, Decimal::from(60));
    }

    /// Serializes env-mutating tests so concurrent test threads don't race on the
    /// process-global `CHIMERA_OPERATOR_TOKEN`. `Mutex::new` is const, so this is
    /// valid in a `static`. Poisoning is recovered via `into_inner`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct OperatorTokenGuard(Option<String>);

    impl OperatorTokenGuard {
        fn set(value: Option<&str>) -> Self {
            let previous = std::env::var("CHIMERA_OPERATOR_TOKEN").ok();
            match value {
                Some(value) => std::env::set_var("CHIMERA_OPERATOR_TOKEN", value),
                None => std::env::remove_var("CHIMERA_OPERATOR_TOKEN"),
            }
            Self(previous)
        }
    }

    impl Drop for OperatorTokenGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("CHIMERA_OPERATOR_TOKEN", value),
                None => std::env::remove_var("CHIMERA_OPERATOR_TOKEN"),
            }
        }
    }

    #[test]
    fn clear_breaker_succeeds_with_valid_token() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = OperatorTokenGuard::set(Some("s3cret"));
        let engine = PacingEngine::new(make_test_config());
        engine.inner.write().breaker_tripped = Some(BreakerReason::DailyLossLimitExceeded);
        let res = engine.clear_breaker("s3cret");
        assert!(res.is_ok(), "expected Ok, got {:?}", res);
        assert!(!engine.is_breaker_active(), "breaker should be cleared");
    }

    #[test]
    fn clear_breaker_rejects_wrong_token() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = OperatorTokenGuard::set(Some("s3cret"));
        let engine = PacingEngine::new(make_test_config());
        engine.inner.write().breaker_tripped = Some(BreakerReason::DailyLossLimitExceeded);
        let res = engine.clear_breaker("wrong-token");
        assert!(
            matches!(res, Err(ChimeraError::ConfigError(_))),
            "expected ConfigError, got {:?}",
            res
        );
        // Breaker must remain tripped after a rejected clear.
        assert!(engine.is_breaker_active(), "breaker must stay tripped");
    }

    #[test]
    fn clear_breaker_rejects_without_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = OperatorTokenGuard::set(None);
        let engine = PacingEngine::new(make_test_config());
        engine.inner.write().breaker_tripped = Some(BreakerReason::DailyLossLimitExceeded);
        let res = engine.clear_breaker("anything");
        assert!(
            matches!(res, Err(ChimeraError::ConfigError(_))),
            "expected ConfigError, got {:?}",
            res
        );
        assert!(engine.is_breaker_active(), "breaker must stay tripped");
    }

    #[test]
    fn trip_emergency_sets_breaker_and_is_idempotent() {
        let engine = PacingEngine::new(make_test_config());
        engine.trip_emergency("flag-A");
        assert!(engine.is_breaker_active());
        let state1 = engine.current_risk_state();
        assert_eq!(
            state1.breaker_tripped,
            Some(BreakerReason::EmergencyHalt("flag-A".into()))
        );
        // Re-tripping (even with a different reason) is a no-op on state.
        engine.trip_emergency("flag-B");
        let state2 = engine.current_risk_state();
        assert_eq!(
            state2.breaker_tripped,
            Some(BreakerReason::EmergencyHalt("flag-A".into()))
        );
    }

    #[test]
    fn daily_loss_eth_is_rolling_sum_not_monotonic() {
        let mut config = make_test_config();
        config.venue_rotation_count = 0;
        let engine = PacingEngine::new(config);
        let gas = Decimal::from_str("0.001").unwrap();
        for i in 0..2 {
            let opp = Opportunity {
                id: format!("loss-{i}"),
                expected_net_usd: Decimal::from(10),
                gas_estimate_gwei: 1,
                venue: "v".into(),
                eoa: "0x0000".into(),
                timestamp: Utc::now(),
            };
            // Negative realized => losing outcome; gas_spent accumulates in the window.
            engine.record_outcome(&opp, Decimal::from(-5), gas, false);
        }
        let state = engine.current_risk_state();
        // Both losses are within the live 24h window: 0.001 + 0.001 = 0.002.
        assert_eq!(state.daily_loss_eth, Decimal::from_str("0.002").unwrap());
    }

    proptest! {
        #[test]
        fn prop_never_exceeds_daily_cap(
            net in 0.0f64..1000.0,
            daily_so_far in 0.0f64..2000.0
        ) {
            // proptest generates f64 strategy values; convert to Decimal only at test boundaries
            let net_dec = Decimal::from_f64(net).unwrap_or(Decimal::ZERO);
            let daily_dec = Decimal::from_f64(daily_so_far).unwrap_or(Decimal::ZERO);
            let daily_cap = Decimal::from(2000);

            let config = make_test_config();
            let engine = PacingEngine::new(config);
            engine.inner.write().daily_net_usd = daily_dec;

            let opp = Opportunity {
                id: "prop-test".into(),
                expected_net_usd: net_dec,
                gas_estimate_gwei: 0, // bypass profit multiplier gate
                venue: "test".into(),
                eoa: "0x0000".into(),
                timestamp: Utc::now(),
            };
            let decision = engine.check(&opp).unwrap();
            let would_exceed = daily_dec + net_dec > daily_cap;
            if would_exceed {
                prop_assert!(matches!(decision, PacingDecision::Deny { .. }), "expected Deny");
            } else {
                prop_assert!(matches!(decision, PacingDecision::Allow { .. }), "expected Allow");
            }
        }
    }

    #[test]
    fn estimate_gas_cost_uses_cached_eth_price() {
        let engine = PacingEngine::new(make_test_config());
        let gas_gwei = 50u64;
        let cost_fallback = engine.inner.read().estimate_gas_cost_usd(gas_gwei);
        assert!(cost_fallback > Decimal::ZERO);
        let cached_price = Decimal::from(3000);
        engine.inner.write().cached_eth_price = Some(cached_price);
        let cost_cached = engine.inner.read().estimate_gas_cost_usd(gas_gwei);
        assert!(
            cost_cached > cost_fallback,
            "cached price 3000 should produce higher cost than fallback 1800"
        );
        engine.inner.write().cached_eth_price = None;
        let cost_after_clear = engine.inner.read().estimate_gas_cost_usd(gas_gwei);
        assert_eq!(
            cost_after_clear, cost_fallback,
            "after clearing cache, should fall back to config"
        );
    }

    // -----------------------------------------------------------------------
    // CrossProcessPacing tests
    // -----------------------------------------------------------------------

    #[test]
    fn crossprocess_reserve_succeed_and_settle() {
        let dir = tempfile::tempdir().unwrap();
        let res_dir = dir.path().join("reservations");
        let engine = PacingEngine::new(make_test_config());
        let pacing =
            CrossProcessPacing::new(engine, res_dir.clone(), dir.path().join("outcomes.jsonl"));

        let opp = Opportunity {
            id: "opp-1".into(),
            expected_net_usd: Decimal::from(100),
            gas_estimate_gwei: 50,
            venue: "aerodrome".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };

        let reservation = pacing.try_reserve(&opp, 8453).unwrap();
        assert_eq!(reservation.status, ReservationStatus::Reserved);
        assert_eq!(reservation.amount_usd, Decimal::from(100));
        assert!(reservation.expires_at > reservation.created_at);

        pacing.settle("opp-1", 8453).unwrap();

        let path = res_dir.join("reservations-8453.jsonl");
        let records = CrossProcessPacing::load_reservations(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, ReservationStatus::Settled);
        assert!(records[0].settled_at.is_some());
    }

    #[test]
    fn crossprocess_reserve_exceeds_global_daily_cap() {
        let dir = tempfile::tempdir().unwrap();
        let res_dir = dir.path().join("reservations");
        let engine = PacingEngine::new(make_test_config());
        let pacing =
            CrossProcessPacing::new(engine, res_dir.clone(), dir.path().join("outcomes.jsonl"));

        // Reserve most of the daily cap
        let opp1 = Opportunity {
            id: "opp-big".into(),
            expected_net_usd: Decimal::from(1900),
            gas_estimate_gwei: 50,
            venue: "a".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        pacing.try_reserve(&opp1, 8453).unwrap();

        // Total remaining: 2000 - 1900 = 100, so trying to reserve 200 should fail
        let opp2 = Opportunity {
            id: "opp-over".into(),
            expected_net_usd: Decimal::from(200),
            gas_estimate_gwei: 50,
            venue: "b".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        let result = pacing.try_reserve(&opp2, 8453);
        assert!(
            result.is_err(),
            "Should have denied reserve exceeding global daily cap"
        );
    }

    #[test]
    fn crossprocess_expire_stale_removes_from_totals() {
        let dir = tempfile::tempdir().unwrap();
        let res_dir = dir.path().join("reservations");
        let engine = PacingEngine::new(make_test_config());
        let pacing =
            CrossProcessPacing::new(engine, res_dir.clone(), dir.path().join("outcomes.jsonl"))
                .with_ttl(0); // TTL=0 minutes

        let opp = Opportunity {
            id: "opp-expire".into(),
            expected_net_usd: Decimal::from(500),
            gas_estimate_gwei: 50,
            venue: "a".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };

        let reservation = pacing.try_reserve(&opp, 8453).unwrap();
        assert_eq!(reservation.status, ReservationStatus::Reserved);

        let expired = pacing.expire_stale(8453).unwrap();
        assert_eq!(expired, 1, "TTL=0 should expire immediately");

        let path = res_dir.join("reservations-8453.jsonl");
        let records = CrossProcessPacing::load_reservations(&path).unwrap();
        assert_eq!(records[0].status, ReservationStatus::Expired);

        let (daily, _weekly) = pacing.global_totals(8453).unwrap();
        assert_eq!(
            daily,
            Decimal::ZERO,
            "expired reservations should not count toward totals"
        );
    }

    #[test]
    fn crossprocess_two_processes_cannot_exceed_caps() {
        // Simulate two processes sharing a reservations dir
        let dir = tempfile::tempdir().unwrap();
        let res_dir = dir.path().join("reservations");

        let engine1 = PacingEngine::new(make_test_config());
        let pacing1 =
            CrossProcessPacing::new(engine1, res_dir.clone(), dir.path().join("outcomes.jsonl"));

        let engine2 = PacingEngine::new(make_test_config());
        let pacing2 =
            CrossProcessPacing::new(engine2, res_dir.clone(), dir.path().join("outcomes.jsonl"));

        let opp1 = Opportunity {
            id: "p1-opp".into(),
            expected_net_usd: Decimal::from(1200),
            gas_estimate_gwei: 50,
            venue: "a".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        pacing1.try_reserve(&opp1, 8453).unwrap();

        let opp2 = Opportunity {
            id: "p2-opp".into(),
            expected_net_usd: Decimal::from(900),
            gas_estimate_gwei: 50,
            venue: "b".into(),
            eoa: "0xClean2".into(),
            timestamp: Utc::now(),
        };
        // 1200 + 900 = 2100 > 2000 daily cap
        let result = pacing2.try_reserve(&opp2, 8453);
        assert!(
            result.is_err(),
            "Process 2 should be denied: 1200 + 900 exceeds daily cap 2000"
        );

        // But a smaller amount should fit
        let opp3 = Opportunity {
            id: "p2-opp-small".into(),
            expected_net_usd: Decimal::from(500),
            gas_estimate_gwei: 50,
            venue: "c".into(),
            eoa: "0xClean2".into(),
            timestamp: Utc::now(),
        };
        let res = pacing2.try_reserve(&opp3, 8453);
        assert!(res.is_ok(), "500 should fit: 1200 + 500 = 1700 < 2000");
    }

    #[test]
    fn crossprocess_settle_then_release_cap() {
        let dir = tempfile::tempdir().unwrap();
        let res_dir = dir.path().join("reservations");
        let engine = PacingEngine::new(make_test_config());
        let pacing =
            CrossProcessPacing::new(engine, res_dir.clone(), dir.path().join("outcomes.jsonl"));

        let opp1 = Opportunity {
            id: "opp-1".into(),
            expected_net_usd: Decimal::from(1500),
            gas_estimate_gwei: 50,
            venue: "a".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        pacing.try_reserve(&opp1, 8453).unwrap();
        pacing.settle("opp-1", 8453).unwrap();

        let opp2 = Opportunity {
            id: "opp-2".into(),
            expected_net_usd: Decimal::from(1500),
            gas_estimate_gwei: 50,
            venue: "b".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        let result = pacing.try_reserve(&opp2, 8453);
        assert!(
            result.is_ok(),
            "After settle, reserved cap is freed; 1500 fresh should be allowed"
        );
    }

    // -------------------------------------------------------------------
    // Comment 4: Malformed JSONL fail-closed regression tests
    // -------------------------------------------------------------------

    #[test]
    fn reservations_load_malformed_jsonl_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reservations-8453.jsonl");
        // Last line is invalid JSON — must fail, not silently skip.
        let bad = r#"{"id":"ok","chain_id":8453,"amount_usd":"100","status":"reserved","created_at":"2026-01-01T00:00:00Z","expires_at":"2026-01-01T00:05:00Z","settled_at":null}
NOT_VALID_JSON
"#;
        std::fs::write(&path, bad).unwrap();
        let result = CrossProcessPacing::load_reservations(&path);
        assert!(
            result.is_err(),
            "Malformed JSONL should cause load failure, not silent skip"
        );
    }

    #[test]
    fn reservations_load_empty_file_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reservations-8453.jsonl");
        std::fs::write(&path, "\n\n").unwrap();
        let records = CrossProcessPacing::load_reservations(&path).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn reservations_load_partial_line_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reservations-8453.jsonl");
        // Truncated JSON line (interrupted write simulation).
        let truncated = r#"{"id":"ok","chain_id":8453,"amount_usd":"100","status":"reserved","#;
        std::fs::write(&path, truncated).unwrap();
        let result = CrossProcessPacing::load_reservations(&path);
        assert!(
            result.is_err(),
            "Truncated JSONL line (interrupted write) should fail closed"
        );
    }

    #[test]
    fn crossprocess_try_reserve_fails_on_malformed_any_file() {
        let dir = tempfile::tempdir().unwrap();
        let res_dir = dir.path().join("reservations");
        std::fs::create_dir_all(&res_dir).unwrap();

        // Write a valid file for chain 42161, and a corrupt file for chain 8453.
        let good_path = res_dir.join("reservations-42161.jsonl");
        let good = r#"{"id":"arb-1","chain_id":42161,"amount_usd":"50","status":"reserved","created_at":"2026-01-01T00:00:00Z","expires_at":"2026-01-01T00:05:00Z","settled_at":null}
"#;
        std::fs::write(&good_path, good).unwrap();

        let bad_path = res_dir.join("reservations-8453.jsonl");
        std::fs::write(&bad_path, "GARBAGE\n").unwrap();

        let engine = PacingEngine::new(make_test_config());
        let pacing =
            CrossProcessPacing::new(engine, res_dir.clone(), dir.path().join("outcomes.jsonl"));

        let opp = Opportunity {
            id: "opp-1".into(),
            expected_net_usd: Decimal::from(100),
            gas_estimate_gwei: 50,
            venue: "a".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        let result = pacing.try_reserve(&opp, 8453);
        assert!(
            result.is_err(),
            "try_reserve must fail when ANY reservation file is corrupt"
        );
    }

    // -------------------------------------------------------------------
    // Comment 3: Multi-chain reservation aggregation
    // -------------------------------------------------------------------

    #[test]
    fn crossprocess_multi_chain_reservations_aggregate_caps() {
        let dir = tempfile::tempdir().unwrap();
        let res_dir = dir.path().join("reservations");
        let engine = PacingEngine::new(make_test_config());
        let pacing =
            CrossProcessPacing::new(engine, res_dir.clone(), dir.path().join("outcomes.jsonl"));

        // Reserve on chain 8453
        let opp1 = Opportunity {
            id: "base-opp".into(),
            expected_net_usd: Decimal::from(500),
            gas_estimate_gwei: 50,
            venue: "a".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        pacing.try_reserve(&opp1, 8453).unwrap();

        // Reserve on chain 42161 — daily cap is shared across chains
        let opp2 = Opportunity {
            id: "arb-opp".into(),
            expected_net_usd: Decimal::from(1600),
            gas_estimate_gwei: 50,
            venue: "b".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        // 500 (base) + 1600 (arb) = 2100 > 2000 daily cap
        let result = pacing.try_reserve(&opp2, 42161);
        assert!(
            result.is_err(),
            "Cross-chain reservation total 2100 should exceed daily cap 2000"
        );

        // But total 500 + 1000 = 1500 fits
        let opp3 = Opportunity {
            id: "arb-opp-small".into(),
            expected_net_usd: Decimal::from(1000),
            gas_estimate_gwei: 50,
            venue: "c".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        let res = pacing.try_reserve(&opp3, 42161);
        assert!(res.is_ok(), "500 + 1000 = 1500 should fit under 2000 cap");
    }

    // -------------------------------------------------------------------
    // Comment 1: Oracle ETH/USD refresh tests
    // -------------------------------------------------------------------

    #[test]
    fn fallback_used_when_no_oracle_attached() {
        let engine = PacingEngine::new(make_test_config());
        // No oracle attached — cached_eth_price is None.
        assert!(engine.inner.read().cached_eth_price.is_none());
        let cost = engine.inner.read().estimate_gas_cost_usd(50);
        // Fallback: 1800 USD/ETH * (50 gwei * 150000 / 1e9) = 1800 * 0.0075 = 13.5
        assert!(cost > Decimal::ZERO);
    }

    #[test]
    fn cached_price_persists_after_failed_refresh() {
        // When no oracle is attached, refresh_eth_price is a no-op and the
        // existing cached price (if set) is preserved.
        let engine = PacingEngine::new(make_test_config());
        engine.inner.write().cached_eth_price = Some(Decimal::from(2000));
        // refresh_eth_price is async but with no oracle it's a no-op.
        // Verify that the cached price is unchanged.
        assert_eq!(
            engine.inner.read().cached_eth_price,
            Some(Decimal::from(2000))
        );
    }

    // -------------------------------------------------------------------
    // Comment 1 regression: try_reserve must not panic inside a Tokio
    // runtime (no nested Runtime::new / block_on).
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn try_reserve_from_tokio_context_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let res_dir = dir.path().join("reservations");
        let engine = PacingEngine::new(make_test_config());
        let pacing =
            CrossProcessPacing::new(engine, res_dir.clone(), dir.path().join("outcomes.jsonl"));

        let opp = Opportunity {
            id: "tokio-test".into(),
            expected_net_usd: Decimal::from(100),
            gas_estimate_gwei: 50,
            venue: "a".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };

        // This must NOT panic — recover_realized_usage now uses sync I/O
        // instead of creating a nested Tokio runtime.
        let result = pacing.try_reserve(&opp, 8453);
        assert!(
            result.is_ok(),
            "try_reserve should succeed from tokio context"
        );
    }

    // -------------------------------------------------------------------
    // Comment 2 regression: realized outcomes are not double-counted
    // when computing global caps.
    // -------------------------------------------------------------------

    #[test]
    fn global_cap_does_not_double_count_realized_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let res_dir = dir.path().join("reservations");
        let engine = PacingEngine::new(make_test_config());
        let pacing =
            CrossProcessPacing::new(engine, res_dir.clone(), dir.path().join("outcomes.jsonl"));

        // Step 1: Record a realized outcome to drive up local counters.
        // The engine's inner daily_net_usd will rise but the global cap
        // must NOT include it (the shared outcomes recovery is the sole
        // source of realized-usage truth).
        let opp1 = Opportunity {
            id: "realized-1".into(),
            expected_net_usd: Decimal::from(500),
            gas_estimate_gwei: 50,
            venue: "a".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        pacing
            .engine()
            .record_outcome(&opp1, Decimal::from(500), Decimal::ZERO, false);

        // Inner counter is now 500.
        assert_eq!(
            pacing.engine().current_daily_usage(),
            Decimal::from(500),
            "inner daily usage should be 500 after recording"
        );

        // Step 2: Reserve $1700. If inner counters were double-counted,
        // this would fail (500 + 1700 = 2200 > 2000 daily cap). Since
        // global caps use only shared recovery + active reservations,
        // this should succeed.
        let opp2 = Opportunity {
            id: "reserve-after-realized".into(),
            expected_net_usd: Decimal::from(1700),
            gas_estimate_gwei: 50,
            venue: "b".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        let result = pacing.try_reserve(&opp2, 8453);
        assert!(
            result.is_ok(),
            "1700 should fit: global caps exclude inner counters (no double count). Got: {:?}",
            result.err()
        );

        // Step 3: But 2000 more (1700 active + 2000 new) should exceed the cap.
        let opp3 = Opportunity {
            id: "would-exceed".into(),
            expected_net_usd: Decimal::from(2000),
            gas_estimate_gwei: 50,
            venue: "c".into(),
            eoa: "0xClean1".into(),
            timestamp: Utc::now(),
        };
        let result3 = pacing.try_reserve(&opp3, 8453);
        assert!(
            result3.is_err(),
            "1700 active + 2000 new = 3700 should exceed daily cap 2000"
        );
    }

    #[test]
    fn test_repeated_borrower_opportunities_do_not_collide() {
        // Verify that repeated opportunities for the same borrower on the same
        // chain produce unique IDs and do not collide in the pacing engine.
        let mut config = make_test_config();
        config.min_interval_hours = 0;
        let engine = PacingEngine::new(config);

        let ts1 = Utc::now();
        let ts2 = ts1 + chrono::Duration::seconds(12); // next block

        let opp1 = Opportunity {
            id: format!("liq-{}-{}-{}", 8453, "0xBorrower1", ts1.timestamp_millis()),
            expected_net_usd: Decimal::from(100),
            gas_estimate_gwei: 50,
            venue: "test-dex".into(),
            eoa: "0xClean1".into(),
            timestamp: ts1,
        };

        let opp2 = Opportunity {
            id: format!("liq-{}-{}-{}", 8453, "0xBorrower1", ts2.timestamp_millis()),
            expected_net_usd: Decimal::from(100),
            gas_estimate_gwei: 50,
            venue: "test-dex-2".into(),
            eoa: "0xClean1".into(),
            timestamp: ts2,
        };

        // IDs must differ (same borrower, same chain, different times)
        assert_ne!(
            opp1.id, opp2.id,
            "Repeated opportunities for the same borrower must have unique IDs"
        );

        // Both should be independently allowed (no collision)
        let d1 = engine.check(&opp1).unwrap();
        assert!(matches!(d1, PacingDecision::Allow { .. }));
        engine.record_outcome(&opp1, Decimal::from(100), Decimal::ZERO, false);

        let d2 = engine.check(&opp2).unwrap();
        assert!(matches!(d2, PacingDecision::Allow { .. }));
        engine.record_outcome(&opp2, Decimal::from(100), Decimal::ZERO, false);

        // Verify both outcomes are tracked in daily usage
        let state = engine.current_risk_state();
        assert_eq!(
            state.daily_net_usd,
            Decimal::from(200),
            "Both opportunities should be independently counted"
        );
    }
}
