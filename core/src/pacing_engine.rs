//! Pacing Engine - The financial governor and risk-control layer for Chimera.
//! Pure logic (no I/O in hot path). Enforces every cap, jitter window, and breaker from the Executive Brief.
//! Called on EVERY candidate opportunity before any simulation or submission.
//! Post-outcome updates are also mandatory.
//!
//! SAFETY NOTES:
//! - All monetary values use `Decimal` (not `f64`) to avoid floating-point errors.
//! - The engine uses an internal `parking_lot::RwLock` for thread-safe access.
//! - State is persisted to JSONL on every `record_outcome` for crash-safe recovery.
use crate::state::{OutcomeRecord, StatePersistence};
use crate::{ChimeraError, PacingConfig};
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
    breaker_tripped: Option<BreakerReason>,
    venue_rotation: VecDeque<String>,
    eoa_rotation: VecDeque<String>,
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
        if !self.eoa_rotation.is_empty() && !self.eoa_rotation.contains(&opp.eoa) {
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
        gas_cost_eth * self.config.eth_price_usd_fallback
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
        if realized_net_usd < Decimal::ZERO {
            self.daily_loss_eth += gas_spent_eth;
        }
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
            breaker_tripped: state.breaker_tripped,
            venue_rotation: VecDeque::new(),
            eoa_rotation: eoa_pool,
        }
    }
}

/// Thread-safe, crash-safe pacing engine.
/// Uses internal RwLock for concurrent read access and exclusive write access.
pub struct PacingEngine {
    inner: Arc<RwLock<PacingEngineInner>>,
    state_path: Option<std::path::PathBuf>,
    state_persistence: Option<Arc<dyn StatePersistence + Send + Sync>>,
}
impl Clone for PacingEngine {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            state_path: self.state_path.clone(),
            state_persistence: self.state_persistence.clone(),
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
                    breaker_tripped: None,
                    venue_rotation: VecDeque::new(),
                    eoa_rotation: capped_pool,
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
                breaker_tripped: None,
                venue_rotation: VecDeque::new(),
                eoa_rotation: capped_pool,
            }
        };
        Self {
            inner: Arc::new(RwLock::new(inner)),
            state_path,
            state_persistence: None,
        }
    }
    pub fn with_state_persistence(
        mut self,
        persistence: Arc<dyn StatePersistence + Send + Sync>,
    ) -> Self {
        self.state_persistence = Some(persistence);
        self
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
                .filter_map(|v| v.as_str().map(str::to_string))
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
                wallet
                    .get("address")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
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
    pub fn clear_breaker(&self) {
        let mut inner = self.inner.write();
        if inner.breaker_tripped.is_some() {
            warn!("OPERATOR: Manually clearing breaker. Ensure root cause resolved.");
            inner.breaker_tripped = None;
            inner.consecutive_reverts = 0;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rust_decimal::prelude::FromPrimitive;
    use std::str::FromStr;

    fn make_test_config() -> PacingConfig {
        PacingConfig {
            max_daily_net_usd:       Decimal::from(2000),
            max_weekly_net_usd:      Decimal::from(7500),
            max_single_transfer_usd: Decimal::from(1000),
            min_interval_hours:      6,
            max_jitter_hours:        12,
            venue_rotation_count:    5,
            clean_eoa_pool_size:     10,
            auto_halt_on_reverts:    3,
            max_gas_gwei:            300,
            max_daily_loss_eth:      Decimal::from_str("0.005").unwrap(),
            min_profit_multiplier:   Decimal::from_str("2.5").unwrap(),
            execute_mode:            "shadow".into(),
            log_level:               "info".into(),
            metrics_port:            9100,
            chain_id:                8453,
            oracle_staleness_seconds: 300,
            eth_price_usd_fallback:  Decimal::from(1800),
            recent_outcomes_capacity: 128,
            eoa_pool_path:           "nonexistent_eoa_pool.json".into(),
            pools_toml_path:         "config/pools.toml".into(),
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
            "Expected venue rotation denial, got {:?}", decision
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
                "Venue {} should be allowed on first use", venue
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
        assert_eq!(engine.select_next_eoa(), Some("0xA".to_string()));
        assert_eq!(engine.select_next_eoa(), Some("0xB".to_string()));
        assert_eq!(engine.select_next_eoa(), Some("0xC".to_string()));
        assert_eq!(engine.select_next_eoa(), Some("0xA".to_string()));
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
            "Expected EOA validation denial, got {:?}", decision
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
            "Expected InsufficientProfit denial, got {:?}", decision
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
            "Expected Allow, got {:?}", decision
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
}

