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
    use std::time::Duration;
    use tempfile::NamedTempFile;

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
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_yaml()).unwrap();
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
