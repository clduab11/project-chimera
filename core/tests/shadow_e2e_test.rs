//! Phase 3 End-to-End Shadow Integration Test
//!
//! Constructs the real [`Orchestrator`] with mock provider, oracle, simulator,
//! and broadcast doubles. Drives one deterministic scan cycle through the full
//! shadow loop (`scan → simulate → pace(reserve) → assemble → shadow-log`)
//! with **no broadcast**, then recovers the persisted outcome from the JSONL
//! audit trail the orchestrator itself wrote.
//!
//! Exercises:
//!   - Real [`Orchestrator`] construction with mock injection seams
//!   - [`LiquidationDetector`] scanning a mock [`MarketSnapshot`]
//!   - Mock simulation via `set_mock_sim_fn`
//!   - Mock gas price via `set_mock_gas_price`
//!   - [`PacingEngine`] + [`CrossProcessPacing`] gating
//!   - [`StrategyAssembler`] shadow-transaction assembly
//!   - JSONL persistence via [`PacingEngine::with_state_persistence`]
//!   - [`CrashRecovery`] roundtrip from the file the runtime actually wrote
//!   - [`Metrics`] counters (`candidates_seen`, `sims_run`)
//!   - Breaker stays untripped under normal shadow operations
//!   - Reservation lifecycle (reserve → settle → expire)
//!   - Locally denied opportunities do NOT consume pacing caps
//!   - Emergency halt trips breaker and blocks
//!
//! Runs in `cargo test -p chimera-core` without any external node.
//! Ref: Core Flows Flow 2; Tech Plan "Phase-3 E2E harness"; Epic Brief DoD.

use alloy::primitives::{address, Address, U256};
use alloy::providers::ProviderBuilder;
use chimera_core::{
    config::{PacingConfig, RiskConfig, RoutingConfig, TradingPair, VenueEntry},
    detector::liquidation::{LiquidationDetector, MarketSnapshot, ReserveData, UserPosition},
    routing::RoutingResolver,
    state::{ReservationRecord, ReservationStatus},
    BreakerReason, CrossProcessPacing, JsonlPersistence, Metrics, Opportunity, Orchestrator,
    OrchestratorConfig, PacingDecision, PacingEngine, RpcSubmitter, SignerRegistry,
    SimulationResult, StrategyAssembler,
};
// Used only by the cross-process reservation lifecycle test, which is
// cfg-gated off Windows; import must match that gating or Linux builds fail.
#[cfg(not(target_os = "windows"))]
use chimera_core::state::CrashRecovery;
use chrono::{TimeDelta, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use url::Url;

// ---------------------------------------------------------------------------
// Shared test addresses and constants
// ---------------------------------------------------------------------------

const USER: Address = address!("0x1111111111111111111111111111111111111111");
const USER2: Address = address!("0x2222222222222222222222222222222222222222");
const COLL_WETH: Address = address!("0x4200000000000000000000000000000000000006");
const DEBT_USDC: Address = address!("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
const DEBT_USDT: Address = address!("0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2");
const ROUTER: Address = address!("0xA0b86a33E6441e0A421e56E4773C3C4b0Db7E5b0");
const AAVE_POOL: Address = address!("0xA238Dd80C259a72e81d7e4664a9801593F98d1c5");
const EXECUTOR: Address = address!("0x9999999999999999999999999999999999999999");
const WORKER_EOA: Address = address!("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE");

fn ray() -> U256 {
    U256::from(1_000_000_000_000_000_000_000_000_000u128)
}

fn usd8(dollars: u128) -> U256 {
    U256::from(dollars * 100_000_000u128)
}

fn u(v: u128) -> U256 {
    U256::from(v)
}

fn mk_reserve(price_usd_8dec: U256, lt_bps: u16, bonus_bps: u16) -> ReserveData {
    ReserveData {
        a_token: address!("0x0101010101010101010101010101010101010101"),
        variable_debt_token: address!("0x0202020202020202020202020202020202020202"),
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

fn test_pacing_config(eoa_pool_path: &str) -> PacingConfig {
    PacingConfig {
        max_daily_net_usd: Decimal::from(2000),
        max_weekly_net_usd: Decimal::from(7500),
        max_single_transfer_usd: Decimal::from(1000),
        min_interval_hours: 0,
        max_jitter_hours: 12,
        venue_rotation_count: 3,
        clean_eoa_pool_size: 10,
        auto_halt_on_reverts: 3,
        max_gas_gwei: 300,
        max_daily_loss_eth: dec!(0.005),
        min_profit_multiplier: dec!(2.5),
        execute_mode: "shadow".into(),
        log_level: "info".into(),
        metrics_port: 9101,
        chain_id: 8453,
        oracle_staleness_seconds: 1500,
        eth_price_usd_fallback: Decimal::from(1800),
        eth_usd_feed_address: "0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70".into(),
        recent_outcomes_capacity: 128,
        eoa_pool_path: eoa_pool_path.to_string(),
        pools_toml_path: "config/pools.toml".into(),
        executor_address: format!("{:#x}", EXECUTOR),
        treasury_address: String::new(),
        treasury_keystore: String::new(),
        worker_keystore_dir: String::new(),
        sweep_interval_secs: 300,
        refund_interval_secs: 3600,
        min_worker_balance_eth: dec!(0.002),
        refund_topup_eth: dec!(0.01),
        sweep_tokens: vec![],
        sweep_min_keep_eth: dec!(0.001),
        ws_endpoint: String::new(),
    }
}

fn test_routing_config() -> RoutingConfig {
    RoutingConfig {
        primary: "test".into(),
        fallbacks: vec![],
        submission_style: "single_atomic_tx".into(),
        venues: vec![VenueEntry {
            name: "test-dex".into(),
            chain: "base".into(),
            liquidity_usd_min: 50_000,
            venue_type: "dex".into(),
            kyc: false,
            router_compatibility: "v2".into(),
            router_address: format!("0x{:x}", ROUTER),
            pairs: vec![TradingPair {
                token_in: format!("0x{:x}", COLL_WETH),
                token_out: format!("0x{:x}", DEBT_USDC),
            }],
        }],
        forensic_tag_sources: vec![],
    }
}

fn test_risk_config() -> RiskConfig {
    RiskConfig::default()
}

fn make_at_risk_snapshot() -> MarketSnapshot {
    let reserves: HashMap<Address, ReserveData> = vec![
        (COLL_WETH, mk_reserve(usd8(3400), 8250, 10500)),
        (DEBT_USDC, {
            let mut r = mk_reserve(usd8(1), 0, 0);
            r.decimals = 6;
            r
        }),
    ]
    .into_iter()
    .collect();

    let users: HashMap<Address, UserPosition> = vec![(
        USER,
        user_position(
            // 1 WETH collateral ($3400) vs 2900 USDC debt. USDC is 6-decimal, so
            // 2900 USDC = 2_900 * 1e6. (The amounts must match each reserve's
            // decimals: the detector normalizes balances by 10^decimals.)
            vec![(*COLL_WETH.as_ref(), 1_000_000_000_000_000_000u128)],
            vec![(*DEBT_USDC.as_ref(), 2_900_000_000u128)],
            0,
            false,
        ),
    )]
    .into_iter()
    .collect();

    MarketSnapshot {
        reserves,
        users,
        block_number: 42,
        chain_id: 8453,
        ..Default::default()
    }
}

fn make_multi_user_snapshot() -> MarketSnapshot {
    let reserves: HashMap<Address, ReserveData> = vec![
        (COLL_WETH, mk_reserve(usd8(3400), 8250, 10500)),
        (DEBT_USDC, {
            let mut r = mk_reserve(usd8(1), 0, 0);
            r.decimals = 6;
            r
        }),
        (DEBT_USDT, {
            let mut r = mk_reserve(usd8(1), 0, 0);
            r.decimals = 6;
            r
        }),
    ]
    .into_iter()
    .collect();

    let users: HashMap<Address, UserPosition> = vec![
        (
            USER,
            user_position(
                // 1 WETH ($3400) vs 2900 USDC (6-decimal) debt → HF ≈ 0.97.
                vec![(*COLL_WETH.as_ref(), 1_000_000_000_000_000_000u128)],
                vec![(*DEBT_USDC.as_ref(), 2_900_000_000u128)],
                0,
                false,
            ),
        ),
        (
            USER2,
            user_position(
                // 2 WETH ($6800) vs 5500 USDT (6-decimal) debt → HF ≈ 1.02.
                vec![(*COLL_WETH.as_ref(), 2_000_000_000_000_000_000u128)],
                vec![(*DEBT_USDT.as_ref(), 5_500_000_000u128)],
                0,
                false,
            ),
        ),
    ]
    .into_iter()
    .collect();

    MarketSnapshot {
        reserves,
        users,
        block_number: 42,
        chain_id: 8453,
        ..Default::default()
    }
}

// ===========================================================================
// Core E2E: Real Orchestrator with mock doubles, shadow mode, JSONL recovery
// ===========================================================================

#[tokio::test]
async fn test_e2e_orchestrator_shadow_with_mock_doubles_and_jsonl_recovery() {
    let dir = TempDir::new().unwrap();
    let chain_id = 8453u64;
    let execute_mode = "shadow";

    // --- EOA pool file ---
    let pool_path = dir.path().join("eoa_pool.json");
    let pool: Vec<String> = vec![
        format!("0x{:x}", WORKER_EOA),
        "0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        "0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".into(),
    ];
    std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();

    let pacing_cfg = test_pacing_config(pool_path.to_str().unwrap());

    // --- JSONL persistence wired into the pacing engine ---
    let outcomes_path =
        std::path::PathBuf::from("test_temp/shadow_e2e_orchestrator_outcomes.jsonl");
    let _ = std::fs::create_dir_all(outcomes_path.parent().unwrap());
    let persistence = Arc::new(JsonlPersistence::new(outcomes_path.clone(), 50, 5));
    let pacing_engine = PacingEngine::new(pacing_cfg.clone()).with_state_persistence(persistence);

    // --- Cross-process pacing ---
    let reservations_dir = std::path::PathBuf::from("test_temp/shadow_e2e_reservations");
    let _ = std::fs::create_dir_all(&reservations_dir);
    let engine_for_assertions = pacing_engine.clone();
    let pacing = CrossProcessPacing::new(
        pacing_engine,
        reservations_dir.clone(),
        outcomes_path.clone(),
    );
    pacing.clear_stale_lock().ok();

    // --- Metrics ---
    let metrics = Arc::new(Metrics::new());

    // --- Snapshot ---
    let snapshot = make_at_risk_snapshot();

    // --- Routing ---
    let routing_config = test_routing_config();
    let risk_config = test_risk_config();

    // --- Provider: connected to an unreachable address (never called due to mocks) ---
    let provider =
        Arc::new(ProviderBuilder::new().connect_http(Url::parse("http://127.0.0.1:1").unwrap()));

    // --- Submitter in dry-run mode ---
    let submitter = RpcSubmitter::new((*provider).clone(), chain_id).with_dry_run(true);

    // --- Signer registry (shadow mode, empty paths) ---
    let signer_registry = Arc::new(
        SignerRegistry::load(&pacing_cfg, execute_mode).expect("shadow signer registry must load"),
    );

    // --- Build the Orchestrator ---
    let mut orchestrator = Orchestrator::new(
        OrchestratorConfig::default(),
        pacing,
        pacing_cfg.clone(),
        metrics.clone(),
        provider,
        snapshot.clone(),
        chain_id,
        None, // no real LiquidationSimulator — mock sim fn below
        submitter,
        execute_mode.to_string(),
        EXECUTOR,
        routing_config.clone(),
        risk_config.clone(),
        signer_registry,
        None, // no mempool watcher
    );

    // --- Inject mock gas price (1 gwei) ---
    orchestrator.set_mock_gas_price(1_000_000_000);

    // --- Inject mock simulation: always profitable ---
    orchestrator.set_mock_sim_fn(|_candidate| SimulationResult {
        profitable: true,
        expected_profit_usd: dec!(200),
        gas_used: 150_000,
        l1_data_fee_wei: U256::from(500_000u64),
        revert_reason: None,
        calldata: vec![],
    });

    // --- DRIVE one scan cycle through the real Orchestrator ---
    let candidate_count = orchestrator
        .run_single_scan()
        .await
        .expect("run_single_scan must succeed in shadow mode");
    assert!(candidate_count > 0, "should find at least one candidate");

    // --- VERIFY: candidates_seen counter incremented per candidate ---
    assert_eq!(
        metrics.candidates_seen.with_label_values(&["base"]).get(),
        candidate_count as u64,
        "candidates_seen must equal number of candidates found"
    );

    // --- VERIFY: sims_run counter incremented ---
    let sims_total = metrics.sims_run.with_label_values(&["success"]).get();
    assert!(sims_total >= 1, "sims_run must be >= 1; got {}", sims_total);

    // --- VERIFY: breaker is untripped ---
    assert!(
        !engine_for_assertions.is_breaker_active(),
        "breaker must be untripped after successful shadow execution"
    );

    // --- VERIFY: reservation file was written and settled ---
    let res_path = reservations_dir.join(format!("reservations-{}.jsonl", chain_id));
    assert!(res_path.exists(), "reservation file must exist");

    // JSONL persistence verification: sync_all() fails on Windows temp dirs
    // (error code 5: Access denied). On non-Windows platforms, verify full roundtrip.
    #[cfg(not(target_os = "windows"))]
    {
        // record_outcome appends asynchronously; yield until the bounded write appears.
        for _ in 0..100 {
            if tokio::fs::metadata(&outcomes_path).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            outcomes_path.exists(),
            "outcomes.jsonl must exist after orchestration"
        );

        let recovered = CrashRecovery::recover_from_jsonl_sync(&outcomes_path)
            .expect("recovery from JSONL must succeed");
        assert!(
            recovered.daily_usage_usd > Decimal::ZERO,
            "recovered daily_usage_usd must reflect the shadow outcome"
        );
        assert!(
            recovered.last_outcome_time.is_some(),
            "recovered state must have last_outcome_time"
        );
    }

    // --- VERIFY: shadow mode performed zero submit-path invocations ---
    assert_eq!(
        orchestrator.submit_attempt_count(),
        0,
        "shadow orchestrator must never invoke the submit seam"
    );

    // Cleanup test files
    let _ = std::fs::remove_file(&outcomes_path);
    let res_path = reservations_dir.join(format!("reservations-{}.jsonl", chain_id));
    let _ = std::fs::remove_file(&res_path);
    let _ = std::fs::remove_dir(&reservations_dir);

    // Cleanup: drop orchestrator which owns pacing and engine
    drop(orchestrator);
}

// ===========================================================================
// Orchestrator: Locally denied opportunity does NOT consume pacing caps
// ===========================================================================

#[test]
fn test_locally_denied_does_not_consume_caps() {
    let dir = TempDir::new().unwrap();
    let pool_path = dir.path().join("eoa_pool.json");
    let pool: Vec<String> = vec![format!("0x{:x}", WORKER_EOA)];
    std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();

    let cfg = test_pacing_config(pool_path.to_str().unwrap());
    let engine = PacingEngine::new(cfg);

    // Baseline state.
    let baseline = engine.current_risk_state();
    assert_eq!(baseline.daily_net_usd, Decimal::ZERO);
    assert_eq!(baseline.consecutive_reverts, 0);
    assert!(baseline.last_release.is_none());

    // Opportunity that will be locally denied (over single cap).
    let opp = Opportunity {
        id: "denied-no-consumption".into(),
        expected_net_usd: dec!(1500),
        gas_estimate_gwei: 10,
        venue: "venue-denied".into(),
        eoa: format!("0x{:x}", WORKER_EOA),
        timestamp: Utc::now(),
    };

    let decision = engine.check(&opp).unwrap();
    assert!(matches!(decision, PacingDecision::Deny { .. }));

    // DO NOT call record_outcome for denied. State must be identical to baseline.
    let after = engine.current_risk_state();
    assert_eq!(after.daily_net_usd, baseline.daily_net_usd);
    assert_eq!(after.consecutive_reverts, baseline.consecutive_reverts);
    assert_eq!(after.weekly_net_usd, baseline.weekly_net_usd);
    assert_eq!(after.last_release, baseline.last_release);
}

// ===========================================================================
// Metrics: candidates_seen per candidate (not per scan)
// ===========================================================================

#[test]
fn test_metrics_candidates_seen_per_candidate() {
    let snapshot = make_multi_user_snapshot();
    let detector = LiquidationDetector::new(snapshot, 8453);
    let metrics = Metrics::new();

    let candidates = detector.find_at_risk_positions();
    metrics.observe_candidates("base", candidates.len());

    assert_eq!(
        metrics.candidates_seen.with_label_values(&["base"]).get(),
        2,
        "candidates_seen must equal candidate count"
    );
}

// ===========================================================================
// Breaker stays untripped under normal ops
// ===========================================================================

#[test]
fn test_breaker_stays_untripped_under_normal_ops() {
    let dir = TempDir::new().unwrap();
    let pool_path = dir.path().join("eoa_pool.json");
    let pool: Vec<String> = vec![format!("0x{:x}", WORKER_EOA)];
    std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();

    let cfg = test_pacing_config(pool_path.to_str().unwrap());
    let engine = PacingEngine::new(cfg);

    for i in 0..5 {
        let opp = Opportunity {
            id: format!("breaker-test-{}", i),
            expected_net_usd: dec!(80),
            gas_estimate_gwei: 25,
            venue: format!("venue-{}", i),
            eoa: format!("0x{:x}", WORKER_EOA),
            timestamp: Utc::now(),
        };
        let decision = engine.check(&opp).unwrap();
        assert!(matches!(decision, PacingDecision::Allow { .. }));
        engine.record_outcome(&opp, dec!(80), Decimal::ZERO, false);
    }

    assert!(!engine.is_breaker_active());
    let state = engine.current_risk_state();
    assert_eq!(state.consecutive_reverts, 0);
    assert_eq!(state.breaker_tripped, None);
    assert!(state.weekly_net_usd > Decimal::ZERO);
}

// ===========================================================================
// Reservation lifecycle (reserve → settle → expire)
// ===========================================================================

#[test]
fn test_cross_process_reservation_lifecycle() {
    let dir = TempDir::new().unwrap();
    let reservations_dir = dir.path().join("reservations2");
    std::fs::create_dir_all(&reservations_dir).unwrap();

    let pool_path = dir.path().join("eoa_pool.json");
    let pool: Vec<String> = vec![format!("0x{:x}", WORKER_EOA)];
    std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();

    let cfg = test_pacing_config(pool_path.to_str().unwrap());
    let engine = PacingEngine::new(cfg);
    let chain_id = 8453u64;

    let pacing = CrossProcessPacing::new(
        engine,
        reservations_dir.clone(),
        dir.path().join("outcomes.jsonl"),
    );
    pacing.clear_stale_lock().ok();

    let opp = Opportunity {
        id: "reserve-settle-001".into(),
        expected_net_usd: dec!(150),
        gas_estimate_gwei: 25,
        venue: "test-dex".into(),
        eoa: format!("0x{:x}", WORKER_EOA),
        timestamp: Utc::now(),
    };

    // Reserve.
    let reservation = pacing
        .try_reserve(&opp, chain_id)
        .expect("reservation must succeed");
    assert_eq!(reservation.status, ReservationStatus::Reserved);
    assert_eq!(reservation.amount_usd, dec!(150));

    // Settle.
    pacing
        .settle(&opp.id, chain_id)
        .expect("settle must succeed");

    // Double-settle must fail.
    assert!(pacing.settle(&opp.id, chain_id).is_err());

    // Expire stale.
    let opp2 = Opportunity {
        id: "expire-002".into(),
        expected_net_usd: dec!(50),
        gas_estimate_gwei: 25,
        venue: "test-dex".into(),
        eoa: format!("0x{:x}", WORKER_EOA),
        timestamp: Utc::now(),
    };
    let _ = pacing.try_reserve(&opp2, chain_id).unwrap();

    // Manually expire by rewriting the file.
    let res_path = reservations_dir.join(format!("reservations-{}.jsonl", chain_id));
    let content = std::fs::read_to_string(&res_path).unwrap();
    let mut records: Vec<ReservationRecord> = content
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<ReservationRecord>(l).ok())
        .collect();
    let stale = Utc::now() - TimeDelta::minutes(10);
    for r in records.iter_mut() {
        if r.id == "expire-002" {
            r.expires_at = stale;
        }
    }
    let updated: String = records
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&res_path, updated).unwrap();

    let expired = pacing.expire_stale(chain_id).expect("expire must succeed");
    assert!(expired >= 1);

    pacing.clear_stale_lock().ok();
}

// ===========================================================================
// Emergency halt trips breaker and blocks
// ===========================================================================

#[test]
fn test_emergency_halt_trips_breaker_and_blocks() {
    let dir = TempDir::new().unwrap();
    let pool_path = dir.path().join("eoa_pool.json");
    let pool: Vec<String> = vec![];
    std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();

    let cfg = test_pacing_config(pool_path.to_str().unwrap());
    let engine = PacingEngine::new(cfg);

    assert!(!engine.is_breaker_active());
    engine.trip_emergency("test-emergency");
    assert!(engine.is_breaker_active());

    let state = engine.current_risk_state();
    assert_eq!(
        state.breaker_tripped,
        Some(BreakerReason::EmergencyHalt("test-emergency".into()))
    );

    let opp = Opportunity {
        id: "blocked-001".into(),
        expected_net_usd: dec!(100),
        gas_estimate_gwei: 10,
        venue: "test".into(),
        eoa: format!("0x{:x}", WORKER_EOA),
        timestamp: Utc::now(),
    };
    let decision = engine.check(&opp).unwrap();
    assert!(matches!(decision, PacingDecision::Deny { .. }));
}

// ===========================================================================
// Detector + routing resolver integration
// ===========================================================================

#[test]
fn test_routing_resolver_with_snapshot() {
    let snapshot = make_at_risk_snapshot();
    let detector = LiquidationDetector::new(snapshot, 8453);
    let candidates = detector.find_at_risk_positions();
    let candidate = &candidates[0];

    let routing = test_routing_config();
    let risk = test_risk_config();
    let resolver = RoutingResolver::new(&routing, &risk);

    let route = resolver
        .resolve_v2(
            candidate.collateral_asset,
            candidate.debt_asset,
            "base",
            candidate.debt_to_cover,
        )
        .expect("must resolve route");

    assert_eq!(route.venue_name, "test-dex");
    assert!(route.amount_out_min > U256::ZERO);
    assert!(route.amount_out_min < candidate.debt_to_cover);
}

// ===========================================================================
// Strategy assembly from detector output
// ===========================================================================

#[test]
fn test_strategy_assembly_with_detector_output() {
    let snapshot = make_at_risk_snapshot();
    let detector = LiquidationDetector::new(snapshot, 8453);
    let candidates = detector.find_at_risk_positions();
    let candidate = &candidates[0];

    let routing = test_routing_config();
    let risk = test_risk_config();
    let resolver = RoutingResolver::new(&routing, &risk);
    let route = resolver
        .resolve_v2(
            candidate.collateral_asset,
            candidate.debt_asset,
            "base",
            candidate.debt_to_cover,
        )
        .expect("route must resolve");

    let shadow_tx = StrategyAssembler::build_shadow_transaction(
        EXECUTOR,
        &route,
        candidate.collateral_asset,
        candidate.user,
        candidate.debt_to_cover,
        candidate.receive_a_token,
        candidate.debt_asset,
        0,
    );

    assert_eq!(shadow_tx.to, EXECUTOR);
    assert_ne!(shadow_tx.to, WORKER_EOA);
    assert_ne!(shadow_tx.to, AAVE_POOL);
    assert_eq!(shadow_tx.value, U256::ZERO);
    assert!(!shadow_tx.data.is_empty());

    let execute_selector = &alloy::primitives::keccak256("execute(bytes)")[..4];
    assert_eq!(&shadow_tx.data[..4], execute_selector);

    // execute(bytes) outer head + dynamic length, followed by exactly 11 ABI words.
    let params_start = 4 + 32 + 32;
    let strategy_params = &shadow_tx.data[params_start..params_start + 11 * 32];
    assert_eq!(strategy_params.len(), 352);

    // Word 0: asset, word 1: amount, word 2: collateral.
    assert_eq!(
        Address::from_slice(&strategy_params[12..32]),
        candidate.debt_asset
    );
    let decoded = Address::from_slice(&strategy_params[64 + 12..64 + 32]);
    assert_eq!(decoded, candidate.collateral_asset);

    // Word 3: user_to_liquidate
    let decoded = Address::from_slice(&strategy_params[96 + 12..96 + 32]);
    assert_eq!(decoded, candidate.user);

    // Shadow payload: words 8-10 (min_profit, tip, deadline) must be zero.
    let shadow_tail = &strategy_params[8 * 32..11 * 32];
    assert!(shadow_tail.iter().all(|&b| b == 0));
}

// ===========================================================================
// Pacing gate: insufficient profit
// ===========================================================================

#[test]
fn test_pacing_insufficient_profit_denied() {
    let dir = TempDir::new().unwrap();
    let pool_path = dir.path().join("eoa_pool.json");
    let pool: Vec<String> = vec![format!("0x{:x}", WORKER_EOA)];
    std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();

    let cfg = test_pacing_config(pool_path.to_str().unwrap());
    let engine = PacingEngine::new(cfg);

    let opp = Opportunity {
        id: "low-profit".into(),
        expected_net_usd: dec!(10),
        gas_estimate_gwei: 50,
        venue: "test".into(),
        eoa: format!("0x{:x}", WORKER_EOA),
        timestamp: Utc::now(),
    };

    let decision = engine.check(&opp).unwrap();
    assert!(matches!(decision, PacingDecision::Deny { reason } if reason == "InsufficientProfit"));
}

// ===========================================================================
// Pacing gate: over single cap
// ===========================================================================

#[test]
fn test_pacing_over_single_cap_denied() {
    let dir = TempDir::new().unwrap();
    let pool_path = dir.path().join("eoa_pool.json");
    let pool: Vec<String> = vec![format!("0x{:x}", WORKER_EOA)];
    std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();

    let cfg = test_pacing_config(pool_path.to_str().unwrap());
    let engine = PacingEngine::new(cfg);

    let opp = Opportunity {
        id: "over-cap".into(),
        expected_net_usd: dec!(1500),
        gas_estimate_gwei: 10,
        venue: "test".into(),
        eoa: format!("0x{:x}", WORKER_EOA),
        timestamp: Utc::now(),
    };

    let decision = engine.check(&opp).unwrap();
    assert!(matches!(decision, PacingDecision::Deny { reason } if reason.contains("exceeds cap")));
}

// ===========================================================================
// Record outcome updates risk state
// ===========================================================================

#[test]
fn test_record_outcome_updates_risk_state() {
    let dir = TempDir::new().unwrap();
    let pool_path = dir.path().join("eoa_pool.json");
    let pool: Vec<String> = vec![format!("0x{:x}", WORKER_EOA)];
    std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();

    let cfg = test_pacing_config(pool_path.to_str().unwrap());
    let engine = PacingEngine::new(cfg);

    let opp = Opportunity {
        id: "risk-001".into(),
        expected_net_usd: dec!(120),
        gas_estimate_gwei: 30,
        venue: "venue-1".into(),
        eoa: format!("0x{:x}", WORKER_EOA),
        timestamp: Utc::now(),
    };
    engine.record_outcome(&opp, dec!(120), Decimal::ZERO, false);

    let state = engine.current_risk_state();
    assert_eq!(state.consecutive_reverts, 0);
    assert!(state.daily_net_usd >= dec!(120));
    assert!(state.last_release.is_some());

    // Revert resets counter on successful outcome
    let opp2 = Opportunity {
        id: "risk-002".into(),
        expected_net_usd: dec!(80),
        gas_estimate_gwei: 30,
        venue: "venue-2".into(),
        eoa: format!("0x{:x}", WORKER_EOA),
        timestamp: Utc::now(),
    };
    engine.record_outcome(&opp2, Decimal::ZERO, dec!(0.001), true);

    let state = engine.current_risk_state();
    assert_eq!(state.consecutive_reverts, 1);

    // Profitable outcome resets revert counter
    let opp3 = Opportunity {
        id: "risk-003".into(),
        expected_net_usd: dec!(100),
        gas_estimate_gwei: 30,
        venue: "venue-3".into(),
        eoa: format!("0x{:x}", WORKER_EOA),
        timestamp: Utc::now(),
    };
    engine.record_outcome(&opp3, dec!(100), Decimal::ZERO, false);

    let state = engine.current_risk_state();
    assert_eq!(state.consecutive_reverts, 0);
}
