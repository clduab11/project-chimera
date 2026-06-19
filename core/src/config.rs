//! Configuration loading and validation for Chimera.
use crate::ChimeraError;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;
use std::time::SystemTime;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PacingConfig {
    pub max_daily_net_usd: Decimal,
    pub max_weekly_net_usd: Decimal,
    pub max_single_transfer_usd: Decimal,
    pub min_interval_hours: u64,
    pub max_jitter_hours: u64,
    pub venue_rotation_count: u64,
    pub clean_eoa_pool_size: usize,
    pub auto_halt_on_reverts: u32,
    pub max_gas_gwei: u64,
    pub max_daily_loss_eth: Decimal,
    pub min_profit_multiplier: Decimal,
    pub execute_mode: String,
    pub log_level: String,
    pub metrics_port: u16,
    #[serde(default = "default_chain_id")]
    pub chain_id: u64,
    #[serde(default = "default_oracle_staleness_seconds")]
    pub oracle_staleness_seconds: u64,
    #[serde(default = "default_eth_price_usd_fallback")]
    pub eth_price_usd_fallback: Decimal,
    #[serde(default = "default_recent_outcomes_capacity")]
    pub recent_outcomes_capacity: usize,
    #[serde(default = "default_eoa_pool_path")]
    pub eoa_pool_path: String,
    #[serde(default = "default_pools_toml_path")]
    pub pools_toml_path: String,
}

fn default_chain_id() -> u64 {
    8453
}
fn default_oracle_staleness_seconds() -> u64 {
    300
}
fn default_eth_price_usd_fallback() -> Decimal {
    Decimal::from(1800)
}
fn default_recent_outcomes_capacity() -> usize {
    128
}
fn default_eoa_pool_path() -> String {
    "config/eoa_pool.json".into()
}
fn default_pools_toml_path() -> String {
    "config/pools.toml".into()
}

impl Default for PacingConfig {
    fn default() -> Self {
        Self {
            max_daily_net_usd: Decimal::from(2000),
            max_weekly_net_usd: Decimal::from(7500),
            max_single_transfer_usd: Decimal::from(1000),
            min_interval_hours: 6,
            max_jitter_hours: 12,
            venue_rotation_count: 5,
            clean_eoa_pool_size: 10,
            auto_halt_on_reverts: 3,
            max_gas_gwei: 300,
            max_daily_loss_eth: Decimal::from_str("0.005").expect("valid literal"),
            min_profit_multiplier: Decimal::from_str("2.5").expect("valid literal"),
            execute_mode: "shadow".into(),
            log_level: "info".into(),
            metrics_port: 9100,
            chain_id: 8453,
            oracle_staleness_seconds: 300,
            eth_price_usd_fallback: Decimal::from(1800),
            recent_outcomes_capacity: 128,
            eoa_pool_path: "config/eoa_pool.json".into(),
            pools_toml_path: "config/pools.toml".into(),
        }
    }
}

impl PacingConfig {
    /// Load from YAML file. Validates critical invariants.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ChimeraError> {
        let contents = std::fs::read_to_string(path)?;
        let cfg: PacingConfig = serde_yaml::from_str(&contents)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load from YAML, then override any field with a matching `CHIMERA_*` env var
    /// (e.g. `CHIMERA_MAX_DAILY_NET_USD` overrides `max_daily_net_usd`).
    pub fn load_with_env(path: impl AsRef<Path>) -> Result<Self, ChimeraError> {
        let mut cfg = Self::load(path)?;
        cfg.apply_env_overrides()?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Re-load config from disk and apply env overrides (for SIGHUP).
    pub fn reload(&mut self, path: impl AsRef<Path>) -> Result<(), ChimeraError> {
        let fresh = Self::load_with_env(path)?;
        *self = fresh;
        Ok(())
    }

    /// Validate hard invariants. Never relax without explicit operator decision.
    fn validate(&self) -> Result<(), ChimeraError> {
        if self.max_daily_net_usd > Decimal::from(2000) {
            return Err(ChimeraError::ConfigError(
                "max_daily_net_usd exceeds conservative risk threshold (2000)".into(),
            ));
        }
        if self.max_single_transfer_usd > Decimal::from(1000) {
            return Err(ChimeraError::ConfigError(
                "max_single_transfer_usd too high for L2 clean-wallet rotation".into(),
            ));
        }
        if self.min_profit_multiplier < Decimal::from(2) {
            return Err(ChimeraError::ConfigError(
                "min_profit_multiplier below safety floor (2.0x)".into(),
            ));
        }
        if self.execute_mode != "shadow" && self.execute_mode != "live" {
            return Err(ChimeraError::ConfigError(
                "execute_mode must be 'shadow' or 'live'".into(),
            ));
        }
        if self.min_interval_hours >= self.max_jitter_hours {
            return Err(ChimeraError::ConfigError(
                "min_interval_hours must be less than max_jitter_hours".into(),
            ));
        }
        if self.metrics_port <= 1024 || self.metrics_port == 65535 {
            return Err(ChimeraError::ConfigError(
                "metrics_port must be > 1024 and < 65535".into(),
            ));
        }
        Ok(())
    }

    /// Validate `execute_mode` transition from shadow to live.
    /// Requires a 7-day shadow period tracked in external state (`shadow_since`).
    pub fn validate_mode_transition(
        &self,
        previous_mode: &str,
        shadow_since: Option<SystemTime>,
    ) -> Result<(), ChimeraError> {
        if previous_mode == "shadow" && self.execute_mode == "live" {
            match shadow_since {
                Some(since) => {
                    let elapsed = since.elapsed().map_err(|e| {
                        ChimeraError::ConfigError(format!("Invalid shadow_since timestamp: {e}"))
                    })?;
                    if elapsed.as_secs() < 7 * 24 * 60 * 60 {
                        return Err(ChimeraError::ConfigError(
                            "execute_mode transition from shadow to live requires 7-day shadow period"
                                .into(),
                        ));
                    }
                }
                None => {
                    return Err(ChimeraError::ConfigError(
                        "execute_mode transition from shadow to live requires shadow_since timestamp in state"
                            .into(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn apply_env_overrides(&mut self) -> Result<(), ChimeraError> {
        macro_rules! override_decimal {
            ($env_key:literal, $field:expr) => {
                if let Ok(v) = std::env::var($env_key) {
                    $field = Decimal::from_str(&v).map_err(|e| {
                        ChimeraError::ConfigError(format!("Invalid {}: {e}", $env_key))
                    })?;
                }
            };
        }
        macro_rules! override_parse {
            ($env_key:literal, $field:expr, $T:ty) => {
                if let Ok(v) = std::env::var($env_key) {
                    $field = v.parse::<$T>().map_err(|e| {
                        ChimeraError::ConfigError(format!("Invalid {}: {e}", $env_key))
                    })?;
                }
            };
        }

        override_decimal!("CHIMERA_MAX_DAILY_NET_USD",        self.max_daily_net_usd);
        override_decimal!("CHIMERA_MAX_WEEKLY_NET_USD",       self.max_weekly_net_usd);
        override_decimal!("CHIMERA_MAX_SINGLE_TRANSFER_USD",  self.max_single_transfer_usd);
        override_decimal!("CHIMERA_MAX_DAILY_LOSS_ETH",       self.max_daily_loss_eth);
        override_decimal!("CHIMERA_MIN_PROFIT_MULTIPLIER",    self.min_profit_multiplier);
        override_decimal!("CHIMERA_ETH_PRICE_USD_FALLBACK",   self.eth_price_usd_fallback);

        override_parse!("CHIMERA_MIN_INTERVAL_HOURS",        self.min_interval_hours,        u64);
        override_parse!("CHIMERA_MAX_JITTER_HOURS",          self.max_jitter_hours,          u64);
        override_parse!("CHIMERA_VENUE_ROTATION_COUNT",      self.venue_rotation_count,      u64);
        override_parse!("CHIMERA_CLEAN_EOA_POOL_SIZE",       self.clean_eoa_pool_size,       usize);
        override_parse!("CHIMERA_AUTO_HALT_ON_REVERTS",      self.auto_halt_on_reverts,      u32);
        override_parse!("CHIMERA_MAX_GAS_GWEI",              self.max_gas_gwei,              u64);
        override_parse!("CHIMERA_METRICS_PORT",              self.metrics_port,              u16);
        override_parse!("CHIMERA_CHAIN_ID",                  self.chain_id,                  u64);
        override_parse!("CHIMERA_ORACLE_STALENESS_SECONDS",  self.oracle_staleness_seconds,  u64);

        if let Ok(v) = std::env::var("CHIMERA_EXECUTE_MODE") { self.execute_mode = v; }
        if let Ok(v) = std::env::var("CHIMERA_LOG_LEVEL")    { self.log_level    = v; }
        if let Ok(v) = std::env::var("CHIMERA_EOA_POOL_PATH")    { self.eoa_pool_path    = v; }
        if let Ok(v) = std::env::var("CHIMERA_POOLS_TOML_PATH")  { self.pools_toml_path  = v; }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::Duration;
    use tempfile::NamedTempFile;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn valid_yaml() -> String {
        r#"
max_daily_net_usd: 2000
max_weekly_net_usd: 7500
max_single_transfer_usd: 1000
min_interval_hours: 6
max_jitter_hours: 12
venue_rotation_count: 5
clean_eoa_pool_size: 10
auto_halt_on_reverts: 3
max_gas_gwei: 300
max_daily_loss_eth: 0.005
min_profit_multiplier: 2.5
execute_mode: shadow
log_level: info
metrics_port: 9100
chain_id: 8453
oracle_staleness_seconds: 300
eth_price_usd_fallback: 1800
recent_outcomes_capacity: 128
eoa_pool_path: config/eoa_pool.json
pools_toml_path: config/pools.toml
"#
        .into()
    }

    #[test]
    fn test_load_valid_config() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_yaml()).unwrap();
        let cfg = PacingConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.max_daily_net_usd,       Decimal::from(2000));
        assert_eq!(cfg.max_weekly_net_usd,      Decimal::from(7500));
        assert_eq!(cfg.max_single_transfer_usd, Decimal::from(1000));
        assert_eq!(cfg.execute_mode,            "shadow");
        assert_eq!(cfg.chain_id,                8453);
        assert_eq!(cfg.oracle_staleness_seconds, 300);
        assert_eq!(cfg.eth_price_usd_fallback,  Decimal::from(1800));
        assert_eq!(cfg.eoa_pool_path,           "config/eoa_pool.json");
        assert_eq!(cfg.pools_toml_path,         "config/pools.toml");
    }

    #[test]
    fn test_rejects_unsafe_daily_cap() {
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_yaml().replace("max_daily_net_usd: 2000", "max_daily_net_usd: 5000");
        writeln!(tmp, "{}", yaml).unwrap();
        let result = PacingConfig::load(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_yaml()).unwrap();
        std::env::remove_var("CHIMERA_CHAIN_ID");
        std::env::remove_var("CHIMERA_MAX_WEEKLY_NET_USD");
        std::env::set_var("CHIMERA_MAX_WEEKLY_NET_USD", "4999");
        std::env::set_var("CHIMERA_CHAIN_ID", "1");
        let cfg = PacingConfig::load_with_env(tmp.path()).unwrap();
        assert_eq!(cfg.max_weekly_net_usd, Decimal::from(4999));
        assert_eq!(cfg.chain_id, 1);
        std::env::remove_var("CHIMERA_MAX_WEEKLY_NET_USD");
        std::env::remove_var("CHIMERA_CHAIN_ID");
    }

    #[test]
    fn test_rejects_min_interval_not_less_than_max_jitter() {
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_yaml().replace("min_interval_hours: 6", "min_interval_hours: 12");
        writeln!(tmp, "{}", yaml).unwrap();
        let result = PacingConfig::load(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_rejects_invalid_metrics_port() {
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_yaml().replace("metrics_port: 9100", "metrics_port: 1024");
        writeln!(tmp, "{}", yaml).unwrap();
        let result = PacingConfig::load(tmp.path());
        assert!(result.is_err());

        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_yaml().replace("metrics_port: 9100", "metrics_port: 65535");
        writeln!(tmp, "{}", yaml).unwrap();
        let result = PacingConfig::load(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_mode_transition_requires_shadow_period() {
        let cfg = PacingConfig {
            execute_mode: "live".into(),
            ..PacingConfig::default()
        };
        // Missing shadow_since => fails
        let result = cfg.validate_mode_transition("shadow", None);
        assert!(result.is_err());
        // Recent shadow (< 7 days) => fails
        let recent = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
        let result = cfg.validate_mode_transition("shadow", Some(recent));
        assert!(result.is_err());
        // Old shadow (>= 7 days) => succeeds
        let old = SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
        let result = cfg.validate_mode_transition("shadow", Some(old));
        assert!(result.is_ok());
        // No transition (already live) => succeeds
        let result = cfg.validate_mode_transition("live", None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_reload() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CHIMERA_MAX_DAILY_NET_USD");
        std::env::remove_var("CHIMERA_CHAIN_ID");
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_yaml()).unwrap();
        let mut cfg = PacingConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.max_daily_net_usd, Decimal::from(2000));
        let updated = valid_yaml().replace("max_daily_net_usd: 2000", "max_daily_net_usd: 1000");
        let mut tmp2 = NamedTempFile::new().unwrap();
        writeln!(tmp2, "{}", updated).unwrap();
        cfg.reload(tmp2.path()).unwrap();
        assert_eq!(cfg.max_daily_net_usd, Decimal::from(1000));
    }
}

// ---------------------------------------------------------------------------
// RiskConfig ΓÇö loaded from config/risk.yaml
// ---------------------------------------------------------------------------

/// Risk and safety thresholds. Loaded from `config/risk.yaml`.
///
/// All monetary values use `Decimal` (Invariant #3).
/// Fields mirror the YAML keys exactly so serde_yaml deserializes them directly.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RiskConfig {
    /// Daily realized loss ceiling in ETH before full halt.
    pub max_loss_eth: Decimal,
    /// Minimum profit multiplier over (gas + priority + L1 data fee).
    pub min_profit: Decimal,
    /// Consecutive bundle reverts before auto-halt.
    pub auto_halt_reverts: u32,
    /// L2 gas price ceiling in gwei.
    pub max_gas_gwei: u64,
    /// Maximum acceptable slippage on any single leg, in basis points.
    pub slippage_max_bps: u32,
    /// Hard timeout for REVM full-path simulations, in milliseconds.
    pub simulation_timeout_ms: u64,
    /// Buffer multiplier applied on top of eth_getL1Fee result (e.g. 1.15 = +15%).
    pub l1_fee_scalar_buffer: Decimal,
    /// If the sequencer feed is silent for this many ms, pause new candidates.
    pub sequencer_stall_ms: u64,
    /// Maximum number of contracts to audit per day (bounty/audit risk gate).
    pub audit_max_contracts_per_day: u32,
    /// Minimum severity to treat a bounty finding as blocking (e.g. "MEDIUM").
    pub bounty_min_severity: String,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_loss_eth: Decimal::from_str("0.005").expect("valid literal"),
            min_profit: Decimal::from_str("2.5").expect("valid literal"),
            auto_halt_reverts: 3,
            max_gas_gwei: 300,
            slippage_max_bps: 50,
            simulation_timeout_ms: 1500,
            l1_fee_scalar_buffer: Decimal::from_str("1.15").expect("valid literal"),
            sequencer_stall_ms: 500,
            audit_max_contracts_per_day: 50,
            bounty_min_severity: "MEDIUM".into(),
        }
    }
}

impl RiskConfig {
    /// Load from YAML file and validate.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ChimeraError> {
        let contents = std::fs::read_to_string(path)?;
        let cfg: RiskConfig = serde_yaml::from_str(&contents)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ChimeraError> {
        if self.min_profit < Decimal::from(2) {
            return Err(ChimeraError::ConfigError(
                "risk.yaml min_profit below safety floor (2.0x)".into(),
            ));
        }
        if self.slippage_max_bps > 200 {
            return Err(ChimeraError::ConfigError(
                "risk.yaml slippage_max_bps exceeds 200bps (2%) safety ceiling".into(),
            ));
        }
        if self.l1_fee_scalar_buffer < Decimal::ONE {
            return Err(ChimeraError::ConfigError(
                "risk.yaml l1_fee_scalar_buffer must be >= 1.0".into(),
            ));
        }
        if self.simulation_timeout_ms == 0 {
            return Err(ChimeraError::ConfigError(
                "risk.yaml simulation_timeout_ms must be > 0".into(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RoutingConfig ΓÇö loaded from config/routing.yaml
// ---------------------------------------------------------------------------

/// A single DEX/liquidity venue entry.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct VenueEntry {
    pub name: String,
    pub chain: String,
    pub liquidity_usd_min: u64,
    #[serde(rename = "type")]
    pub venue_type: String,
    pub kyc: bool,
}

/// Routing and venue configuration. Loaded from `config/routing.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RoutingConfig {
    /// Primary RPC / submission endpoint identifier.
    pub primary: String,
    /// Ordered fallback RPC identifiers.
    #[serde(default)]
    pub fallbacks: Vec<String>,
    /// Submission style (e.g. "single_atomic_tx").
    pub submission_style: String,
    /// Active venue list. Rotated weekly via `scripts/update_venues.py`.
    #[serde(default)]
    pub venues: Vec<VenueEntry>,
    /// Forensic tag source URIs (remote URLs or "local:<path>").
    #[serde(default)]
    pub forensic_tag_sources: Vec<String>,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            primary: "alchemy-base-private".into(),
            fallbacks: vec![
                "public-base-rpc".into(),
                "public-arbitrum-rpc".into(),
            ],
            submission_style: "single_atomic_tx".into(),
            venues: Vec::new(),
            forensic_tag_sources: Vec::new(),
        }
    }
}

impl RoutingConfig {
    /// Load from YAML file and validate.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ChimeraError> {
        let contents = std::fs::read_to_string(path)?;
        let cfg: RoutingConfig = serde_yaml::from_str(&contents)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ChimeraError> {
        if self.primary.is_empty() {
            return Err(ChimeraError::ConfigError(
                "routing.yaml primary RPC must not be empty".into(),
            ));
        }
        if self.submission_style != "single_atomic_tx" && self.submission_style != "bundle" {
            return Err(ChimeraError::ConfigError(
                "routing.yaml submission_style must be 'single_atomic_tx' or 'bundle'".into(),
            ));
        }
        for venue in &self.venues {
            if venue.kyc {
                return Err(ChimeraError::ConfigError(format!(
                    "routing.yaml venue '{}' has kyc: true ΓÇö only non-KYC venues allowed",
                    venue.name
                )));
            }
            if venue.liquidity_usd_min < 50_000 {
                return Err(ChimeraError::ConfigError(format!(
                    "routing.yaml venue '{}' liquidity_usd_min {} below $50k floor",
                    venue.name, venue.liquidity_usd_min
                )));
            }
        }
        Ok(())
    }

    /// Return only the venues for a specific chain identifier.
    pub fn venues_for_chain(&self, chain: &str) -> Vec<&VenueEntry> {
        self.venues.iter().filter(|v| v.chain == chain).collect()
    }
}

#[cfg(test)]
mod risk_routing_tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn valid_risk_yaml() -> &'static str {
        r#"
max_loss_eth: 0.005
min_profit: 2.5
auto_halt_reverts: 3
max_gas_gwei: 300
slippage_max_bps: 50
simulation_timeout_ms: 1500
l1_fee_scalar_buffer: 1.15
sequencer_stall_ms: 500
audit_max_contracts_per_day: 50
bounty_min_severity: MEDIUM
"#
    }

    fn valid_routing_yaml() -> &'static str {
        r#"
primary: alchemy-base-private
fallbacks:
  - public-base-rpc
  - public-arbitrum-rpc
submission_style: single_atomic_tx
venues:
  - name: aerodrome-base
    chain: base
    liquidity_usd_min: 50000
    type: dex
    kyc: false
  - name: uniswap-v3-base
    chain: base
    liquidity_usd_min: 75000
    type: dex
    kyc: false
forensic_tag_sources:
  - "local:config/forensic_tags.json"
"#
    }

    #[test]
    fn test_risk_config_loads_and_defaults_match() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_risk_yaml()).unwrap();
        let cfg = RiskConfig::load(tmp.path()).unwrap();
        let defaults = RiskConfig::default();
        assert_eq!(cfg.max_loss_eth,           defaults.max_loss_eth);
        assert_eq!(cfg.min_profit,             defaults.min_profit);
        assert_eq!(cfg.auto_halt_reverts,      defaults.auto_halt_reverts);
        assert_eq!(cfg.max_gas_gwei,           defaults.max_gas_gwei);
        assert_eq!(cfg.slippage_max_bps,       defaults.slippage_max_bps);
        assert_eq!(cfg.simulation_timeout_ms,  defaults.simulation_timeout_ms);
        assert_eq!(cfg.l1_fee_scalar_buffer,   defaults.l1_fee_scalar_buffer);
        assert_eq!(cfg.sequencer_stall_ms,     defaults.sequencer_stall_ms);
        assert_eq!(cfg.bounty_min_severity,    defaults.bounty_min_severity);
    }

    #[test]
    fn test_risk_config_rejects_low_min_profit() {
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_risk_yaml().replace("min_profit: 2.5", "min_profit: 1.5");
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RiskConfig::load(tmp.path()).is_err());
    }

    #[test]
    fn test_risk_config_rejects_high_slippage() {
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_risk_yaml().replace("slippage_max_bps: 50", "slippage_max_bps: 300");
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RiskConfig::load(tmp.path()).is_err());
    }

    #[test]
    fn test_risk_config_rejects_low_l1_buffer() {
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_risk_yaml().replace("l1_fee_scalar_buffer: 1.15", "l1_fee_scalar_buffer: 0.9");
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RiskConfig::load(tmp.path()).is_err());
    }

    #[test]
    fn test_routing_config_loads() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_routing_yaml()).unwrap();
        let cfg = RoutingConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.primary, "alchemy-base-private");
        assert_eq!(cfg.submission_style, "single_atomic_tx");
        assert_eq!(cfg.venues.len(), 2);
        assert_eq!(cfg.venues[0].name, "aerodrome-base");
        assert!(!cfg.venues[0].kyc);
        assert_eq!(cfg.venues[0].liquidity_usd_min, 50_000);
        assert_eq!(cfg.forensic_tag_sources.len(), 1);
    }

    #[test]
    fn test_routing_venues_for_chain() {
        let yaml = r#"
primary: alchemy-base-private
fallbacks:
  - public-base-rpc
submission_style: single_atomic_tx
venues:
  - name: aerodrome-base
    chain: base
    liquidity_usd_min: 50000
    type: dex
    kyc: false
  - name: uniswap-v3-base
    chain: base
    liquidity_usd_min: 75000
    type: dex
    kyc: false
  - name: camelot-arbitrum
    chain: arbitrum
    liquidity_usd_min: 60000
    type: dex
    kyc: false
forensic_tag_sources:
  - "local:config/forensic_tags.json"
"#;
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", yaml).unwrap();
        let cfg = RoutingConfig::load(tmp.path()).unwrap();
        let base_venues = cfg.venues_for_chain("base");
        assert_eq!(base_venues.len(), 2);
        let arb_venues = cfg.venues_for_chain("arbitrum");
        assert_eq!(arb_venues.len(), 1);
        assert_eq!(arb_venues[0].name, "camelot-arbitrum");
    }

    #[test]
    fn test_routing_rejects_kyc_venue() {
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_routing_yaml().replace("kyc: false", "kyc: true");
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RoutingConfig::load(tmp.path()).is_err());
    }

    #[test]
    fn test_routing_rejects_low_liquidity_venue() {
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_routing_yaml().replace("liquidity_usd_min: 50000", "liquidity_usd_min: 10000");
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RoutingConfig::load(tmp.path()).is_err());
    }

    #[test]
    fn test_routing_rejects_invalid_submission_style() {
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_routing_yaml().replace("submission_style: single_atomic_tx", "submission_style: flashbots_bundle");
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RoutingConfig::load(tmp.path()).is_err());
    }

    #[test]
    fn test_disk_risk_yaml_parses() {
        let path = std::path::Path::new("../config/risk.yaml");
        let path = if path.exists() { path } else { std::path::Path::new("config/risk.yaml") };
        if path.exists() {
            RiskConfig::load(path).expect("config/risk.yaml must parse as RiskConfig");
        }
    }

    #[test]
    fn test_disk_routing_yaml_parses() {
        let path = std::path::Path::new("../config/routing.yaml");
        let path = if path.exists() { path } else { std::path::Path::new("config/routing.yaml") };
        if path.exists() {
            RoutingConfig::load(path).expect("config/routing.yaml must parse as RoutingConfig");
        }
    }
}


