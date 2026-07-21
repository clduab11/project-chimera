//! Live-Refresh Detection E2E
//!
//! THE acceptance criterion for the live-detection re-architecture: the same
//! running orchestrator must OBSERVE a position crossing below HF 1.05 between
//! two scans when the live price refresh path updates the shared snapshot.
//! The frozen-snapshot architecture this replaces could never do that — every
//! scan re-ran identical math on identical data.
//!
//! All tests drive the REAL scan path (`run_single_scan` delegates to the same
//! `scan_once` the live loop runs): refresher tick → detector rebuild from the
//! shared snapshot → candidate processing. Only the RPC wire (price source) and
//! the simulator verdict are scripted.
//!
//! Runs in `cargo test -p chimera-core` without any external node. The one
//! `#[ignore]`d smoke test hits real Base RPC when CHIMERA_RPC_URL is set.

use alloy::primitives::{address, Address, U256};
use alloy::providers::ProviderBuilder;
use async_trait::async_trait;
use chimera_core::{
    config::{PacingConfig, RiskConfig, RoutingConfig, TradingPair, VenueEntry},
    detector::liquidation::{MarketSnapshot, ReserveData, UserPosition},
    ChimeraError, CrossProcessPacing, JsonlPersistence, Metrics, Orchestrator,
    OrchestratorConfig, PacingEngine, ReservePriceSource, RpcSubmitter, SignerRegistry,
    SimulationResult, SnapshotRefresher,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tempfile::TempDir;
use url::Url;

// ---------------------------------------------------------------------------
// Shared test addresses and builders (duplicated per repo test convention)
// ---------------------------------------------------------------------------

const USER: Address = address!("0x1111111111111111111111111111111111111111");
const COLL_WETH: Address = address!("0x4200000000000000000000000000000000000006");
const DEBT_USDC: Address = address!("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
const ROUTER: Address = address!("0xA0b86a33E6441e0A421e56E4773C3C4b0Db7E5b0");
const EXECUTOR: Address = address!("0x9999999999999999999999999999999999999999");
const WORKER_EOA: Address = address!("0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE");

fn ray() -> U256 {
    U256::from(1_000_000_000_000_000_000_000_000_000u128)
}

fn usd8(dollars: u128) -> U256 {
    U256::from(dollars * 100_000_000u128)
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
        active: true,
        ..Default::default()
    }
}

/// 1 WETH collateral (LT 8250) vs 2,900 USDC (6-dec) debt.
/// HF = weth_price × 0.825 / 2900:
///   WETH $4000 → HF ≈ 1.138 (healthy, above the 1.05 threshold)
///   WETH $3400 → HF ≈ 0.967 (liquidatable)
fn snapshot_with_weth_price(weth_price_8dec: U256) -> MarketSnapshot {
    let reserves: HashMap<Address, ReserveData> = vec![
        (COLL_WETH, mk_reserve(weth_price_8dec, 8250, 10500)),
        (DEBT_USDC, {
            let mut r = mk_reserve(usd8(1), 0, 0);
            r.decimals = 6;
            r
        }),
    ]
    .into_iter()
    .collect();

    let mut collateral = HashMap::new();
    collateral.insert(COLL_WETH, U256::from(1_000_000_000_000_000_000u128));
    let mut debt = HashMap::new();
    debt.insert(DEBT_USDC, U256::from(2_900_000_000u128));
    let users: HashMap<Address, UserPosition> = vec![(
        USER,
        UserPosition {
            collateral,
            debt,
            emode_category: 0,
            is_in_isolation: false,
        },
    )]
    .into_iter()
    .collect();

    MarketSnapshot {
        reserves,
        users,
        block_number: 42,
        chain_id: 8453,
        timestamp: chrono::Utc::now(),
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

// ---------------------------------------------------------------------------
// Scripted price source: replaces only the RPC wire; everything downstream of
// ReservePriceSource is production code.
// ---------------------------------------------------------------------------

struct ScriptedPricer {
    prices: StdMutex<HashMap<Address, U256>>,
    fail: AtomicBool,
}

impl ScriptedPricer {
    fn new(prices: &[(Address, U256)]) -> Arc<Self> {
        Arc::new(Self {
            prices: StdMutex::new(prices.iter().copied().collect()),
            fail: AtomicBool::new(false),
        })
    }

    fn set_price(&self, asset: Address, price: U256) {
        self.prices.lock().unwrap().insert(asset, price);
    }
}

#[async_trait]
impl ReservePriceSource for ScriptedPricer {
    async fn get_prices_raw(
        &self,
        assets: &[Address],
    ) -> Result<HashMap<Address, U256>, ChimeraError> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(ChimeraError::OracleError("scripted failure".into()));
        }
        let prices = self.prices.lock().unwrap();
        Ok(assets
            .iter()
            .filter_map(|a| prices.get(a).map(|p| (*a, *p)))
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness<P: alloy::providers::Provider<alloy::network::Ethereum> + Clone + 'static> {
    orchestrator: Orchestrator<P>,
    metrics: Arc<Metrics>,
    captured: Arc<StdMutex<Vec<(Address, U256)>>>,
    _dir: TempDir,
}

/// Build a shadow orchestrator over `snapshot` with the mock sim capturing every
/// candidate it sees, optionally attaching a refresher over `pricer` +
/// `snapshot_file` with a 1-second reprice interval.
fn build_harness(
    snapshot: MarketSnapshot,
    refresher_parts: Option<(Arc<dyn ReservePriceSource>, std::path::PathBuf)>,
    risk_config: RiskConfig,
    execute_mode: &str,
) -> Harness<impl alloy::providers::Provider<alloy::network::Ethereum> + Clone + 'static> {
    let dir = TempDir::new().unwrap();
    let chain_id = 8453u64;

    let pool_path = dir.path().join("eoa_pool.json");
    let pool: Vec<String> = vec![format!("0x{:x}", WORKER_EOA)];
    std::fs::write(&pool_path, serde_json::to_string(&pool).unwrap()).unwrap();
    let pacing_cfg = test_pacing_config(pool_path.to_str().unwrap());

    let outcomes_path = dir.path().join("outcomes.jsonl");
    let persistence = Arc::new(JsonlPersistence::new(outcomes_path.clone(), 50, 5));
    let pacing_engine = PacingEngine::new(pacing_cfg.clone()).with_state_persistence(persistence);
    let reservations_dir = dir.path().join("reservations");
    std::fs::create_dir_all(&reservations_dir).unwrap();
    let pacing = CrossProcessPacing::new(pacing_engine, reservations_dir, outcomes_path);
    pacing.clear_stale_lock().ok();

    let metrics = Arc::new(Metrics::new());
    let provider =
        Arc::new(ProviderBuilder::new().connect_http(Url::parse("http://127.0.0.1:1").unwrap()));
    let submitter = RpcSubmitter::new((*provider).clone(), chain_id).with_dry_run(true);
    let signer_registry = Arc::new(
        SignerRegistry::load(&pacing_cfg, "shadow").expect("shadow signer registry must load"),
    );

    let orchestrator = Orchestrator::new(
        OrchestratorConfig::default(),
        pacing,
        pacing_cfg,
        metrics.clone(),
        provider,
        snapshot,
        chain_id,
        None, // no real simulator — mock sim fn below
        submitter,
        execute_mode.to_string(),
        EXECUTOR,
        test_routing_config(),
        risk_config,
        signer_registry,
        None, // no mempool watcher
    );

    let orchestrator = match refresher_parts {
        Some((pricer, snapshot_file)) => {
            let refresher = Arc::new(SnapshotRefresher::new(
                orchestrator.shared_snapshot(),
                pricer,
                snapshot_file,
                chain_id,
                1, // reprice due every second
            ));
            orchestrator.with_snapshot_refresher(refresher)
        }
        None => orchestrator,
    };

    let mut orchestrator = orchestrator;
    orchestrator.set_mock_gas_price(1_000_000_000);
    let captured: Arc<StdMutex<Vec<(Address, U256)>>> = Arc::new(StdMutex::new(Vec::new()));
    let captured_in_sim = captured.clone();
    orchestrator.set_mock_sim_fn(move |candidate| {
        captured_in_sim
            .lock()
            .unwrap()
            .push((candidate.user, candidate.current_hf));
        SimulationResult {
            profitable: true,
            expected_profit_usd: dec!(200),
            gas_used: 150_000,
            l1_data_fee_wei: U256::from(500_000u64),
            revert_reason: None,
            calldata: vec![],
        }
    });

    Harness {
        orchestrator,
        metrics,
        captured,
        _dir: dir,
    }
}

/// RAY band for HF ≈ 0.967 (1 WETH @ $3400, LT 8250, 2900 USDC debt).
fn crossed_band() -> (U256, U256) {
    (
        U256::from(960_000_000_000_000_000_000_000_000u128),
        U256::from(975_000_000_000_000_000_000_000_000u128),
    )
}

// ===========================================================================
// THE acceptance test: a position crosses 1.05 between two scans of the SAME
// orchestrator, and the engine observes it.
// ===========================================================================

#[tokio::test]
async fn test_orchestrator_observes_hf_crossing_below_105_between_scans() {
    let pricer = ScriptedPricer::new(&[(COLL_WETH, usd8(4000)), (DEBT_USDC, usd8(1))]);
    let dir = TempDir::new().unwrap();
    let h = build_harness(
        snapshot_with_weth_price(usd8(4000)),
        Some((pricer.clone(), dir.path().join("no_snapshot.json"))),
        RiskConfig::default(),
        "shadow",
    );

    // SCAN 1 — the tick repriced from the live source ($4000: unchanged) and the
    // position is healthy (HF ≈ 1.138): zero candidates.
    let n1 = h.orchestrator.run_single_scan().await.unwrap();
    assert_eq!(n1, 0, "healthy position (HF≈1.138) must produce zero candidates");
    assert_eq!(h.metrics.candidates_seen.with_label_values(&["base"]).get(), 0);
    assert_eq!(h.metrics.sims_run.with_label_values(&["success"]).get(), 0);
    assert_eq!(
        h.metrics
            .price_refresh_total
            .with_label_values(&["base", "success"])
            .get(),
        1,
        "scan 1 must have repriced through the live path"
    );

    // PRICE EVENT — the oracle now reports $3400. Wait out the 1s reprice pacing
    // so the next scan's tick picks it up through the REAL refresh path.
    pricer.set_price(COLL_WETH, usd8(3400));
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    // SCAN 2 — same orchestrator, no reconstruction: observes the crossing.
    let n2 = h.orchestrator.run_single_scan().await.unwrap();
    assert!(n2 >= 1, "post-refresh scan must observe the underwater position");

    // The shared snapshot the scan read reflects the repriced value.
    assert_eq!(
        h.orchestrator.shared_snapshot().snapshot().reserves[&COLL_WETH].price_usd,
        usd8(3400)
    );

    // The candidate is the repriced position, with HF in the crossed band.
    let captured = h.captured.lock().unwrap();
    assert_eq!(captured.len(), n2.min(5), "mock sim must have seen each candidate");
    let (user, hf) = captured[0];
    assert_eq!(user, USER);
    let (lo, hi) = crossed_band();
    assert!(
        hf >= lo && hf <= hi,
        "HF must be ≈0.967e27 (crossed below 1.05e27), got {hf}"
    );

    assert_eq!(
        h.metrics.candidates_seen.with_label_values(&["base"]).get(),
        n2 as u64
    );
    assert!(h.metrics.sims_run.with_label_values(&["success"]).get() >= 1);
    assert_eq!(
        h.orchestrator.submit_attempt_count(),
        0,
        "shadow mode must never touch the submit path"
    );
}

// ===========================================================================
// Companions
// ===========================================================================

/// A reprice failure retains last-known-good prices and detection keeps
/// operating on them (shadow mode).
#[tokio::test]
async fn test_reprice_failure_retains_prices_and_keeps_scanning() {
    let pricer = ScriptedPricer::new(&[]);
    pricer.fail.store(true, Ordering::Relaxed);
    let dir = TempDir::new().unwrap();
    let h = build_harness(
        snapshot_with_weth_price(usd8(3400)), // already at-risk at boot
        Some((pricer.clone(), dir.path().join("no_snapshot.json"))),
        RiskConfig::default(),
        "shadow",
    );

    let n = h.orchestrator.run_single_scan().await.unwrap();
    assert!(n >= 1, "detection must keep working on retained prices");
    assert_eq!(
        h.orchestrator.shared_snapshot().snapshot().reserves[&COLL_WETH].price_usd,
        usd8(3400),
        "failed reprice must not touch prices"
    );
    assert_eq!(
        h.metrics
            .price_refresh_total
            .with_label_values(&["base", "error"])
            .get(),
        1
    );
}

/// File-driven discovery reload: an atomic snapshot replacement between scans
/// swaps users/reserves/block and the next scan sees it (the reload analogue of
/// the acceptance test's reprice path).
#[tokio::test]
async fn test_snapshot_reload_swaps_between_scans() {
    fn generator_json(block: u64, weth_price_dollars: f64) -> String {
        let weth = format!("{COLL_WETH:#x}");
        let usdc = format!("{DEBT_USDC:#x}");
        let user = format!("{USER:#x}");
        format!(
            r#"{{
  "chain": "base",
  "block_number": {block},
  "pool": "0xA238Dd80C259a72e81d7e4664a9801593F98d1c5",
  "timestamp": {ts},
  "reserves": [
    {{"address": "{weth}", "symbol": "WETH", "decimals": 18,
      "liquidation_threshold": 8250, "liquidation_bonus": 10500,
      "price_usd": {weth_price_dollars},
      "a_token": "0x0101010101010101010101010101010101010101",
      "variable_debt_token": "0x0202020202020202020202020202020202020202",
      "active": true}},
    {{"address": "{usdc}", "symbol": "USDC", "decimals": 6,
      "liquidation_threshold": 0, "liquidation_bonus": 0,
      "price_usd": 1.0,
      "a_token": "0x0303030303030303030303030303030303030303",
      "variable_debt_token": "0x0404040404040404040404040404040404040404",
      "active": true}}
  ],
  "users": {{
    "{user}": {{
      "collateral": {{"{weth}": "1000000000000000000"}},
      "debt": {{"{usdc}": "2900000000"}},
      "emode_category": 0,
      "is_in_isolation": false
    }}
  }}
}}"#,
            ts = chrono::Utc::now().timestamp(),
        )
    }
    fn write_atomically(path: &std::path::Path, content: &str) {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, content).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("snapshot.json");
    write_atomically(&path, &generator_json(100, 4000.0));
    let boot = MarketSnapshot::load_from_file(&path).unwrap();

    // Pricer intentionally failing: this test isolates the reload path.
    let pricer = ScriptedPricer::new(&[]);
    pricer.fail.store(true, Ordering::Relaxed);
    let h = build_harness(
        boot,
        Some((pricer, path.clone())),
        RiskConfig::default(),
        "shadow",
    );

    let n1 = h.orchestrator.run_single_scan().await.unwrap();
    assert_eq!(n1, 0, "boot file is healthy (HF≈1.138)");

    // Generator rewrites the file: newer block, crashed price.
    write_atomically(&path, &generator_json(101, 3400.0));
    let n2 = h.orchestrator.run_single_scan().await.unwrap();
    assert!(n2 >= 1, "reloaded snapshot must produce the candidate");
    assert_eq!(h.orchestrator.shared_snapshot().snapshot().block_number, 101);
    assert_eq!(
        h.metrics
            .snapshot_reload_total
            .with_label_values(&["base", "applied"])
            .get(),
        1
    );
}

/// Live-mode staleness halt: with prices older than price_max_stale_secs and a
/// failing price source, candidate emission is skipped even though the snapshot
/// holds an at-risk position (stale prices produce false candidates that cost
/// real gas in live mode). Shadow mode (all other tests) keeps scanning.
#[tokio::test]
async fn test_live_mode_staleness_halt_skips_emission() {
    let pricer = ScriptedPricer::new(&[]);
    pricer.fail.store(true, Ordering::Relaxed);
    let dir = TempDir::new().unwrap();

    // Snapshot with an EPOCH timestamp => boot price age is enormous.
    let mut snapshot = snapshot_with_weth_price(usd8(3400));
    snapshot.timestamp = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH;

    let risk = RiskConfig {
        price_max_stale_secs: 1,
        ..RiskConfig::default()
    };
    let h = build_harness(
        snapshot,
        Some((pricer, dir.path().join("no_snapshot.json"))),
        risk,
        "live",
    );

    let n = h.orchestrator.run_single_scan().await.unwrap();
    assert_eq!(n, 0, "stale prices in live mode must skip candidate emission");
    assert_eq!(h.metrics.candidates_seen.with_label_values(&["base"]).get(), 0);
    assert_eq!(
        h.metrics
            .scans_skipped_stale_price
            .with_label_values(&["base"])
            .get(),
        1
    );
    assert!(h.captured.lock().unwrap().is_empty(), "no candidate may reach the sim");
}

/// Concurrent snapshot writes vs scans: no deadlock, no torn reads. Every scan
/// sees either the healthy price or the crashed price — never a mix — because
/// the scan clones the snapshot under one short read lock.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_refresh_and_scan_no_deadlock_or_torn_reads() {
    let h = build_harness(
        snapshot_with_weth_price(usd8(4000)),
        None, // writer task below plays the refresher's role
        RiskConfig::default(),
        "shadow",
    );
    let shared = h.orchestrator.shared_snapshot();

    let writer = tokio::spawn(async move {
        for i in 0..200u32 {
            let price = if i % 2 == 0 { usd8(3400) } else { usd8(4000) };
            let mut prices = HashMap::new();
            prices.insert(COLL_WETH, price);
            shared.apply_prices(&prices);
            tokio::task::yield_now().await;
        }
    });

    for _ in 0..50 {
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            h.orchestrator.run_single_scan(),
        )
        .await
        .expect("scan deadlocked against concurrent snapshot writes")
        .unwrap();
        assert!(n <= 1, "single-user snapshot can never yield more than one candidate");
    }
    writer.await.unwrap();

    // Every candidate the sim saw must carry the crashed-price HF — a torn read
    // (healthy price mixed with candidate emission) would fall outside the band.
    let (lo, hi) = crossed_band();
    for (_, hf) in h.captured.lock().unwrap().iter() {
        assert!(
            *hf >= lo && *hf <= hi,
            "captured HF {hf} outside the crashed-price band: torn snapshot read"
        );
    }
}

/// Manual smoke against real Base RPC (needs CHIMERA_RPC_URL or BASE_RPC_URL):
/// `cargo test -p chimera-core -- --ignored live_getassetsprices`
#[tokio::test]
#[ignore]
async fn live_getassetsprices_smoke_base() {
    let url = std::env::var("CHIMERA_RPC_URL")
        .or_else(|_| std::env::var("BASE_RPC_URL"))
        .expect("set CHIMERA_RPC_URL or BASE_RPC_URL to run this smoke test");
    let provider = ProviderBuilder::new().connect_http(Url::parse(&url).unwrap());
    let oracle = chimera_core::AaveOracle::new(
        provider,
        address!("0x2Cc0Fc26eD4563A5ce5e8bdcfe1A2878676Ae156"), // Aave V3 Oracle, Base
        std::time::Duration::from_secs(300),
    );

    let prices = oracle
        .get_asset_prices_raw(&[COLL_WETH, DEBT_USDC])
        .await
        .expect("live getAssetsPrices must succeed");

    let weth = prices[&COLL_WETH];
    let usdc = prices[&DEBT_USDC];
    assert!(
        weth > usd8(100) && weth < usd8(100_000),
        "WETH price sanity band failed: {weth}"
    );
    assert!(
        usdc > U256::from(95_000_000u64) && usdc < U256::from(105_000_000u64),
        "USDC must be ~$1: {usdc}"
    );
}
