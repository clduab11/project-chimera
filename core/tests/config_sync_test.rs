//! Integration test: verify config/pacing.yaml matches Rust defaults.
//! Run with: cargo test --test config_sync_test

use chimera_core::PacingConfig;
use std::fs;

const CANONICAL_YAML: &str = include_str!("fixtures/pacing_canonical.yaml");

#[test]
fn pacing_yaml_matches_rust_defaults() {
    let canonical: PacingConfig = serde_yaml::from_str(CANONICAL_YAML)
        .expect("Canonical YAML must parse");
    let defaults = PacingConfig::default();

    assert_eq!(canonical.max_daily_net_usd, defaults.max_daily_net_usd,
        "pacing.yaml max_daily_net_usd ({}) != Rust default ({})",
        canonical.max_daily_net_usd, defaults.max_daily_net_usd);
    assert_eq!(canonical.max_weekly_net_usd, defaults.max_weekly_net_usd);
    assert_eq!(canonical.max_single_transfer_usd, defaults.max_single_transfer_usd);
    assert_eq!(canonical.eth_price_usd_fallback, defaults.eth_price_usd_fallback,
        "pacing.yaml eth_price_usd_fallback ({}) != Rust default ({})",
        canonical.eth_price_usd_fallback, defaults.eth_price_usd_fallback);
}

#[test]
fn disk_yaml_matches_canonical() {
    let disk = fs::read_to_string("../config/pacing.yaml")
        .or_else(|_| fs::read_to_string("config/pacing.yaml"))
        .expect("config/pacing.yaml must exist on disk");
    let on_disk: PacingConfig = serde_yaml::from_str(&disk)
        .expect("config/pacing.yaml must parse as valid PacingConfig");
    let canonical: PacingConfig = serde_yaml::from_str(CANONICAL_YAML)
        .expect("Canonical YAML must parse");

    assert_eq!(on_disk.max_daily_net_usd, canonical.max_daily_net_usd);
    assert_eq!(on_disk.eth_price_usd_fallback, canonical.eth_price_usd_fallback);
}
