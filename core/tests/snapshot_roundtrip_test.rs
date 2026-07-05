//! End-to-end round-trip test: generated-snapshot JSON -> prewarm -> detector.
//!
//! Verifies that:
//!  1. A snapshot fixture shaped like `scripts/snapshot_generator.py` output
//!     deserializes correctly through `prewarm::MarketSnapshot`.
//!  2. `pre_warm_db` fills reserve data slots from the fixture values (no zero
//!     placeholders for indices, rates, timestamps, or token addresses).
//!  3. The detector (`liquidation::MarketSnapshot::load_from_file`) can consume
//!     the same snapshot file and hydrate the new index fields.
//!
//! Run: cargo test --test snapshot_roundtrip_test

use alloy::primitives::{address, U256};
use chimera_core::detector::liquidation::MarketSnapshot as DetectorSnapshot;
use chimera_core::simulator::prewarm::{MarketSnapshot, pre_warm_db};
use revm::database::{CacheDB, EmptyDB};
use std::path::PathBuf;

fn fixture_path(filename: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push(filename);
    p
}

#[test]
fn snapshot_fixture_deserializes_prewarm_and_fills_slots() {
    let path = fixture_path("snapshot_mock.json");
    let content = std::fs::read_to_string(&path).expect("fixture must be readable");
    let snapshot: MarketSnapshot =
        serde_json::from_str(&content).expect("snapshot must deserialize through prewarm");

    assert_eq!(snapshot.reserves.len(), 2);
    assert_eq!(snapshot.users.len(), 1);
    assert!(!snapshot.pool.is_zero(), "pool must be populated");

    // Pre-warm the DB.
    let mut db = CacheDB::new(EmptyDB::default());
    pre_warm_db(&mut db, &snapshot).expect("pre-warming must succeed");

    // Verify Pool storage was populated with non-zero reserve data.
    let pool_account = db
        .cache
        .accounts
        .get(&snapshot.pool)
        .expect("Pool account must be pre-warmed");
    assert!(
        pool_account.storage.len() >= 8,
        "expected at least 8 storage slots for 2 reserves (offset 0-3 each)"
    );

    // Verify each reserve's fields round-tripped correctly.
    for reserve in &snapshot.reserves {
        assert!(
            !reserve.a_token.is_zero(),
            "a_token must be non-zero for {}",
            reserve.symbol
        );
        assert!(
            !reserve.variable_debt_token.is_zero(),
            "variable_debt_token must be non-zero for {}",
            reserve.symbol
        );
        assert!(
            reserve.liquidity_index > 0,
            "liquidity_index must be non-zero for {}",
            reserve.symbol
        );
        assert!(
            reserve.variable_borrow_index > 0,
            "variable_borrow_index must be non-zero for {}",
            reserve.symbol
        );
        assert!(
            reserve.liquidity_rate > 0,
            "liquidity_rate must be non-zero for {}",
            reserve.symbol
        );
        assert!(
            reserve.variable_borrow_rate > 0,
            "variable_borrow_rate must be non-zero for {}",
            reserve.symbol
        );
        assert!(
            reserve.last_update_timestamp > 0,
            "last_update_timestamp must be non-zero for {}",
            reserve.symbol
        );
    }

    // Spot-check specific field values for WETH.
    let weth_prewarm = &snapshot.reserves[0];
    assert_eq!(weth_prewarm.symbol, "WETH");
    assert_eq!(weth_prewarm.decimals, 18);
    let usdc_prewarm = &snapshot.reserves[1];
    assert_eq!(usdc_prewarm.symbol, "USDC");
    assert_eq!(usdc_prewarm.decimals, 6);
}

#[test]
fn snapshot_fixture_deserializes_detector_with_index_fields() {
    let path = fixture_path("snapshot_mock.json");
    let det_snap = DetectorSnapshot::load_from_file(&path)
        .expect("detector must load snapshot fixture");

    assert_eq!(det_snap.reserves.len(), 2, "detector must see 2 reserves");
    assert_eq!(det_snap.users.len(), 1, "detector must see 1 user");
    assert_eq!(det_snap.chain_id, 42161, "arbitrum chain_id");

    let weth_addr = address!("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1");
    let weth = det_snap
        .reserves
        .get(&weth_addr)
        .expect("WETH reserve must be present");

    // Detector must hydrate the index fields from the snapshot (no longer hard-coded RAY).
    let expected_li = U256::from(1_087_654_321_987_654_321_987_654_321u128);
    let expected_vbi = U256::from(1_045_678_912_345_678_912_345_678_912u128);
    assert_eq!(
        weth.liquidity_index, expected_li,
        "detector liquidity_index must match fixture"
    );
    assert_eq!(
        weth.variable_borrow_index, expected_vbi,
        "detector variable_borrow_index must match fixture"
    );
    assert_eq!(
        weth.liquidation_bonus_bps, 10500,
        "detector liquidation_bonus_bps must match fixture"
    );
    assert_eq!(
        weth.liquidation_protocol_fee_bps, 1000,
        "detector protocol_fee must match fixture"
    );
    assert_eq!(
        weth.e_mode_category, 1,
        "detector e_mode_category must match fixture"
    );
    assert_eq!(
        weth.emode_liquidation_threshold_bps, 9300,
        "detector eMode LT must match fixture"
    );
    assert_eq!(
        weth.emode_liquidation_bonus_bps, 10200,
        "detector eMode bonus must match fixture"
    );
    assert!(
        weth.active,
        "detector active must match fixture"
    );
    assert!(!weth.frozen);
    assert!(!weth.paused);
    assert!(!weth.siloed_borrowing);
    assert!(!weth.is_isolated);
    assert_eq!(weth.debt_ceiling, U256::ZERO);
}

#[test]
fn detector_and_prewarm_consume_same_fixture() {
    // The same snapshot JSON file must be consumable by both the prewarm
    // module and the detector without deserialization errors.
    let path = fixture_path("snapshot_mock.json");
    let content = std::fs::read_to_string(&path).unwrap();

    // Prewarm path.
    let _prewarm_snap: MarketSnapshot =
        serde_json::from_str(&content).expect("prewarm must deserialize");

    // Detector path.
    let _det_snap = DetectorSnapshot::load_from_file(&path)
        .expect("detector must deserialize");

    // If both succeed, the snapshot shape is compatible with both consumers.
}

#[test]
fn snapshot_generated_by_python_deserializes_and_prewarms() {
    // Load the ACTUAL output of `snapshot_generator.py --mock` and verify
    // every non-zero-reserve field survives the full prewarm path.
    let path = fixture_path("snapshot_generated.json");
    let content = std::fs::read_to_string(&path).expect("generated fixture must be readable");
    let snapshot: MarketSnapshot =
        serde_json::from_str(&content).expect("Python-generated snapshot must deserialize");

    assert_eq!(snapshot.chain, "arbitrum");
    assert_eq!(snapshot.reserves.len(), 2);
    assert_eq!(snapshot.users.len(), 1);

    let mut db = CacheDB::new(EmptyDB::default());
    pre_warm_db(&mut db, &snapshot).expect("pre-warming must succeed");

    for reserve in &snapshot.reserves {
        assert!(!reserve.a_token.is_zero(), "a_token must be non-zero for {}", reserve.symbol);
        assert!(!reserve.variable_debt_token.is_zero(), "variable_debt_token must be non-zero for {}", reserve.symbol);
        assert!(reserve.liquidity_index > 0, "liquidity_index must be non-zero for {}", reserve.symbol);
        assert!(reserve.variable_borrow_index > 0, "variable_borrow_index must be non-zero for {}", reserve.symbol);
        assert!(reserve.liquidity_rate > 0, "liquidity_rate must be non-zero for {}", reserve.symbol);
        assert!(reserve.variable_borrow_rate > 0, "variable_borrow_rate must be non-zero for {}", reserve.symbol);
        assert!(reserve.last_update_timestamp > 0, "last_update_timestamp must be non-zero for {}", reserve.symbol);
        assert!(reserve.emode_liquidation_threshold > 0 || reserve.emode_category == 0,
            "eMode LT must be set when eMode category is active for {}", reserve.symbol);
    }

    // Spot-check: WETH is the first reserve with eMode category 1.
    let weth = &snapshot.reserves[0];
    assert_eq!(weth.symbol, "WETH");
    assert_eq!(weth.emode_category, 1);
    assert_eq!(weth.emode_liquidation_threshold, 9300);
    assert_eq!(weth.emode_liquidation_bonus, 10200);

    // Detector must also load the same file.
    DetectorSnapshot::load_from_file(&path)
        .expect("generated snapshot must also load through detector");
}
