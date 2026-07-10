//! Integration test: verify config/pacing.yaml matches Rust defaults.
//! Run with: cargo test --test config_sync_test
//!
//! This is the comprehensive 3-way-sync guard for Invariant #1: the canonical
//! fixture, the on-disk `config/pacing.yaml`, and `PacingConfig::default()` must
//! all agree field-by-field. `PacingConfig` does not derive `PartialEq`, so we
//! compare every field explicitly via `assert_configs_eq` (deriving PartialEq on
//! the struct is out of scope for this file-locked task).

use chimera_core::PacingConfig;
use std::fs;

const CANONICAL_YAML: &str = include_str!("fixtures/pacing_canonical.yaml");

/// Read the on-disk `config/pacing.yaml`, honoring the workspace-root vs
/// crate-root fallback used by the rest of the suite.
fn read_disk_yaml() -> String {
    fs::read_to_string("../config/pacing.yaml")
        .or_else(|_| fs::read_to_string("config/pacing.yaml"))
        .expect("config/pacing.yaml must exist on disk")
}

/// Assert that two `PacingConfig` values are equal field-by-field.
///
/// One `assert_eq!` per field, each naming the field and the comparison
/// context (`ctx`) so a failure pinpoints exactly which value drifted out of
/// 3-way sync. Every field of `PacingConfig` is covered here.
fn assert_configs_eq(a: &PacingConfig, b: &PacingConfig, ctx: &str) {
    assert_eq!(
        a.max_daily_net_usd, b.max_daily_net_usd,
        "[{ctx}] max_daily_net_usd: {} != {}",
        a.max_daily_net_usd, b.max_daily_net_usd
    );
    assert_eq!(
        a.max_weekly_net_usd, b.max_weekly_net_usd,
        "[{ctx}] max_weekly_net_usd: {} != {}",
        a.max_weekly_net_usd, b.max_weekly_net_usd
    );
    assert_eq!(
        a.max_single_transfer_usd, b.max_single_transfer_usd,
        "[{ctx}] max_single_transfer_usd: {} != {}",
        a.max_single_transfer_usd, b.max_single_transfer_usd
    );
    assert_eq!(
        a.min_interval_hours, b.min_interval_hours,
        "[{ctx}] min_interval_hours: {} != {}",
        a.min_interval_hours, b.min_interval_hours
    );
    assert_eq!(
        a.max_jitter_hours, b.max_jitter_hours,
        "[{ctx}] max_jitter_hours: {} != {}",
        a.max_jitter_hours, b.max_jitter_hours
    );
    assert_eq!(
        a.venue_rotation_count, b.venue_rotation_count,
        "[{ctx}] venue_rotation_count: {} != {}",
        a.venue_rotation_count, b.venue_rotation_count
    );
    assert_eq!(
        a.clean_eoa_pool_size, b.clean_eoa_pool_size,
        "[{ctx}] clean_eoa_pool_size: {} != {}",
        a.clean_eoa_pool_size, b.clean_eoa_pool_size
    );
    assert_eq!(
        a.auto_halt_on_reverts, b.auto_halt_on_reverts,
        "[{ctx}] auto_halt_on_reverts: {} != {}",
        a.auto_halt_on_reverts, b.auto_halt_on_reverts
    );
    assert_eq!(
        a.max_gas_gwei, b.max_gas_gwei,
        "[{ctx}] max_gas_gwei: {} != {}",
        a.max_gas_gwei, b.max_gas_gwei
    );
    assert_eq!(
        a.max_daily_loss_eth, b.max_daily_loss_eth,
        "[{ctx}] max_daily_loss_eth: {} != {}",
        a.max_daily_loss_eth, b.max_daily_loss_eth
    );
    assert_eq!(
        a.min_profit_multiplier, b.min_profit_multiplier,
        "[{ctx}] min_profit_multiplier: {} != {}",
        a.min_profit_multiplier, b.min_profit_multiplier
    );
    assert_eq!(
        a.execute_mode, b.execute_mode,
        "[{ctx}] execute_mode: {} != {}",
        a.execute_mode, b.execute_mode
    );
    assert_eq!(
        a.log_level, b.log_level,
        "[{ctx}] log_level: {} != {}",
        a.log_level, b.log_level
    );
    assert_eq!(
        a.metrics_port, b.metrics_port,
        "[{ctx}] metrics_port: {} != {}",
        a.metrics_port, b.metrics_port
    );
    assert_eq!(
        a.chain_id, b.chain_id,
        "[{ctx}] chain_id: {} != {}",
        a.chain_id, b.chain_id
    );
    assert_eq!(
        a.oracle_staleness_seconds, b.oracle_staleness_seconds,
        "[{ctx}] oracle_staleness_seconds: {} != {}",
        a.oracle_staleness_seconds, b.oracle_staleness_seconds
    );
    assert_eq!(
        a.eth_price_usd_fallback, b.eth_price_usd_fallback,
        "[{ctx}] eth_price_usd_fallback: {} != {}",
        a.eth_price_usd_fallback, b.eth_price_usd_fallback
    );
    assert_eq!(
        a.eth_usd_feed_address, b.eth_usd_feed_address,
        "[{ctx}] eth_usd_feed_address: {} != {}",
        a.eth_usd_feed_address, b.eth_usd_feed_address
    );
    assert_eq!(
        a.recent_outcomes_capacity, b.recent_outcomes_capacity,
        "[{ctx}] recent_outcomes_capacity: {} != {}",
        a.recent_outcomes_capacity, b.recent_outcomes_capacity
    );
    assert_eq!(
        a.eoa_pool_path, b.eoa_pool_path,
        "[{ctx}] eoa_pool_path: {} != {}",
        a.eoa_pool_path, b.eoa_pool_path
    );
    assert_eq!(
        a.pools_toml_path, b.pools_toml_path,
        "[{ctx}] pools_toml_path: {} != {}",
        a.pools_toml_path, b.pools_toml_path
    );
    assert_eq!(
        a.executor_address, b.executor_address,
        "[{ctx}] executor_address: {} != {}",
        a.executor_address, b.executor_address
    );
    assert_eq!(
        a.treasury_address, b.treasury_address,
        "[{ctx}] treasury_address: {} != {}",
        a.treasury_address, b.treasury_address
    );
    assert_eq!(
        a.treasury_keystore, b.treasury_keystore,
        "[{ctx}] treasury_keystore: {} != {}",
        a.treasury_keystore, b.treasury_keystore
    );
    assert_eq!(
        a.worker_keystore_dir, b.worker_keystore_dir,
        "[{ctx}] worker_keystore_dir: {} != {}",
        a.worker_keystore_dir, b.worker_keystore_dir
    );
    assert_eq!(
        a.sweep_interval_secs, b.sweep_interval_secs,
        "[{ctx}] sweep_interval_secs: {} != {}",
        a.sweep_interval_secs, b.sweep_interval_secs
    );
    assert_eq!(
        a.refund_interval_secs, b.refund_interval_secs,
        "[{ctx}] refund_interval_secs: {} != {}",
        a.refund_interval_secs, b.refund_interval_secs
    );
    assert_eq!(
        a.min_worker_balance_eth, b.min_worker_balance_eth,
        "[{ctx}] min_worker_balance_eth: {} != {}",
        a.min_worker_balance_eth, b.min_worker_balance_eth
    );
    assert_eq!(
        a.refund_topup_eth, b.refund_topup_eth,
        "[{ctx}] refund_topup_eth: {} != {}",
        a.refund_topup_eth, b.refund_topup_eth
    );
    assert_eq!(
        a.sweep_tokens, b.sweep_tokens,
        "[{ctx}] sweep_tokens: {:?} != {:?}",
        a.sweep_tokens, b.sweep_tokens
    );
    assert_eq!(
        a.sweep_min_keep_eth, b.sweep_min_keep_eth,
        "[{ctx}] sweep_min_keep_eth: {} != {}",
        a.sweep_min_keep_eth, b.sweep_min_keep_eth
    );
    assert_eq!(
        a.ws_endpoint, b.ws_endpoint,
        "[{ctx}] ws_endpoint: {} != {}",
        a.ws_endpoint, b.ws_endpoint
    );
}

#[test]
fn pacing_yaml_matches_rust_defaults() {
    let canonical: PacingConfig =
        serde_yaml::from_str(CANONICAL_YAML).expect("Canonical YAML must parse");
    let defaults = PacingConfig::default();

    // Comprehensive 3-way-sync guard: every field of the canonical fixture must
    // equal the corresponding PacingConfig::default() field.
    assert_configs_eq(&canonical, &defaults, "canonical vs defaults");
}

#[test]
fn disk_yaml_matches_canonical() {
    let on_disk: PacingConfig = serde_yaml::from_str(&read_disk_yaml())
        .expect("config/pacing.yaml must parse as valid PacingConfig");
    let canonical: PacingConfig =
        serde_yaml::from_str(CANONICAL_YAML).expect("Canonical YAML must parse");

    // Full field coverage: on-disk config must equal the canonical fixture.
    assert_configs_eq(&on_disk, &canonical, "disk vs canonical");
}

#[test]
fn canonical_matches_disk_and_defaults_all_fields() {
    // Single authoritative triple-check: load all three sources and assert that
    // all three agree field-by-field (Invariant #1).
    let defaults = PacingConfig::default();
    let canonical: PacingConfig =
        serde_yaml::from_str(CANONICAL_YAML).expect("Canonical YAML must parse");
    let on_disk: PacingConfig = serde_yaml::from_str(&read_disk_yaml())
        .expect("config/pacing.yaml must parse as valid PacingConfig");

    assert_configs_eq(&canonical, &defaults, "canonical vs defaults");
    assert_configs_eq(&on_disk, &canonical, "disk vs canonical");
    assert_configs_eq(&on_disk, &defaults, "disk vs defaults");
}
