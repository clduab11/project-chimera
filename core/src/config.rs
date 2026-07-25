//! Configuration loading and validation for Chimera.
use crate::ChimeraError;
use alloy::primitives::Address;
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
    /// Chainlink ETH/USD feed address for live gas-cost pricing.
    /// Pacing uses ChainlinkOracle — the feed address is inserted into
    /// the feed map keyed by itself so `refresh_eth_price` can resolve it.
    /// On chains where AaveOracle is preferred for pacing, rename this field
    /// to match the WETH/asset address and change the oracle wiring in main.rs.
    #[serde(default = "default_eth_usd_feed_address")]
    pub eth_usd_feed_address: String,
    #[serde(default = "default_recent_outcomes_capacity")]
    pub recent_outcomes_capacity: usize,
    #[serde(default = "default_eoa_pool_path")]
    pub eoa_pool_path: String,
    #[serde(default = "default_pools_toml_path")]
    pub pools_toml_path: String,
    #[serde(default)]
    pub executor_address: String,
    #[serde(default)]
    pub treasury_address: String,
    #[serde(default)]
    pub treasury_keystore: String,
    #[serde(default)]
    pub worker_keystore_dir: String,
    #[serde(default = "default_sweep_interval_secs")]
    pub sweep_interval_secs: u64,
    #[serde(default = "default_refund_interval_secs")]
    pub refund_interval_secs: u64,
    #[serde(default = "default_min_worker_balance_eth")]
    pub min_worker_balance_eth: Decimal,
    #[serde(default = "default_refund_topup_eth")]
    pub refund_topup_eth: Decimal,

    #[serde(default)]
    pub sweep_tokens: Vec<String>,

    #[serde(default = "default_sweep_min_keep_eth")]
    pub sweep_min_keep_eth: Decimal,

    /// WebSocket endpoint for the block-watch trigger. When populated, the
    /// orchestrator subscribes to `newHeads` via WebSocket instead of using
    /// fixed-interval polling. If empty (default), polling is used.
    /// Form: "ws://host:port" or "wss://host:port".
    #[serde(default)]
    pub ws_endpoint: String,
}

fn default_chain_id() -> u64 {
    8453
}
fn default_oracle_staleness_seconds() -> u64 {
    // Must exceed the Base ETH/USD Chainlink heartbeat (1200s; on-chain max
    // inter-round gap 1232s) or pacing sees a "stale" price between heartbeats.
    1500
}
fn default_eth_price_usd_fallback() -> Decimal {
    Decimal::from(1800)
}
fn default_eth_usd_feed_address() -> String {
    "0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70".into()
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
fn default_sweep_interval_secs() -> u64 {
    900
}
fn default_refund_interval_secs() -> u64 {
    3600
}
fn default_min_worker_balance_eth() -> Decimal {
    Decimal::from_str("0.002").expect("valid literal")
}
fn default_refund_topup_eth() -> Decimal {
    Decimal::from_str("0.01").expect("valid literal")
}
fn default_sweep_min_keep_eth() -> Decimal {
    Decimal::from_str("0.001").expect("valid literal")
}
fn default_router_compatibility() -> String {
    String::new()
}
fn default_min_profit_fraction() -> Decimal {
    Decimal::from_str("0.8").expect("valid literal")
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
            oracle_staleness_seconds: 1500,
            eth_price_usd_fallback: Decimal::from(1800),
            eth_usd_feed_address: default_eth_usd_feed_address(),
            recent_outcomes_capacity: 128,
            eoa_pool_path: "config/eoa_pool.json".into(),
            pools_toml_path: "config/pools.toml".into(),
            executor_address: String::new(),
            treasury_address: String::new(),
            treasury_keystore: String::new(),
            worker_keystore_dir: String::new(),
            sweep_interval_secs: 900,
            refund_interval_secs: 3600,
            min_worker_balance_eth: Decimal::from_str("0.002").expect("valid literal"),
            refund_topup_eth: Decimal::from_str("0.01").expect("valid literal"),
            sweep_tokens: Vec::new(),
            sweep_min_keep_eth: Decimal::from_str("0.001").expect("valid literal"),
            ws_endpoint: String::new(),
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
        let contents = std::fs::read_to_string(path)?;
        let mut cfg: PacingConfig = serde_yaml::from_str(&contents)?;
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
    pub fn validate(&self) -> Result<(), ChimeraError> {
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
        Self::parse_nonzero_address("eth_usd_feed_address", &self.eth_usd_feed_address)?;
        if self.execute_mode == "live" {
            self.validate_live_fields()?;
            self.validate_live_paths()?;
        }
        Ok(())
    }

    /// Validate live-only values without touching the filesystem. Keeping this
    /// separate makes address and required-field validation deterministic in tests.
    fn validate_live_fields(&self) -> Result<(), ChimeraError> {
        for (field, value) in [
            ("executor_address", self.executor_address.as_str()),
            ("treasury_address", self.treasury_address.as_str()),
            ("treasury_keystore", self.treasury_keystore.as_str()),
            ("worker_keystore_dir", self.worker_keystore_dir.as_str()),
            ("eoa_pool_path", self.eoa_pool_path.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ChimeraError::ConfigError(format!(
                    "execute_mode=live requires nonempty {field}; set it in pacing.yaml or the corresponding CHIMERA_* environment variable"
                )));
            }
        }

        Self::parse_nonzero_address("executor_address", &self.executor_address)?;
        Self::parse_nonzero_address("treasury_address", &self.treasury_address)?;
        if self.min_worker_balance_eth <= Decimal::ZERO {
            return Err(ChimeraError::ConfigError(
                "execute_mode=live requires min_worker_balance_eth > 0".into(),
            ));
        }
        if self.refund_topup_eth <= Decimal::ZERO {
            return Err(ChimeraError::ConfigError(
                "execute_mode=live requires refund_topup_eth > 0".into(),
            ));
        }
        Ok(())
    }

    fn validate_live_paths(&self) -> Result<(), ChimeraError> {
        Self::require_file("treasury_keystore", &self.treasury_keystore)?;
        Self::require_dir("worker_keystore_dir", &self.worker_keystore_dir)?;
        Self::require_file("eoa_pool_path", &self.eoa_pool_path)?;
        Ok(())
    }

    fn parse_nonzero_address(field: &str, value: &str) -> Result<Address, ChimeraError> {
        let address = value.parse::<Address>().map_err(|e| {
            ChimeraError::ConfigError(format!(
                "{field} must be a valid EVM address; got {value:?}: {e}"
            ))
        })?;
        if address == Address::ZERO {
            return Err(ChimeraError::ConfigError(format!(
                "{field} must not be the zero address"
            )));
        }
        Ok(address)
    }

    fn require_file(field: &str, value: &str) -> Result<(), ChimeraError> {
        let path = Path::new(value);
        if !path.exists() {
            return Err(ChimeraError::ConfigError(format!(
                "execute_mode=live {field} does not exist: {}",
                path.display()
            )));
        }
        if !path.is_file() {
            return Err(ChimeraError::ConfigError(format!(
                "execute_mode=live {field} must be a file: {}",
                path.display()
            )));
        }
        Ok(())
    }

    fn require_dir(field: &str, value: &str) -> Result<(), ChimeraError> {
        let path = Path::new(value);
        if !path.exists() {
            return Err(ChimeraError::ConfigError(format!(
                "execute_mode=live {field} does not exist: {}",
                path.display()
            )));
        }
        if !path.is_dir() {
            return Err(ChimeraError::ConfigError(format!(
                "execute_mode=live {field} must be a directory: {}",
                path.display()
            )));
        }
        Ok(())
    }

    /// Validate mode state whenever the effective `execute_mode` is live.
    /// The 7-day (604800s) shadow-soak requirement was de-listed by operator
    /// decision on 2026-07-20: a missing or recent `shadow_since` no longer
    /// blocks live mode. A future `shadow_since` still fails as a corruption
    /// guard against an invalid mode-state file.
    pub fn validate_mode_transition(
        &self,
        _previous_mode: &str,
        shadow_since: Option<SystemTime>,
    ) -> Result<(), ChimeraError> {
        if self.execute_mode == "live" {
            if let Some(since) = shadow_since {
                SystemTime::now().duration_since(since).map_err(|_| {
                    ChimeraError::ConfigError(
                        "execute_mode=live requires shadow_since to be a valid past timestamp; correct the future timestamp in mode state"
                            .into(),
                    )
                })?;
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

        override_decimal!("CHIMERA_MAX_DAILY_NET_USD", self.max_daily_net_usd);
        override_decimal!("CHIMERA_MAX_WEEKLY_NET_USD", self.max_weekly_net_usd);
        override_decimal!(
            "CHIMERA_MAX_SINGLE_TRANSFER_USD",
            self.max_single_transfer_usd
        );
        override_decimal!("CHIMERA_MAX_DAILY_LOSS_ETH", self.max_daily_loss_eth);
        override_decimal!("CHIMERA_MIN_PROFIT_MULTIPLIER", self.min_profit_multiplier);
        override_decimal!(
            "CHIMERA_ETH_PRICE_USD_FALLBACK",
            self.eth_price_usd_fallback
        );

        override_parse!("CHIMERA_MIN_INTERVAL_HOURS", self.min_interval_hours, u64);
        override_parse!("CHIMERA_MAX_JITTER_HOURS", self.max_jitter_hours, u64);
        override_parse!(
            "CHIMERA_VENUE_ROTATION_COUNT",
            self.venue_rotation_count,
            u64
        );
        override_parse!(
            "CHIMERA_CLEAN_EOA_POOL_SIZE",
            self.clean_eoa_pool_size,
            usize
        );
        override_parse!(
            "CHIMERA_AUTO_HALT_ON_REVERTS",
            self.auto_halt_on_reverts,
            u32
        );
        override_parse!("CHIMERA_MAX_GAS_GWEI", self.max_gas_gwei, u64);
        override_parse!("CHIMERA_METRICS_PORT", self.metrics_port, u16);
        override_parse!("CHIMERA_CHAIN_ID", self.chain_id, u64);
        override_parse!(
            "CHIMERA_ORACLE_STALENESS_SECONDS",
            self.oracle_staleness_seconds,
            u64
        );

        if let Ok(v) = std::env::var("CHIMERA_EXECUTE_MODE") {
            self.execute_mode = v;
        }
        if let Ok(v) = std::env::var("CHIMERA_LOG_LEVEL") {
            self.log_level = v;
        }
        if let Ok(v) = std::env::var("CHIMERA_ETH_USD_FEED_ADDRESS") {
            self.eth_usd_feed_address = v;
        }
        if let Ok(v) = std::env::var("CHIMERA_EOA_POOL_PATH") {
            self.eoa_pool_path = v;
        }
        if let Ok(v) = std::env::var("CHIMERA_POOLS_TOML_PATH") {
            self.pools_toml_path = v;
        }
        if let Ok(v) = std::env::var("CHIMERA_EXECUTOR_ADDRESS") {
            self.executor_address = v;
        }
        if let Ok(v) = std::env::var("CHIMERA_TREASURY_ADDRESS") {
            self.treasury_address = v;
        }
        if let Ok(v) = std::env::var("CHIMERA_TREASURY_KEYSTORE") {
            self.treasury_keystore = v;
        }
        if let Ok(v) = std::env::var("CHIMERA_WORKER_KEYSTORE_DIR") {
            self.worker_keystore_dir = v;
        }
        override_parse!("CHIMERA_SWEEP_INTERVAL_SECS", self.sweep_interval_secs, u64);
        override_parse!(
            "CHIMERA_REFUND_INTERVAL_SECS",
            self.refund_interval_secs,
            u64
        );
        override_decimal!(
            "CHIMERA_MIN_WORKER_BALANCE_ETH",
            self.min_worker_balance_eth
        );
        override_decimal!("CHIMERA_REFUND_TOPUP_ETH", self.refund_topup_eth);
        override_parse!(
            "CHIMERA_RECENT_OUTCOMES_CAPACITY",
            self.recent_outcomes_capacity,
            usize
        );
        override_decimal!("CHIMERA_SWEEP_MIN_KEEP_ETH", self.sweep_min_keep_eth);
        if let Ok(v) = std::env::var("CHIMERA_SWEEP_TOKENS") {
            self.sweep_tokens = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }

        if let Ok(v) = std::env::var("CHIMERA_WS_ENDPOINT") {
            self.ws_endpoint = v;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Mutex, MutexGuard};
    use std::time::Duration;
    use tempfile::NamedTempFile;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const PACING_ENV_KEYS: &[&str] = &[
        "CHIMERA_MAX_DAILY_NET_USD",
        "CHIMERA_MAX_WEEKLY_NET_USD",
        "CHIMERA_MAX_SINGLE_TRANSFER_USD",
        "CHIMERA_MAX_DAILY_LOSS_ETH",
        "CHIMERA_MIN_PROFIT_MULTIPLIER",
        "CHIMERA_ETH_PRICE_USD_FALLBACK",
        "CHIMERA_MIN_INTERVAL_HOURS",
        "CHIMERA_MAX_JITTER_HOURS",
        "CHIMERA_VENUE_ROTATION_COUNT",
        "CHIMERA_CLEAN_EOA_POOL_SIZE",
        "CHIMERA_AUTO_HALT_ON_REVERTS",
        "CHIMERA_MAX_GAS_GWEI",
        "CHIMERA_METRICS_PORT",
        "CHIMERA_CHAIN_ID",
        "CHIMERA_ORACLE_STALENESS_SECONDS",
        "CHIMERA_EXECUTE_MODE",
        "CHIMERA_LOG_LEVEL",
        "CHIMERA_ETH_USD_FEED_ADDRESS",
        "CHIMERA_EOA_POOL_PATH",
        "CHIMERA_POOLS_TOML_PATH",
        "CHIMERA_EXECUTOR_ADDRESS",
        "CHIMERA_TREASURY_ADDRESS",
        "CHIMERA_TREASURY_KEYSTORE",
        "CHIMERA_WORKER_KEYSTORE_DIR",
        "CHIMERA_SWEEP_INTERVAL_SECS",
        "CHIMERA_REFUND_INTERVAL_SECS",
        "CHIMERA_MIN_WORKER_BALANCE_ETH",
        "CHIMERA_REFUND_TOPUP_ETH",
        "CHIMERA_RECENT_OUTCOMES_CAPACITY",
        "CHIMERA_SWEEP_MIN_KEEP_ETH",
        "CHIMERA_SWEEP_TOKENS",
        "CHIMERA_WS_ENDPOINT",
    ];

    struct PacingEnvGuard {
        _lock: MutexGuard<'static, ()>,
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl PacingEnvGuard {
        fn new() -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = PACING_ENV_KEYS
                .iter()
                .map(|&key| (key, std::env::var(key).ok()))
                .collect();
            for &key in PACING_ENV_KEYS {
                std::env::remove_var(key);
            }
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for PacingEnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.previous {
                match value {
                    Some(value) => std::env::set_var(*key, value),
                    None => std::env::remove_var(*key),
                }
            }
        }
    }

    fn valid_yaml() -> String {
        "max_daily_net_usd: 2000
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
oracle_staleness_seconds: 1500
eth_price_usd_fallback: 1800
eth_usd_feed_address: \"0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70\"
recent_outcomes_capacity: 128
eoa_pool_path: config/eoa_pool.json
pools_toml_path: config/pools.toml
executor_address: \"\"
treasury_address: \"\"
treasury_keystore: \"\"
worker_keystore_dir: \"\"
sweep_interval_secs: 900
refund_interval_secs: 3600
min_worker_balance_eth: 0.002
refund_topup_eth: 0.01
sweep_tokens: []
sweep_min_keep_eth: 0.001
ws_endpoint: \"\"
"
        .to_string()
    }

    #[test]
    fn test_load_valid_config() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_yaml()).unwrap();
        let cfg = PacingConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.max_daily_net_usd, Decimal::from(2000));
        assert_eq!(cfg.max_weekly_net_usd, Decimal::from(7500));
        assert_eq!(cfg.max_single_transfer_usd, Decimal::from(1000));
        assert_eq!(cfg.execute_mode, "shadow");
        assert_eq!(cfg.chain_id, 8453);
        assert_eq!(cfg.oracle_staleness_seconds, 1500);
        assert_eq!(cfg.eth_price_usd_fallback, Decimal::from(1800));
        assert_eq!(
            cfg.eth_usd_feed_address,
            "0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70"
        );
        assert_eq!(cfg.eoa_pool_path, "config/eoa_pool.json");
        assert_eq!(cfg.pools_toml_path, "config/pools.toml");
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
    fn test_env_override_recent_outcomes_capacity() {
        let _guard = PacingEnvGuard::new();
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_yaml()).unwrap();
        std::env::set_var("CHIMERA_RECENT_OUTCOMES_CAPACITY", "256");
        let cfg = PacingConfig::load_with_env(tmp.path()).unwrap();
        assert_eq!(cfg.recent_outcomes_capacity, 256);
    }

    #[test]
    fn test_env_override_sweep_min_keep_eth() {
        let _guard = PacingEnvGuard::new();
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_yaml()).unwrap();
        std::env::set_var("CHIMERA_SWEEP_MIN_KEEP_ETH", "0.01");
        let cfg = PacingConfig::load_with_env(tmp.path()).unwrap();
        assert_eq!(cfg.sweep_min_keep_eth, Decimal::from_str("0.01").unwrap());
    }

    #[test]
    fn test_env_override_sweep_tokens() {
        let _guard = PacingEnvGuard::new();
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_yaml()).unwrap();
        std::env::set_var("CHIMERA_SWEEP_TOKENS", "0xUSDC,0xUSDT , 0xDAI");
        let cfg = PacingConfig::load_with_env(tmp.path()).unwrap();
        assert_eq!(cfg.sweep_tokens, vec!["0xUSDC", "0xUSDT", "0xDAI"]);
    }

    #[test]
    fn test_env_override() {
        let _guard = PacingEnvGuard::new();
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", valid_yaml()).unwrap();
        std::env::set_var("CHIMERA_MAX_WEEKLY_NET_USD", "4999");
        std::env::set_var("CHIMERA_CHAIN_ID", "1");
        let cfg = PacingConfig::load_with_env(tmp.path()).unwrap();
        assert_eq!(cfg.max_weekly_net_usd, Decimal::from(4999));
        assert_eq!(cfg.chain_id, 1);
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
    fn test_rejects_invalid_or_zero_eth_usd_feed() {
        let mut cfg = PacingConfig {
            eth_usd_feed_address: "not-an-address".into(),
            ..Default::default()
        };
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("eth_usd_feed_address"));

        cfg.eth_usd_feed_address = format!("{:#x}", Address::ZERO);
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("zero address"));
    }

    #[test]
    fn test_live_field_validation_is_pure_and_actionable() {
        let mut cfg = PacingConfig {
            execute_mode: "live".into(),
            ..PacingConfig::default()
        };
        assert!(cfg
            .validate_live_fields()
            .unwrap_err()
            .to_string()
            .contains("executor_address"));

        cfg.executor_address = "0x1111111111111111111111111111111111111111".into();
        cfg.treasury_address = "0x2222222222222222222222222222222222222222".into();
        cfg.treasury_keystore = "not-created-treasury.json".into();
        cfg.worker_keystore_dir = "not-created-workers".into();
        cfg.eoa_pool_path = "not-created-pool.json".into();
        cfg.validate_live_fields()
            .expect("pure live field validation must not require real keystores");
    }

    #[test]
    fn test_executor_address_validation_is_nonzero_and_live_only() {
        let mut cfg = PacingConfig {
            executor_address: format!("{:#x}", Address::ZERO),
            ..PacingConfig::default()
        };
        cfg.validate()
            .expect("shadow config must not require or validate executor_address");

        cfg.execute_mode = "live".into();
        cfg.treasury_address = "0x2222222222222222222222222222222222222222".into();
        cfg.treasury_keystore = "treasury.json".into();
        cfg.worker_keystore_dir = "workers".into();
        cfg.eoa_pool_path = "eoa_pool.json".into();
        let error = cfg.validate_live_fields().unwrap_err().to_string();
        assert!(error.contains("executor_address"));
        assert!(error.contains("zero address"));
    }

    #[test]
    fn test_live_validation_requires_positive_refund_topup() {
        let cfg = PacingConfig {
            execute_mode: "live".into(),
            executor_address: "0x1111111111111111111111111111111111111111".into(),
            treasury_address: "0x2222222222222222222222222222222222222222".into(),
            treasury_keystore: "treasury.json".into(),
            worker_keystore_dir: "workers".into(),
            eoa_pool_path: "eoa_pool.json".into(),
            refund_topup_eth: Decimal::ZERO,
            ..PacingConfig::default()
        };

        let error = cfg.validate_live_fields().unwrap_err().to_string();
        assert!(error.contains("refund_topup_eth > 0"));
    }

    #[test]
    fn test_live_validation_requires_correct_path_types() {
        let treasury = NamedTempFile::new().unwrap();
        let pool = NamedTempFile::new().unwrap();
        let workers = tempfile::tempdir().unwrap();
        let cfg = PacingConfig {
            execute_mode: "live".into(),
            executor_address: "0x1111111111111111111111111111111111111111".into(),
            treasury_address: "0x2222222222222222222222222222222222222222".into(),
            treasury_keystore: treasury.path().to_string_lossy().into_owned(),
            worker_keystore_dir: workers.path().to_string_lossy().into_owned(),
            eoa_pool_path: pool.path().to_string_lossy().into_owned(),
            ..PacingConfig::default()
        };
        cfg.validate()
            .expect("valid live path types should pass config validation");

        let wrong_type = PacingConfig {
            worker_keystore_dir: treasury.path().to_string_lossy().into_owned(),
            ..cfg
        };
        assert!(wrong_type
            .validate()
            .unwrap_err()
            .to_string()
            .contains("must be a directory"));
    }

    #[test]
    fn test_validate_mode_transition_soak_gate_delisted() {
        let cfg = PacingConfig {
            execute_mode: "live".into(),
            ..PacingConfig::default()
        };
        // Soak gate de-listed by operator decision on 2026-07-20: a missing or
        // recent shadow_since no longer blocks live mode.
        assert!(cfg.validate_mode_transition("shadow", None).is_ok());
        let recent = SystemTime::now() - Duration::from_secs(60);
        assert!(cfg.validate_mode_transition("shadow", Some(recent)).is_ok());
        assert!(cfg.validate_mode_transition("live", None).is_ok());
        assert!(cfg.validate_mode_transition("live", Some(recent)).is_ok());
        let old = SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
        assert!(cfg.validate_mode_transition("live", Some(old)).is_ok());
    }

    #[test]
    fn test_validate_mode_transition_allows_effective_shadow_mode() {
        let cfg = PacingConfig::default();
        cfg.validate_mode_transition("live", None)
            .expect("effective shadow mode must not require a shadow_since timestamp");
    }

    #[test]
    fn test_validate_mode_transition_rejects_future_shadow_timestamp() {
        let cfg = PacingConfig {
            execute_mode: "live".into(),
            ..PacingConfig::default()
        };
        let future = SystemTime::now() + Duration::from_secs(60 * 60);
        let error = cfg
            .validate_mode_transition("live", Some(future))
            .unwrap_err()
            .to_string();
        assert!(error.contains("future"));
    }

    #[test]
    fn test_reload() {
        let _guard = PacingEnvGuard::new();
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
    /// Fraction of expected profit enforced as the on-chain `min_profit` gate.
    /// Range 0.0–1.0. Default 0.8 requires 80% of simulated expected profit to be
    /// retained on-chain, protecting against sandwich attacks while leaving a buffer
    /// for gas/slippage variance.
    ///
    /// The orchestrator converts `expected_profit_usd → debt‑token wei` using the
    /// ETH/USD oracle price (assumes WETH as the debt asset, matching the routing
    /// config pairs). For non‑WETH debt assets, this conversion is approximate.
    #[serde(default = "default_min_profit_fraction")]
    pub min_profit_fraction: Decimal,
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
    /// Minimum seconds between live oracle repricings of the detector snapshot.
    /// One batched eth_call per refresh regardless of reserve count; consecutive
    /// failures stretch the interval exponentially (capped at max(60, interval)).
    #[serde(default = "default_price_refresh_secs")]
    pub price_refresh_secs: u64,
    /// Live-price age (seconds) past which detection is considered degraded:
    /// shadow mode warns; live mode skips candidate emission for the scan
    /// (stale prices produce false candidates, which cost real gas when live).
    #[serde(default = "default_price_max_stale_secs")]
    pub price_max_stale_secs: u64,
}

fn default_price_refresh_secs() -> u64 {
    8
}

fn default_price_max_stale_secs() -> u64 {
    300
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_loss_eth: Decimal::from_str("0.005").expect("valid literal"),
            min_profit: Decimal::from_str("2.5").expect("valid literal"),
            min_profit_fraction: Decimal::from_str("0.8").expect("valid literal"),
            auto_halt_reverts: 3,
            max_gas_gwei: 300,
            slippage_max_bps: 50,
            simulation_timeout_ms: 1500,
            l1_fee_scalar_buffer: Decimal::from_str("1.15").expect("valid literal"),
            sequencer_stall_ms: 500,
            audit_max_contracts_per_day: 50,
            bounty_min_severity: "MEDIUM".into(),
            price_refresh_secs: default_price_refresh_secs(),
            price_max_stale_secs: default_price_max_stale_secs(),
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
        if self.price_refresh_secs == 0 {
            return Err(ChimeraError::ConfigError(
                "risk.yaml price_refresh_secs must be > 0".into(),
            ));
        }
        if self.price_max_stale_secs < self.price_refresh_secs {
            return Err(ChimeraError::ConfigError(
                "risk.yaml price_max_stale_secs must be >= price_refresh_secs \
                 (a staleness bound below the refresh interval always trips)"
                    .into(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// StrategyParams — nine-word strategy tail inside Executor execute(bytes)
// ---------------------------------------------------------------------------

/// Explicit nine-word (288-byte) strategy tail. In the deployed Executor's
/// eleven-word `execute(bytes)` payload, these fields occupy words 2 through 10
/// after the debt asset and flash-loan amount.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrategyParams {
    /// word 0: collateralAsset (address, left-padded)
    pub collateral_asset: [u8; 32],
    /// word 1: userToLiquidate (address, left-padded)
    pub user_to_liquidate: [u8; 32],
    /// word 2: debtToCover (uint256)
    pub debt_to_cover: [u8; 32],
    /// word 3: receiveAToken (uint256, 0/1)
    pub receive_a_token: [u8; 32],
    /// word 4: dexRouter (address, left-padded)
    pub dex_router: [u8; 32],
    /// word 5: amountOutMin (uint256)
    pub amount_out_min: [u8; 32],
    /// word 6: minProfit (uint256)
    pub min_profit: [u8; 32],
    /// word 7: tip (uint256)
    pub tip: [u8; 32],
    /// word 8: deadline (uint256)
    pub deadline: [u8; 32],
}

impl StrategyParams {
    /// Encode to exactly 288 bytes (9 * 32) in the documented field order.
    pub fn encode(&self) -> [u8; 288] {
        let mut out = [0u8; 288];
        out[0..32].copy_from_slice(&self.collateral_asset);
        out[32..64].copy_from_slice(&self.user_to_liquidate);
        out[64..96].copy_from_slice(&self.debt_to_cover);
        out[96..128].copy_from_slice(&self.receive_a_token);
        out[128..160].copy_from_slice(&self.dex_router);
        out[160..192].copy_from_slice(&self.amount_out_min);
        out[192..224].copy_from_slice(&self.min_profit);
        out[224..256].copy_from_slice(&self.tip);
        out[256..288].copy_from_slice(&self.deadline);
        out
    }
}

// ---------------------------------------------------------------------------
// RoutingConfig — loaded from config/routing.yaml
// ---------------------------------------------------------------------------

/// A single DEX/liquidity venue entry.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct VenueEntry {
    pub name: String,
    pub chain: String,
    pub liquidity_usd_min: u64,
    #[serde(rename = "type")]
    pub venue_type: String,
    pub kyc: bool,
    #[serde(default)]
    pub router_address: String,
    #[serde(default)]
    pub pairs: Vec<TradingPair>,
    #[serde(default = "default_router_compatibility")]
    pub router_compatibility: String,
}

/// V2 2-hop trading pair definition.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct TradingPair {
    pub token_in: String,
    pub token_out: String,
}

/// Known DEX *factory* addresses (lowercased). A factory in a `router_address`
/// slot is a catastrophic config error: factories expose no swap selector, so
/// every collateral→debt swap reverts silently. Config load hard-fails on these.
///
/// NOTE: the canonical Uniswap V3 factory (0x1f98431c8ad98523631ae4a59f267346ea31f984)
/// is deliberately NOT listed: the committed `uniswap-v3-arbitrum` venue still
/// carries it as a known placeholder (inert — the resolver skips "v3") pending
/// Phase Delta re-verification. Listing it would make the committed
/// routing.yaml fail to load and brick engine startup. The ops dashboard
/// surfaces it as a soft config-lint warning instead.
pub const KNOWN_FACTORY_ROUTERS: &[&str] = &[
    "0x71524b4f93c58fcbf659783284e38825f0622859", // SushiSwap V2 factory (Base)
    "0x33128a8fc17869897dce68ed026d694621f6fdfd", // Uniswap V3 factory (Base)
];

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
            fallbacks: vec!["public-base-rpc".into(), "public-arbitrum-rpc".into()],
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
        self.validate_with_factories(KNOWN_FACTORY_ROUTERS)
    }

    /// Full validation with an explicit factory denylist (lowercased 0x-hex
    /// addresses). `validate()` calls this with [`KNOWN_FACTORY_ROUTERS`].
    pub fn validate_with_factories(&self, known_factories: &[&str]) -> Result<(), ChimeraError> {
        const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";
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
        // (chain, lowercased router, venue name) for same-chain duplicate detection.
        let mut seen_routers: Vec<(String, String, String)> = Vec::new();
        for venue in &self.venues {
            if venue.kyc {
                return Err(ChimeraError::ConfigError(format!(
                    "routing.yaml venue '{}' has kyc: true — only non-KYC venues allowed",
                    venue.name
                )));
            }
            if venue.liquidity_usd_min < 50_000 {
                return Err(ChimeraError::ConfigError(format!(
                    "routing.yaml venue '{}' liquidity_usd_min {} below $50k floor",
                    venue.name, venue.liquidity_usd_min
                )));
            }
            if !["v2", "v3", "custom", ""].contains(&venue.router_compatibility.as_str()) {
                return Err(ChimeraError::ConfigError(format!(
                    "routing.yaml venue '{}' has unknown router_compatibility '{}'",
                    venue.name, venue.router_compatibility
                )));
            }

            let router_lc = venue.router_address.to_lowercase();

            // A venue that declares a router_compatibility is (now or in a
            // later phase) selectable by the resolver — it must carry a real
            // router. Venues with an empty compatibility remain legacy/inert
            // and may omit the router (existing fixtures rely on this).
            let selectable = venue.venue_type == "dex" && !venue.router_compatibility.is_empty();
            if selectable && (router_lc.is_empty() || router_lc == ZERO_ADDRESS) {
                return Err(ChimeraError::ConfigError(format!(
                    "routing.yaml venue '{}' (router_compatibility '{}') has an empty/zero \
                     router_address — selectable venues need a real router",
                    venue.name, venue.router_compatibility
                )));
            }

            if !router_lc.is_empty() {
                if known_factories.contains(&router_lc.as_str()) {
                    return Err(ChimeraError::ConfigError(format!(
                        "routing.yaml venue '{}' router_address {} is a known DEX *factory*, \
                         not a router — every swap through it would revert",
                        venue.name, venue.router_address
                    )));
                }
                // Same-chain duplicate routers are a copy-paste error. The same
                // address on DIFFERENT chains is legitimate (deterministic
                // cross-chain deployments), so the check is scoped per chain.
                if let Some((_, _, other)) = seen_routers
                    .iter()
                    .find(|(chain, router, _)| *chain == venue.chain && *router == router_lc)
                {
                    return Err(ChimeraError::ConfigError(format!(
                        "routing.yaml venues '{}' and '{}' share router_address {} on chain '{}'",
                        other, venue.name, venue.router_address, venue.chain
                    )));
                }
                seen_routers.push((venue.chain.clone(), router_lc, venue.name.clone()));
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
    router_address: "0x0000000000000000000000000000000000000001"
    pairs:
      - token_in: "0xUSDC"
        token_out: "0xWETH"
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
        assert_eq!(cfg.max_loss_eth, defaults.max_loss_eth);
        assert_eq!(cfg.min_profit, defaults.min_profit);
        assert_eq!(cfg.auto_halt_reverts, defaults.auto_halt_reverts);
        assert_eq!(cfg.max_gas_gwei, defaults.max_gas_gwei);
        assert_eq!(cfg.slippage_max_bps, defaults.slippage_max_bps);
        assert_eq!(cfg.simulation_timeout_ms, defaults.simulation_timeout_ms);
        assert_eq!(cfg.l1_fee_scalar_buffer, defaults.l1_fee_scalar_buffer);
        assert_eq!(cfg.sequencer_stall_ms, defaults.sequencer_stall_ms);
        assert_eq!(cfg.bounty_min_severity, defaults.bounty_min_severity);
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
        let yaml =
            valid_risk_yaml().replace("l1_fee_scalar_buffer: 1.15", "l1_fee_scalar_buffer: 0.9");
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
        let yaml =
            valid_routing_yaml().replace("liquidity_usd_min: 50000", "liquidity_usd_min: 10000");
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RoutingConfig::load(tmp.path()).is_err());
    }

    /// Minimal selectable-venue YAML for router-guard tests.
    fn selectable_venue_yaml(name: &str, chain: &str, compat: &str, router: &str) -> String {
        format!(
            r#"  - name: {name}
    chain: {chain}
    liquidity_usd_min: 50000
    type: dex
    kyc: false
    router_compatibility: "{compat}"
    router_address: "{router}"
"#
        )
    }

    fn routing_yaml_with_venues(venues: &str) -> String {
        format!(
            "primary: alchemy-base-private\nsubmission_style: single_atomic_tx\nvenues:\n{venues}"
        )
    }

    #[test]
    fn test_routing_rejects_zero_router_on_selectable_venue() {
        // Rule (a): a venue the resolver can select must not have a zero router.
        let yaml = routing_yaml_with_venues(&selectable_venue_yaml(
            "bad-dex",
            "base",
            "v2",
            "0x0000000000000000000000000000000000000000",
        ));
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RoutingConfig::load(tmp.path()).is_err());

        // ...nor an empty router.
        let yaml = routing_yaml_with_venues(&selectable_venue_yaml("bad-dex", "base", "v2", ""));
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RoutingConfig::load(tmp.path()).is_err());
    }

    #[test]
    fn test_routing_rejects_factory_as_router() {
        // Rule (b): a known DEX factory in a router slot must fail config load
        // (factories have no swap selector — every swap would revert).
        // Checksummed casing on purpose: the check must be case-insensitive.
        let yaml = routing_yaml_with_venues(&selectable_venue_yaml(
            "sushi-base",
            "base",
            "v2",
            "0x71524B4f93c58fcbF659783284E38825f0622859", // Sushi V2 FACTORY
        ));
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RoutingConfig::load(tmp.path()).is_err());
    }

    #[test]
    fn test_routing_rejects_duplicate_router_same_chain() {
        // Rule (c): two venues on the SAME chain sharing a router is a
        // copy-paste error...
        let venues = format!(
            "{}{}",
            selectable_venue_yaml(
                "dex-one",
                "base",
                "v2",
                "0x6BDED42c6DA8FBf0d2bA55B2fa120C5e0c8D7891"
            ),
            selectable_venue_yaml(
                "dex-two",
                "base",
                "v2",
                "0x6bded42c6da8fbf0d2ba55b2fa120c5e0c8d7891" // same router, different case
            ),
        );
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", routing_yaml_with_venues(&venues)).unwrap();
        assert!(RoutingConfig::load(tmp.path()).is_err());

        // ...but the same address on DIFFERENT chains is legitimate
        // (deterministic cross-chain deployments) and must load.
        let venues = format!(
            "{}{}",
            selectable_venue_yaml(
                "dex-base",
                "base",
                "v2",
                "0x6BDED42c6DA8FBf0d2bA55B2fa120C5e0c8D7891"
            ),
            selectable_venue_yaml(
                "dex-arb",
                "arbitrum",
                "v2",
                "0x6BDED42c6DA8FBf0d2bA55B2fa120C5e0c8D7891"
            ),
        );
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "{}", routing_yaml_with_venues(&venues)).unwrap();
        assert!(RoutingConfig::load(tmp.path()).is_ok());
    }

    #[test]
    fn test_risk_config_rejects_underflow_slippage() {
        // Rule (d): slippage_max_bps > 10000 would underflow the amountOutMin
        // discount at resolver::compute_amount_out_min. The existing 200 bps
        // ceiling (validate()) already rejects it — this test locks that the
        // underflow region stays unreachable even if the ceiling is ever raised.
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_risk_yaml().replace("slippage_max_bps: 50", "slippage_max_bps: 10001");
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RiskConfig::load(tmp.path()).is_err());
    }

    #[test]
    fn test_routing_rejects_invalid_submission_style() {
        let mut tmp = NamedTempFile::new().unwrap();
        let yaml = valid_routing_yaml().replace(
            "submission_style: single_atomic_tx",
            "submission_style: flashbots_bundle",
        );
        writeln!(tmp, "{}", yaml).unwrap();
        assert!(RoutingConfig::load(tmp.path()).is_err());
    }

    #[test]
    fn test_disk_risk_yaml_parses() {
        let path = std::path::Path::new("../config/risk.yaml");
        let path = if path.exists() {
            path
        } else {
            std::path::Path::new("config/risk.yaml")
        };
        if path.exists() {
            RiskConfig::load(path).expect("config/risk.yaml must parse as RiskConfig");
        }
    }

    #[test]
    fn test_routing_yaml_new_fields_present() {
        let path = std::path::Path::new("../config/routing.yaml");
        let path = if path.exists() {
            path
        } else {
            std::path::Path::new("config/routing.yaml")
        };
        if path.exists() {
            let cfg = RoutingConfig::load(path).expect("routing.yaml must parse");
            // At least one venue must have the new T3 routing fields populated
            let has_new_fields = cfg
                .venues
                .iter()
                .any(|v| !v.router_address.is_empty() && !v.pairs.is_empty());
            assert!(
                has_new_fields,
                "checked-in routing.yaml must contain router_address + pairs for ≥1 venue"
            );
        }
    }

    #[test]
    fn test_disk_routing_yaml_parses() {
        let path = std::path::Path::new("../config/routing.yaml");
        let path = if path.exists() {
            path
        } else {
            std::path::Path::new("config/routing.yaml")
        };
        if path.exists() {
            RoutingConfig::load(path).expect("config/routing.yaml must parse as RoutingConfig");
        }
    }

    #[test]
    fn test_strategy_params_288_byte_abi_layout() {
        // Build a sample StrategyParams with known address values
        let mut p = StrategyParams::default();
        // collateral_asset = 0x0000...0001 (left-padded address)
        p.collateral_asset[31] = 0x01;
        // user_to_liquidate = 0x0000...0002
        p.user_to_liquidate[31] = 0x02;
        // debt_to_cover = 1000 (little-endian at end of word)
        p.debt_to_cover[28..32].copy_from_slice(&1000u32.to_be_bytes());
        // receive_a_token = 1
        p.receive_a_token[31] = 0x01;
        // dex_router = 0x0000...0003
        p.dex_router[31] = 0x03;
        // amount_out_min = 500
        p.amount_out_min[28..32].copy_from_slice(&500u32.to_be_bytes());
        // min_profit = 10
        p.min_profit[31] = 0x0A;
        // tip = 1
        p.tip[31] = 0x01;
        // deadline = 999999
        p.deadline[24..32].copy_from_slice(&999999u64.to_be_bytes());

        let encoded = p.encode();
        assert_eq!(
            encoded.len(),
            288,
            "StrategyParams must encode to exactly 288 bytes"
        );

        // Offsets within the nine-word strategy tail.
        assert_eq!(&encoded[0..32], &p.collateral_asset, "word0 offset 0");
        assert_eq!(&encoded[32..64], &p.user_to_liquidate, "word1 offset 32");
        assert_eq!(&encoded[64..96], &p.debt_to_cover, "word2 offset 64");
        assert_eq!(&encoded[96..128], &p.receive_a_token, "word3 offset 96");
        assert_eq!(&encoded[128..160], &p.dex_router, "word4 offset 128");
        assert_eq!(&encoded[160..192], &p.amount_out_min, "word5 offset 160");
        assert_eq!(&encoded[192..224], &p.min_profit, "word6 offset 192");
        assert_eq!(&encoded[224..256], &p.tip, "word7 offset 224");
        assert_eq!(&encoded[256..288], &p.deadline, "word8 offset 256");
    }
}
