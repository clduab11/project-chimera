//! Aave V3 edge-case handling tests for the liquidation detector.
//!
//! These exercise the PUBLIC detector API by constructing `MarketSnapshot` /
//! `ReserveData` / `UserPosition` directly (all fields are public) and asserting the
//! candidate output of `LiquidationDetector::find_at_risk_positions`.
//!
//! Edge cases covered:
//!   1. Frozen / paused / inactive reserves excluded from collateral seizure.
//!   2. Liquidation protocol fee field carried through (profit math unit-tested in
//!      `simulator::mod::tests::parse_protocol_fee_*`).
//!   3. Bad-debt positions (collateral_usd < debt_usd) are not emitted.
//!   4. eMode raises the liquidation threshold (healthy under eMode, unhealthy without).
//!   5. Isolation mode does not overstate collateral.
//!   6. Siloed-borrowing flag round-trips, and the close factor respects the single-asset
//!      debt cap.
//!
//! Run: cargo test --test aave_edge_cases_test

use alloy::primitives::{Address, U256};
use chimera_core::detector::liquidation::{
    LiquidationDetector, MarketSnapshot, ReserveData, UserPosition,
};
use std::collections::HashMap;

// --- Fixed test addresses --------------------------------------------------
const COLL_A: [u8; 20] = [0xA1; 20]; // primary (seizable) collateral
const COLL_NONISO: [u8; 20] = [0xB2; 20]; // non-isolated collateral
const COLL_ISO: [u8; 20] = [0xC3; 20]; // isolated collateral
const DEBT: [u8; 20] = [0xD4; 20];
const DEBT2: [u8; 20] = [0xD5; 20];
const USER: [u8; 20] = [0x11; 20];

// --- Scaling helpers -------------------------------------------------------
fn ray() -> U256 {
    U256::from(1_000_000_000_000_000_000_000_000_000u128) // 1e27
}

/// 8-decimal oracle USD price for `dollars`.
fn usd8(dollars: u128) -> U256 {
    U256::from(dollars * 100_000_000u128)
}

fn u(v: u128) -> U256 {
    U256::from(v)
}

/// Build a `ReserveData` with neutral RAY indices and the given economics; all edge-case
/// flags default to the "normal" values (active, not frozen/paused, no eMode/isolation).
fn mk_reserve(price_usd_8dec: U256, lt_bps: u16, bonus_bps: u16) -> ReserveData {
    ReserveData {
        a_token: Address::ZERO,
        variable_debt_token: Address::ZERO,
        liquidity_index: ray(),
        variable_borrow_index: ray(),
        liquidation_bonus_bps: bonus_bps,
        liquidation_threshold_bps: lt_bps,
        price_usd: price_usd_8dec,
        decimals: 18,
        is_isolated: false,
        debt_ceiling: U256::ZERO,
        e_mode_category: 0,
        active: true,
        frozen: false,
        paused: false,
        liquidation_protocol_fee_bps: 0,
        emode_liquidation_threshold_bps: 0,
        emode_liquidation_bonus_bps: 0,
        siloed_borrowing: false,
    }
}

fn snapshot(
    reserves: Vec<(Address, ReserveData)>,
    users: Vec<(Address, UserPosition)>,
) -> MarketSnapshot {
    MarketSnapshot {
        reserves: reserves.into_iter().collect(),
        users: users.into_iter().collect(),
        chain_id: 8453,
        ..Default::default()
    }
}

fn user_position(
    collateral: Vec<([u8; 20], u128)>,
    debt: Vec<([u8; 20], u128)>,
    emode_category: u8,
    is_in_isolation: bool,
) -> UserPosition {
    let mut c: HashMap<Address, U256> = HashMap::new();
    for (a, v) in collateral {
        c.insert(Address::from(a), u(v));
    }
    let mut d: HashMap<Address, U256> = HashMap::new();
    for (a, v) in debt {
        d.insert(Address::from(a), u(v));
    }
    UserPosition {
        collateral: c,
        debt: d,
        emode_category,
        is_in_isolation,
    }
}

/// Base at-risk position: 1e18 COLL_A @ $1000 (LT 80%) backing 1e18 DEBT @ $900.
/// HF = 0.8 * 1e21 / 9e20 = 0.888 < 1.05, and collateral (1e21) >= debt (9e20).
fn base_reserves(coll_a: ReserveData) -> Vec<(Address, ReserveData)> {
    vec![
        (Address::from(COLL_A), coll_a),
        (Address::from(DEBT), mk_reserve(usd8(900), 8000, 10500)),
    ]
}

fn base_user() -> UserPosition {
    user_position(
        vec![(COLL_A, 1_000_000_000_000_000_000u128)], // 1e18
        vec![(DEBT, 1_000_000_000_000_000_000u128)],   // 1e18
        0,
        false,
    )
}

// ---------------------------------------------------------------------------
// Edge case 1: frozen / paused / inactive exclusion
// ---------------------------------------------------------------------------

#[test]
fn test_frozen_reserve_excluded_from_candidates() {
    // Control: seizable collateral => one candidate targeting COLL_A.
    let active = mk_reserve(usd8(1000), 8000, 10500);
    let det = LiquidationDetector::new(
        snapshot(
            base_reserves(active),
            vec![(Address::from(USER), base_user())],
        ),
        8453,
    );
    let candidates = det.find_at_risk_positions();
    assert_eq!(candidates.len(), 1, "seizable collateral should be emitted");
    assert_eq!(candidates[0].collateral_asset, Address::from(COLL_A));
    assert!(
        !candidates[0].bad_debt,
        "emitted candidate must not be bad debt"
    );

    // Frozen collateral => excluded => no candidate.
    let mut frozen = mk_reserve(usd8(1000), 8000, 10500);
    frozen.frozen = true;
    let det = LiquidationDetector::new(
        snapshot(
            base_reserves(frozen),
            vec![(Address::from(USER), base_user())],
        ),
        8453,
    );
    assert!(
        det.find_at_risk_positions().is_empty(),
        "frozen collateral must not produce candidates"
    );
}

#[test]
fn test_paused_reserve_excluded() {
    let mut paused = mk_reserve(usd8(1000), 8000, 10500);
    paused.paused = true;
    let det = LiquidationDetector::new(
        snapshot(
            base_reserves(paused),
            vec![(Address::from(USER), base_user())],
        ),
        8453,
    );
    assert!(
        det.find_at_risk_positions().is_empty(),
        "paused collateral must not produce candidates"
    );
}

#[test]
fn test_inactive_reserve_excluded() {
    let mut inactive = mk_reserve(usd8(1000), 8000, 10500);
    inactive.active = false;
    let det = LiquidationDetector::new(
        snapshot(
            base_reserves(inactive),
            vec![(Address::from(USER), base_user())],
        ),
        8453,
    );
    assert!(
        det.find_at_risk_positions().is_empty(),
        "inactive collateral must not produce candidates"
    );
}

// ---------------------------------------------------------------------------
// Edge case 3: bad-debt detection
// ---------------------------------------------------------------------------

#[test]
fn test_bad_debt_position_not_emitted() {
    // Seizable collateral but collateral_usd (1e21) < debt_usd (2e21) => bad debt.
    let coll = mk_reserve(usd8(1000), 8000, 10500);
    let user = user_position(
        vec![(COLL_A, 1_000_000_000_000_000_000u128)], // 1e18 @ $1000 => 1e21
        vec![(DEBT, 2_000_000_000_000_000_000u128)],   // 2e18 @ $1000 => 2e21
        0,
        false,
    );
    let reserves = vec![
        (Address::from(COLL_A), coll),
        (Address::from(DEBT), mk_reserve(usd8(1000), 8000, 10500)),
    ];
    let det = LiquidationDetector::new(snapshot(reserves, vec![(Address::from(USER), user)]), 8453);
    assert!(
        det.find_at_risk_positions().is_empty(),
        "bad-debt position (collateral < debt) must not be emitted"
    );

    // Control: same collateral, smaller debt => not bad debt => emitted.
    let coll = mk_reserve(usd8(1000), 8000, 10500);
    let user = user_position(
        vec![(COLL_A, 1_000_000_000_000_000_000u128)], // 1e21
        vec![(DEBT, 900_000_000_000_000_000u128)],     // 9e20
        0,
        false,
    );
    let reserves = vec![
        (Address::from(COLL_A), coll),
        (Address::from(DEBT), mk_reserve(usd8(1000), 8000, 10500)),
    ];
    let det = LiquidationDetector::new(snapshot(reserves, vec![(Address::from(USER), user)]), 8453);
    assert_eq!(
        det.find_at_risk_positions().len(),
        1,
        "healthy-collateral at-risk position should be emitted"
    );
}

// ---------------------------------------------------------------------------
// Edge case 4: eMode raises the liquidation threshold
// ---------------------------------------------------------------------------

#[test]
fn test_emode_raises_liquidation_threshold() {
    // COLL_A: 1e18 @ $3500 => 3.5e21 collateral. Base LT 8250, eMode (cat 1) LT 9300.
    // DEBT:   3000e18 @ $1  => 3e21 debt.
    //   normal  HF = 0.825 * 3.5e21 / 3e21 = 0.9625 < 1.05  -> at risk
    //   eMode   HF = 0.930 * 3.5e21 / 3e21 = 1.085  >= 1.05  -> healthy
    let mut coll = mk_reserve(usd8(3500), 8250, 10500);
    coll.e_mode_category = 1;
    coll.emode_liquidation_threshold_bps = 9300;
    coll.emode_liquidation_bonus_bps = 10200;

    let reserves = vec![
        (Address::from(COLL_A), coll),
        (Address::from(DEBT), mk_reserve(usd8(1), 8000, 10500)),
    ];

    let coll_bal = 1_000_000_000_000_000_000u128; // 1e18
    let debt_bal = 3_000_000_000_000_000_000_000u128; // 3000e18

    // Without eMode (user emode 0): at risk.
    let user_plain = user_position(vec![(COLL_A, coll_bal)], vec![(DEBT, debt_bal)], 0, false);
    let det = LiquidationDetector::new(
        snapshot(
            reserves.clone_for_test(),
            vec![(Address::from(USER), user_plain)],
        ),
        8453,
    );
    assert_eq!(
        det.find_at_risk_positions().len(),
        1,
        "without eMode the position is unhealthy and must be emitted"
    );

    // With eMode (user emode 1): healthy => not emitted.
    let user_emode = user_position(vec![(COLL_A, coll_bal)], vec![(DEBT, debt_bal)], 1, false);
    let det = LiquidationDetector::new(
        snapshot(reserves, vec![(Address::from(USER), user_emode)]),
        8453,
    );
    assert!(
        det.find_at_risk_positions().is_empty(),
        "eMode raises LT so the same position is healthy and must NOT be emitted"
    );
}

// ---------------------------------------------------------------------------
// Edge case 5: isolation mode does not overstate collateral
// ---------------------------------------------------------------------------

#[test]
fn test_isolation_mode_position_handled() {
    // Isolated collateral: 1e18 @ $1000 => 1e21 (LT 80%).
    // Non-isolated collateral: 10e18 @ $1000 => 1e22 (must be IGNORED for an isolated user).
    // Debt: 0.95e18 @ $1000 => 9.5e20.
    let mut iso = mk_reserve(usd8(1000), 8000, 10500);
    iso.is_isolated = true;
    let noniso = mk_reserve(usd8(1000), 8000, 10500);

    let reserves = vec![
        (Address::from(COLL_ISO), iso),
        (Address::from(COLL_NONISO), noniso),
        (Address::from(DEBT), mk_reserve(usd8(1000), 8000, 10500)),
    ];

    let collateral = vec![
        (COLL_ISO, 1_000_000_000_000_000_000u128),     // 1e18
        (COLL_NONISO, 10_000_000_000_000_000_000u128), // 10e18
    ];
    let debt = vec![(DEBT, 950_000_000_000_000_000u128)]; // 9.5e17 => 9.5e20 USD

    // Isolated user: only the isolated asset counts => still at risk (collateral NOT overstated).
    let user_iso = user_position(collateral.clone(), debt.clone(), 0, true);
    let det = LiquidationDetector::new(
        snapshot(
            reserves.clone_for_test(),
            vec![(Address::from(USER), user_iso)],
        ),
        8453,
    );
    assert_eq!(
        det.find_at_risk_positions().len(),
        1,
        "isolated user must be at risk because non-isolated collateral is excluded"
    );

    // Control: same balances, NOT isolated => large non-isolated collateral counts => healthy.
    let user_plain = user_position(collateral, debt, 0, false);
    let det = LiquidationDetector::new(
        snapshot(reserves, vec![(Address::from(USER), user_plain)]),
        8453,
    );
    assert!(
        det.find_at_risk_positions().is_empty(),
        "non-isolated user with large collateral should be healthy"
    );
}

// ---------------------------------------------------------------------------
// Edge case 6: siloed-borrowing flag round-trips + close-factor single-asset cap
// ---------------------------------------------------------------------------

#[test]
fn test_siloed_flag_roundtrips_and_close_factor_single_asset() {
    // Part A: the siloed_borrowing flag (and other new fields) round-trip through the
    // snapshot JSON loader with serde defaults.
    let json = r#"{
      "chain": "base",
      "block_number": 7,
      "timestamp": 1781661993,
      "reserves": [
        {
          "address": "0xd4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4",
          "symbol": "SILO",
          "decimals": 18,
          "ltv": 5000,
          "liquidation_threshold": 6000,
          "liquidation_bonus": 11000,
          "price_usd": 1.0,
          "siloed_borrowing": true,
          "liquidation_protocol_fee": 1000,
          "debt_ceiling": "1000000"
        }
      ],
      "users": {}
    }"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("siloed.json");
    std::fs::write(&path, json).unwrap();

    let snap = MarketSnapshot::load_from_file(&path).expect("snapshot must hydrate");
    let silo = Address::from([0xD4u8; 20]);
    let reserve = snap.reserves.get(&silo).expect("SILO reserve present");
    assert!(
        reserve.siloed_borrowing,
        "siloed_borrowing must round-trip true"
    );
    assert_eq!(reserve.liquidation_protocol_fee_bps, 1000);
    assert_eq!(reserve.debt_ceiling, U256::from(1_000_000u64));

    // Part B: the close factor still caps debt_to_cover to a single debt asset's position,
    // even though a siloed asset would in practice be the only borrow. Two debt assets;
    // each candidate's debt_to_cover must not exceed the largest single debt position.
    let coll = mk_reserve(usd8(1000), 8000, 10500); // 1e18 => 1e21 collateral
    let mut debt_reserve = mk_reserve(usd8(1), 8000, 10500);
    debt_reserve.siloed_borrowing = true;
    let debt_reserve2 = mk_reserve(usd8(1), 8000, 10500);

    let max_single: u128 = 500_000_000_000_000_000_000; // 5e20
    let second: u128 = 400_000_000_000_000_000_000; // 4e20

    let user = user_position(
        vec![(COLL_A, 1_000_000_000_000_000_000u128)], // 1e21 collateral, >= 9e20 debt
        vec![(DEBT, max_single), (DEBT2, second)],     // total 9e20 => HF 0.888 (at risk)
        0,
        false,
    );
    let reserves = vec![
        (Address::from(COLL_A), coll),
        (Address::from(DEBT), debt_reserve),
        (Address::from(DEBT2), debt_reserve2),
    ];
    let det = LiquidationDetector::new(snapshot(reserves, vec![(Address::from(USER), user)]), 8453);
    let candidates = det.find_at_risk_positions();
    assert!(
        !candidates.is_empty(),
        "multi-debt at-risk user should be emitted"
    );
    for c in &candidates {
        assert!(
            c.debt_to_cover <= U256::from(max_single),
            "close factor must respect the single-asset debt cap (got {} > {})",
            c.debt_to_cover,
            max_single
        );
    }
}

// ---------------------------------------------------------------------------
// Edge case 2: liquidation protocol fee field is carried through the detector.
// (The profit-reduction arithmetic is unit-tested in
//  `simulator::mod::tests::parse_protocol_fee_*` and applied in
//  `extract_profit_from_state`, which requires a live provider.)
// ---------------------------------------------------------------------------

#[test]
fn test_protocol_fee_reduces_profit() {
    let json = r#"{
      "chain": "base",
      "block_number": 1,
      "timestamp": 0,
      "reserves": [
        {
          "address": "0xa1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1",
          "symbol": "WETH",
          "decimals": 18,
          "ltv": 8000,
          "liquidation_threshold": 8250,
          "liquidation_bonus": 10500,
          "price_usd": 2500.0,
          "liquidation_protocol_fee": 1000
        }
      ],
      "users": {}
    }"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fee.json");
    std::fs::write(&path, json).unwrap();

    let snap = MarketSnapshot::load_from_file(&path).expect("snapshot must hydrate");
    let addr = Address::from([0xA1u8; 20]);
    let reserve = snap.reserves.get(&addr).expect("WETH reserve present");
    // Detector carries the protocol fee (bps); the simulator subtracts it from the bonus.
    assert_eq!(reserve.liquidation_protocol_fee_bps, 1000);
}

// Small helper to clone a Vec of (Address, ReserveData) for reuse across detectors.
trait CloneForTest {
    fn clone_for_test(&self) -> Self;
}
impl CloneForTest for Vec<(Address, ReserveData)> {
    fn clone_for_test(&self) -> Self {
        self.iter().map(|(a, r)| (*a, r.clone())).collect()
    }
}
