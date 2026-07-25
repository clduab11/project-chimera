//! Continuous detection → simulation → execution orchestrator loop.

use crate::config::{RiskConfig, RoutingConfig};
use crate::snapshot_refresh::{SharedSnapshot, SnapshotRefresher};
use crate::{
    check_eoa_gas_sufficient, ChimeraError, CrossProcessPacing, LiquidationCandidate,
    LiquidationDetector, LiquidationSimulator, MarketSnapshot, MempoolWatcher, Metrics,
    Opportunity, PacingConfig, PacingDecision, ResolvedV2Route, RoutingResolver, RpcSubmitter,
    SignerRegistry, StrategyAssembler,
};
use alloy::consensus::{SignableTransaction, TxEip1559};
use alloy::eips::eip2718::Encodable2718;
use alloy::network::Ethereum;
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::signers::Signer;
use hex;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tracing::{debug, info, warn};

/// Baseline OP-stack / Arbitrum L1 fee scalar (scalar / 1e6 == 1.0). The simulator's
/// chain-specific fee model refines this; a neutral baseline is used per scan.
const DEFAULT_L1_FEE_SCALAR: u64 = 1_000_000;

/// Fallback EOA used only for shadow logging when no clean EOA pool is loaded.
const FALLBACK_EOA: &str = "0x0000000000000000000000000000000000000000";

/// WETH address on Base.
const WETH_BASE: &str = "0x4200000000000000000000000000000000000006";
/// WETH address on Arbitrum.
const WETH_ARBITRUM: &str = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1";

/// USDC on Base (6 decimals).
const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
/// USDC on Arbitrum (6 decimals).
const USDC_ARBITRUM: &str = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831";
/// USDT on Base (6 decimals).
const USDT_BASE: &str = "0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2";
/// USDT on Arbitrum (6 decimals).
const USDT_ARBITRUM: &str = "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9";

/// Returns true if the given address is a known USD-pegged stablecoin on any
/// supported chain. Used to bypass the ETH-price oracle for non-WETH debt.
fn is_stablecoin(addr: &Address) -> bool {
    let usdc_base: Address = USDC_BASE.parse().expect("valid USDC_BASE constant");
    let usdc_arb: Address = USDC_ARBITRUM.parse().expect("valid USDC_ARBITRUM constant");
    let usdt_base: Address = USDT_BASE.parse().expect("valid USDT_BASE constant");
    let usdt_arb: Address = USDT_ARBITRUM.parse().expect("valid USDT_ARBITRUM constant");
    *addr == usdc_base || *addr == usdc_arb || *addr == usdt_base || *addr == usdt_arb
}

/// Sub-interval at which the emergency flag is polled during the inter-scan wait.
/// Keeps emergency-detection latency bounded at <= 3s (well under the 5s requirement).
const EMERGENCY_POLL_SECS: u64 = 3;

/// Configuration for the orchestrator loop.
pub struct OrchestratorConfig {
    pub scan_interval_secs: u64,
    pub max_opportunities_per_scan: usize,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            scan_interval_secs: 12,
            max_opportunities_per_scan: 5,
        }
    }
}

/// Main continuous orchestrator.
///
/// Owns the real runtime components: a [`LiquidationSimulator`] (behind a
/// [`tokio::sync::Mutex`] because `simulate_liquidation` needs `&mut self`), an
/// [`RpcSubmitter`] (already configured with `dry_run = execute_mode != "live"`),
/// and a [`CrossProcessPacing`] that wraps a pre-built [`PacingEngine`].
/// Execution is gated on `execute_mode == "live"` AND pacing-allowed AND profitable.
/// Test-only simulation override used by shadow/e2e harnesses.
#[cfg(any(test, feature = "test-hooks"))]
type MockSimFn = Arc<dyn Fn(&crate::LiquidationCandidate) -> crate::SimulationResult + Send + Sync>;

pub struct Orchestrator<P: Provider<Ethereum> + Clone + Send + Sync + 'static> {
    config: OrchestratorConfig,
    pacing: CrossProcessPacing,
    pacing_cfg: PacingConfig,
    metrics: Arc<Metrics>,
    provider: Arc<P>,
    snapshot: SharedSnapshot,
    chain_id: u64,
    simulator: Mutex<Option<LiquidationSimulator<P>>>,
    submitter: RpcSubmitter<P>,
    execute_mode: String,
    executor: Address,
    routing_config: RoutingConfig,
    risk_config: RiskConfig,
    signer_registry: Arc<SignerRegistry>,
    mempool_watcher: Option<Arc<dyn MempoolWatcher>>,
    // Live snapshot refresher (reprice + discovery reload), ticked inline at the
    // top of every non-emergency scan. None = legacy frozen-snapshot behavior.
    refresher: Option<Arc<SnapshotRefresher>>,
    // Test-only injection hooks — absent from release builds (see the
    // `test-hooks` feature in Cargo.toml). The live pipeline always uses the
    // real provider gas price and the real REVM simulator.
    #[cfg(any(test, feature = "test-hooks"))]
    mock_gas_price_wei: Option<u128>,
    #[cfg(any(test, feature = "test-hooks"))]
    mock_sim_fn: Option<MockSimFn>,
}

impl<P: Provider<Ethereum> + Clone + Send + Sync + 'static> Orchestrator<P> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: OrchestratorConfig,
        pacing: CrossProcessPacing,
        pacing_cfg: PacingConfig,
        metrics: Arc<Metrics>,
        provider: Arc<P>,
        snapshot: MarketSnapshot,
        chain_id: u64,
        simulator: Option<LiquidationSimulator<P>>,
        submitter: RpcSubmitter<P>,
        execute_mode: String,
        executor: Address,
        routing_config: RoutingConfig,
        risk_config: RiskConfig,
        signer_registry: Arc<SignerRegistry>,
        mempool_watcher: Option<Arc<dyn MempoolWatcher>>,
    ) -> Self {
        Self {
            config,
            pacing,
            pacing_cfg,
            metrics,
            provider,
            snapshot: SharedSnapshot::new(snapshot),
            chain_id,
            simulator: Mutex::new(simulator),
            submitter,
            execute_mode,
            executor,
            routing_config,
            risk_config,
            signer_registry,
            mempool_watcher,
            refresher: None,
            #[cfg(any(test, feature = "test-hooks"))]
            mock_gas_price_wei: None,
            #[cfg(any(test, feature = "test-hooks"))]
            mock_sim_fn: None,
        }
    }

    /// Handle to the live shared snapshot (used by `main.rs` to construct the
    /// [`SnapshotRefresher`] over the same state the scan loop reads).
    pub fn shared_snapshot(&self) -> SharedSnapshot {
        self.snapshot.clone()
    }

    /// Attach the live snapshot refresher (builder style, mirrors
    /// `PacingEngine::with_eth_oracle`). Without one, the snapshot stays frozen
    /// at its boot contents.
    pub fn with_snapshot_refresher(mut self, refresher: Arc<SnapshotRefresher>) -> Self {
        self.refresher = Some(refresher);
        self
    }

    pub async fn run(&self) -> Result<(), ChimeraError> {
        info!(
            target = "chimera::orchestrator",
            chain_id = self.chain_id,
            execute_mode = %self.execute_mode,
            "Orchestrator starting"
        );
        let chain_label = self.chain_label_str();

        // Resolve the emergency flag path once. Operators (scripts/emergency_pause.py)
        // create this file to halt and DELETE it to resume.
        let emergency_flag_path = std::path::PathBuf::from(
            std::env::var("CHIMERA_EMERGENCY_FLAG")
                .unwrap_or_else(|_| "core/state/emergency.flag".into()),
        );

        loop {
            // Refresh cached ETH/USD price from the attached oracle before every
            // scan cycle. On failure the previous cached price (or fallback) is
            // retained, and the orchestrator logs the fallback path explicitly.
            self.pacing.engine().refresh_eth_price().await;

            // Emergency check at the TOP of each scan: if active, force-trip the breaker
            // and skip the entire detect/simulate/submit pipeline this iteration.
            if let Some(reason) = Self::read_emergency_flag(&emergency_flag_path) {
                self.handle_emergency(&reason);
            } else {
                // Refresh + detect + process. Inside the non-emergency branch so an
                // operator pause spends zero RPC on repricing or reload checks.
                self.scan_once().await;
            }

            // Update rolling gauges + breaker state every scan (Decimal -> f64 only here).
            let rs = self.pacing.engine().current_risk_state();
            self.metrics
                .set_daily_net_usd(chain_label, rs.daily_net_usd.to_f64().unwrap_or(0.0));
            self.metrics
                .set_weekly_net_usd(chain_label, rs.weekly_net_usd.to_f64().unwrap_or(0.0));
            self.metrics
                .set_breaker_state(chain_label, self.pacing.engine().is_breaker_active());

            // Wait for next scan trigger. When a MempoolWatcher is wired, new blocks
            // trigger scans immediately (vs. fixed 12s interval). The emergency flag
            // is still polled on every EMERGENCY_POLL_SECS boundary regardless of
            // which wait strategy is active.
            if let Some(ref watcher) = self.mempool_watcher {
                // Watcher-driven wait: block until new-heads or timeout.
                // On each EMERGENCY_POLL_SECS timeout, re-check the emergency flag
                // so an operator pause is detected within <= 3s.
                let mut waited = 0u64;
                while waited < self.config.scan_interval_secs {
                    if let Some(reason) = Self::read_emergency_flag(&emergency_flag_path) {
                        self.handle_emergency(&reason);
                        break;
                    }
                    let chunk = EMERGENCY_POLL_SECS.min(self.config.scan_interval_secs - waited);
                    match tokio::time::timeout(
                        Duration::from_secs(chunk),
                        watcher.wait_for_trigger(),
                    )
                    .await
                    {
                        Ok(Ok(event)) => {
                            // Trigger received — scan now.
                            info!(
                                target = "chimera::orchestrator",
                                chain_id = self.chain_id,
                                ?event,
                                "Mempool trigger received"
                            );
                            break;
                        }
                        Ok(Err(e)) => {
                            warn!(
                                target = "chimera::orchestrator",
                                chain_id = self.chain_id,
                                error = %e,
                                "Mempool watcher error; falling back to poll interval"
                            );
                            // Watcher returned within the chunk — account for elapsed time.
                            waited += chunk;
                        }
                        Err(_elapsed) => {
                            // Timeout fired — chunk seconds have elapsed.
                            waited += chunk;
                        }
                    }
                    // When the watcher errors or times out, the chunk interval
                    // has already elapsed; do NOT add a second sleep. Continue
                    // the loop for emergency flag polling.
                    if let Some(reason) = Self::read_emergency_flag(&emergency_flag_path) {
                        self.handle_emergency(&reason);
                        break;
                    }
                }
            } else {
                // No watcher configured: fixed-interval poll with emergency check.
                let mut waited = 0u64;
                while waited < self.config.scan_interval_secs {
                    if let Some(reason) = Self::read_emergency_flag(&emergency_flag_path) {
                        self.handle_emergency(&reason);
                        break;
                    }
                    let chunk = EMERGENCY_POLL_SECS.min(self.config.scan_interval_secs - waited);
                    sleep(Duration::from_secs(chunk)).await;
                    waited += chunk;
                }
            }
        }
    }

    /// Force-trip the breaker in response to an active emergency flag.
    ///
    /// This is intentionally fail-safe: it only *trips* the breaker. When the operator
    /// resumes (deletes the flag), the breaker remains tripped — the operator MUST run
    /// the gated `clear_breaker` path to resume execution. We never auto-clear here.
    fn handle_emergency(&self, reason: &str) {
        warn!(target: "chimera::orchestrator", %reason, "EMERGENCY FLAG ACTIVE — halting execution");
        self.pacing.engine().trip_emergency(reason);
        self.metrics.set_breaker_state(self.chain_label_str(), true);
    }

    /// Returns Some(reason) if the emergency flag file exists AND has paused=true.
    fn read_emergency_flag(path: &std::path::Path) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&content).ok()?;
        if v.get("paused").and_then(|p| p.as_bool()).unwrap_or(false) {
            let reason = v
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("emergency.flag")
                .to_string();
            Some(reason)
        } else {
            None
        }
    }

    /// Map chain_id to a human-readable label for metrics and routing.
    /// Returns "unknown" for unrecognized chain IDs to avoid silently
    /// mislabeling metrics.
    fn chain_label_str(&self) -> &'static str {
        match self.chain_id {
            8453 => "base",
            42161 => "arbitrum",
            other => {
                tracing::warn!(
                    target = "chimera::orchestrator",
                    chain_id = other,
                    "Unrecognized chain ID; metrics label will be 'unknown'"
                );
                "unknown"
            }
        }
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_mock_gas_price(&mut self, price_wei: u128) {
        self.mock_gas_price_wei = Some(price_wei);
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub fn set_mock_sim_fn(
        &mut self,
        f: impl Fn(&LiquidationCandidate) -> crate::SimulationResult + Send + Sync + 'static,
    ) {
        self.mock_sim_fn = Some(Arc::new(f));
    }

    /// Gas-price override for tests. Always `None` in release builds — the
    /// backing field and setter do not exist without the `test-hooks` feature,
    /// so the live pipeline unconditionally queries the provider.
    #[cfg(any(test, feature = "test-hooks"))]
    fn mock_gas_price(&self) -> Option<u128> {
        self.mock_gas_price_wei
    }
    #[cfg(not(any(test, feature = "test-hooks")))]
    #[inline(always)]
    #[allow(clippy::unused_self)]
    fn mock_gas_price(&self) -> Option<u128> {
        None
    }

    /// Simulation override for tests. Always `None` in release builds, so the
    /// live pipeline unconditionally runs the real REVM simulator.
    #[cfg(any(test, feature = "test-hooks"))]
    fn mock_sim(&self, candidate: &LiquidationCandidate) -> Option<crate::SimulationResult> {
        self.mock_sim_fn.as_ref().map(|f| f(candidate))
    }
    #[cfg(not(any(test, feature = "test-hooks")))]
    #[inline(always)]
    #[allow(clippy::unused_self)]
    fn mock_sim(&self, _candidate: &LiquidationCandidate) -> Option<crate::SimulationResult> {
        None
    }

    /// Single-scan entry point for integration testing.
    ///
    /// Delegates to the SAME [`Self::scan_once`] the live loop runs — refresh
    /// tick, simulator rebuild on reload, staleness gate, detection, candidate
    /// processing — exactly once, without entering the infinite loop. Returns
    /// the number of candidates found.
    #[cfg(any(test, feature = "test-hooks"))]
    pub async fn run_single_scan(&self) -> Result<usize, ChimeraError> {
        Ok(self.scan_once().await)
    }

    /// One full scan: refresh tick (reprice + discovery reload), simulator
    /// rebuild when a reload was applied, staleness gate, detector rebuild from
    /// the CURRENT shared snapshot, and candidate processing.
    ///
    /// The snapshot is cloned exactly once per scan; the detector, the HF math,
    /// and the block number handed to the simulator all come from that clone, so
    /// a refresh landing mid-scan can never produce a mixed-epoch scan.
    async fn scan_once(&self) -> usize {
        let chain_label = self.chain_label_str();

        if let Some(refresher) = &self.refresher {
            let outcome = refresher.tick(&self.metrics, chain_label).await;
            if outcome.reloaded {
                self.rebuild_simulator(refresher.snapshot_path()).await;
            }

            // Staleness halt, live mode only: stale prices produce false
            // candidates, and a false candidate costs real gas when live. In
            // shadow the age gauge + reprice-failure warns cover observability.
            if self.execute_mode == "live"
                && refresher.price_age_secs() > self.risk_config.price_max_stale_secs
            {
                warn!(
                    target = "chimera::orchestrator",
                    price_age_secs = refresher.price_age_secs(),
                    max = self.risk_config.price_max_stale_secs,
                    "Live prices stale; skipping candidate emission this scan"
                );
                self.metrics.observe_scan_skipped_stale(chain_label);
                return 0;
            }
        }

        let snap = self.snapshot.snapshot();
        let snapshot_block = snap.block_number;
        let detector = LiquidationDetector::new(snap, self.chain_id);
        let candidates = detector.find_at_risk_positions();
        self.metrics
            .observe_candidates(chain_label, candidates.len());

        for candidate in candidates
            .iter()
            .take(self.config.max_opportunities_per_scan)
        {
            if let Err(e) = self.process_candidate(candidate, snapshot_block).await {
                warn!(target = "chimera::orchestrator", error = %e, "Candidate failed");
            }
        }

        candidates.len()
    }

    /// Rebuild the simulator's fork DB from the just-reloaded snapshot file.
    ///
    /// `CacheDB` never evicts: without a rebuild, users removed from the
    /// snapshot keep their old cached balances and the first-touch-cached
    /// oracle rounds drift from reality indefinitely. Non-fatal — a failed
    /// rebuild keeps the previous DB and warns.
    async fn rebuild_simulator(&self, path: &std::path::Path) {
        let mut guard = self.simulator.lock().await;
        if let Some(sim) = guard.as_mut() {
            if let Err(e) = sim.rebuild_db(path).await {
                warn!(
                    target = "chimera::orchestrator",
                    error = %e,
                    "Simulator rebuild after snapshot reload failed; keeping previous DB"
                );
            }
        }
    }

    /// Number of submit-path invocations (includes dry-run).
    /// Shadow mode should always return 0.
    pub fn submit_attempt_count(&self) -> u64 {
        self.submitter.submit_attempt_count()
    }

    pub async fn process_candidate(
        &self,
        candidate: &LiquidationCandidate,
        snapshot_block: u64,
    ) -> Result<(), ChimeraError> {
        // 1. Current gas price — use the test hook if present, otherwise query
        //    the provider. `mock_gas_price()` is a `None`-returning no-op in
        //    release builds, so the branch collapses to the provider path.
        let gas_price_wei = if let Some(mock_price) = self.mock_gas_price() {
            mock_price
        } else {
            self.provider
                .get_gas_price()
                .await
                .map_err(|e| ChimeraError::RpcError(format!("get_gas_price failed: {e}")))?
        };
        let gas_price_u256 = U256::from(gas_price_wei);

        // Reject a genuinely zero gas price (degenerate / unsynced node) — never
        // simulate or act on it. This checks WEI, not gwei: the previous
        // `div_ceil` to whole gwei both hid sub-gwei prices from the cost model
        // and made this guard unreachable, since any nonzero wei price rounded up
        // to at least 1.
        if gas_price_wei == 0 {
            warn!(
                target = "chimera::orchestrator",
                user = %candidate.user,
                "Skipping candidate: gas price is 0 wei"
            );
            return Ok(());
        }

        // 2. Simulation — use the test hook if present, otherwise run the real
        //    REVM simulator. `mock_sim()` is a `None`-returning no-op in release
        //    builds, so the branch collapses to the real simulator path.
        let sim_start = std::time::Instant::now();
        let sim_result = if let Some(mock_result) = self.mock_sim(candidate) {
            mock_result
        } else {
            let mut sim_guard = self.simulator.lock().await;
            let sim = sim_guard
                .as_mut()
                .ok_or_else(|| ChimeraError::ConfigError("simulator not initialized".into()))?;
            let result = sim
                .simulate_liquidation(
                    candidate,
                    gas_price_u256,
                    U256::from(DEFAULT_L1_FEE_SCALAR),
                    // The scan's captured snapshot epoch — NOT a live read of the
                    // shared snapshot, which a mid-scan reload could have advanced.
                    snapshot_block,
                    &self.pacing_cfg,
                )
                .await;
            match result {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        target = "chimera::orchestrator",
                        user = %candidate.user,
                        error = %e,
                        "Simulation failed; skipping candidate"
                    );
                    return Ok(());
                }
            }
        };

        // Metrics boundary: Decimal -> f64 only for the histogram observation.
        // "reverted" is distinct from "unprofitable": the first means the
        // simulation produced no usable number, the second means it produced a
        // number we did not like. Collapsing them hid a total outage behind a
        // label that read as normal market conditions.
        let result_label = if sim_result.revert_reason.is_some() {
            "reverted"
        } else if sim_result.profitable {
            "success"
        } else {
            "unprofitable"
        };
        self.metrics.observe_sim(
            result_label,
            sim_start.elapsed().as_secs_f64(),
            sim_result.gas_used,
            u64::try_from(sim_result.l1_data_fee_wei).unwrap_or(u64::MAX),
            sim_result.expected_profit_usd.to_f64().unwrap_or(0.0),
        );

        // A reverted simulation carries no price information. Previously such a
        // candidate flowed onward with expected_profit_usd = $0.00 and was denied
        // downstream as "InsufficientProfit", which reported a broken simulation
        // as a merely unattractive trade. Stop here instead — the simulator has
        // already logged the decoded revert reason.
        if sim_result.revert_reason.is_some() {
            return Ok(());
        }

        // 2.5 — Route resolution: find a V2 DEX route for this collateral/debt pair.
        // Must happen BEFORE Opportunity construction so the venue field is accurate.
        // Rotation-aware: venues inside the pacing rotation window are skipped
        // here so resolution falls through to the next eligible venue instead
        // of resolving one the rotation gate would deny (candidate drop).
        let route = if candidate.collateral_asset == candidate.debt_asset {
            // Same-asset liquidation: the seized collateral IS the debt asset, so
            // there is no swap leg to route. Requiring a DEX route here would drop
            // a candidate the Executor can already settle (it skips both the router
            // validation and the swap when `collateralAsset == asset`).
            debug!(
                target = "chimera::orchestrator",
                asset = %candidate.debt_asset,
                "Same-asset liquidation; no swap leg required"
            );
            ResolvedV2Route::no_swap(candidate.debt_asset)
        } else {
            let chain_label = self.chain_label_str();
            let resolver = RoutingResolver::new(&self.routing_config, &self.risk_config);
            let recent_venues = self.pacing.engine().recent_venues();
            let resolved = resolver.resolve_v2_eligible(
                candidate.collateral_asset,
                candidate.debt_asset,
                chain_label,
                candidate.debt_to_cover,
                |venue| !recent_venues.iter().any(|recent| recent == venue),
            );

            let Some(resolved) = resolved else {
                warn!(
                    target = "chimera::orchestrator",
                    collateral = %candidate.collateral_asset,
                    debt = %candidate.debt_asset,
                    "No eligible V2 route found; skipping candidate"
                );
                return Ok(());
            };
            resolved
        };

        // 3. Build the opportunity from REAL candidate + simulation fields + resolved route.
        // The selected EOA signs in live mode; the configured Executor remains
        // the transaction target in both live and shadow assembly.
        let eoa_raw = self
            .pacing
            .engine()
            .select_next_eoa()
            .unwrap_or_else(|| FALLBACK_EOA.to_string());

        let eoa = if self.execute_mode == "live" {
            let eoa_parsed: Address = eoa_raw
                .parse()
                .map_err(|_| ChimeraError::ConfigError(format!("bad eoa address: {}", eoa_raw)))?;
            match self.signer_registry.get_signer(&eoa_parsed) {
                Some(_) => {
                    format!("0x{:x}", eoa_parsed)
                }
                None => {
                    return Err(ChimeraError::ConfigError(format!(
                        "Live mode: no registered worker signer for EOA pool entry {} — \
                         ensure a matching keystore exists in worker_keystore_dir",
                        eoa_raw
                    )));
                }
            }
        } else {
            eoa_raw
        };
        let opp = Opportunity {
            id: format!(
                "liq-{}-{}-{}",
                self.chain_id,
                candidate.user,
                chrono::Utc::now().timestamp_millis()
            ),
            expected_net_usd: sim_result.expected_profit_usd,
            gas_price_wei,
            venue: route.venue_name.clone(),
            eoa,
            timestamp: chrono::Utc::now(),
        };

        // 4. Pacing gate: local per-opportunity caps (venue rotation, EOA pool,
        //    single-transfer cap, min interval, profit multiplier). The global
        //    daily/weekly caps are enforced in the following `try_reserve` step
        //    under cross-process advisory lock.
        let local_decision = self.pacing.engine().check(&opp)?;
        let local_allowed = matches!(local_decision, PacingDecision::Allow { .. });
        if let PacingDecision::Deny { reason } = &local_decision {
            info!(
                target = "chimera::orchestrator",
                id = %opp.id,
                reason = %reason,
                "Denied by local pacing gate"
            );
        }

        // Locally denied: stop immediately. Do NOT record outcome, reserve,
        // assemble calldata, or log shadow.  Denied opportunities must not
        // consume pacing caps, advance venue rotation, or update last_release.
        if !local_allowed {
            return Ok(());
        }

        // 5. Unprofitable candidates never execute; record a non-revert, zero-profit outcome.
        if !sim_result.profitable {
            self.pacing
                .engine()
                .record_outcome(&opp, Decimal::ZERO, Decimal::ZERO, false);
            return Ok(());
        }

        // 6. Cross-process reservation: enforce global daily/weekly caps across
        //    all per-chain processes. On reservation failure, record the denial.
        let _reservation = match self.pacing.try_reserve(&opp, self.chain_id) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    target = "chimera::orchestrator",
                    id = %opp.id,
                    error = %e,
                    "Global reservation denied; recording zero outcome"
                );
                self.pacing
                    .engine()
                    .record_outcome(&opp, Decimal::ZERO, Decimal::ZERO, true);
                return Ok(());
            }
        };

        // Gas actually attributable to this liquidation (Decimal, never f64).
        // Computed from wei: rounding the price up to whole gwei here overstated
        // realized spend on Base by ~200x, which inflates `daily_loss_eth` and
        // would trip the max-loss breaker long before real losses justified it.
        let gas_spent_eth = Decimal::from(sim_result.gas_used)
            * Decimal::from_u128(gas_price_wei).unwrap_or_default()
            / Decimal::from(1_000_000_000_000_000_000u64);

        // 6.5 — Strategy assembly: build the same Executor-target request in all modes.
        let worker_eoa: Address = match opp.eoa.parse() {
            Ok(addr) => addr,
            Err(_) => {
                warn!(
                    target = "chimera::orchestrator",
                    eoa = %opp.eoa,
                    "Failed to parse worker EOA address; using ZERO address for shadow calldata"
                );
                Address::ZERO
            }
        };
        let shadow_tx = StrategyAssembler::build_shadow_transaction(
            self.executor,
            &route,
            candidate.collateral_asset,
            candidate.user,
            candidate.debt_to_cover,
            candidate.receive_a_token,
            candidate.debt_asset,
            0, // shadow nonce
        );

        // 7. Execution gate: live + local-allowed + reserved => submit. Else shadow-log.
        if self.execute_mode == "live" && local_allowed {
            let result = self
                .execute_live(
                    &opp,
                    candidate,
                    &sim_result,
                    &route,
                    gas_price_wei,
                    gas_price_u256,
                    gas_spent_eth,
                )
                .await;
            match result {
                Ok(()) => {
                    if let Err(e) = self.pacing.settle(&opp.id, self.chain_id) {
                        warn!(target = "chimera::orchestrator", id = %opp.id, error = %e, "Reservation settlement failed");
                    }
                }
                Err(e) => {
                    warn!(target = "chimera::orchestrator", id = %opp.id, error = %e, "Execution failed; reservation will expire");
                    return Err(e);
                }
            }
        } else {
            // Shadow or locally-denied: release reservation + shadow log the assembled calldata.
            if let Err(e) = self.pacing.settle(&opp.id, self.chain_id) {
                warn!(target = "chimera::orchestrator", id = %opp.id, error = %e, "Shadow reservation settlement failed");
            }
            info!(
                target = "chimera::orchestrator",
                id = %opp.id,
                mode = %self.execute_mode,
                allowed = local_allowed,
                venue = %route.venue_name,
                router = %route.router,
                path_collateral = %route.path.first().map(|a| a.to_string()).unwrap_or_default(),
                path_debt = %route.path.get(1).map(|a| a.to_string()).unwrap_or_default(),
                amount_out_min = %route.amount_out_min,
                expected_profit_usd = %opp.expected_net_usd,
                flash_loan_asset = %candidate.debt_asset,
                flash_loan_amount = %candidate.debt_to_cover,
                signer = %worker_eoa,
                to = %shadow_tx.to,
                calldata_len = shadow_tx.data.len(),
                calldata_hex = %hex::encode(&shadow_tx.data[..]),
                "SHADOW: would submit Executor.execute(bytes) (not broadcasting)"
            );
            // Shadow accounting: record the would-be profit, no gas spent, no revert.
            self.pacing
                .engine()
                .record_outcome(&opp, opp.expected_net_usd, Decimal::ZERO, false);
        }
        Ok(())
    }

    /// Live broadcast path. Only reached when `execute_mode == "live"`, pacing allowed
    /// and the simulation was profitable. Builds an Executor `execute(bytes)`
    /// transaction and signs it with the selected worker EOA.
    #[allow(clippy::too_many_arguments)] // execution plumbing; params travel together from process_candidate
    async fn execute_live(
        &self,
        opp: &Opportunity,
        candidate: &LiquidationCandidate,
        sim_result: &crate::SimulationResult,
        route: &ResolvedV2Route,
        gas_price_wei: u128,
        _gas_price_u256: U256,
        gas_spent_eth: Decimal,
    ) -> Result<(), ChimeraError> {
        let eoa_addr: Address = opp
            .eoa
            .parse()
            .map_err(|_| ChimeraError::ConfigError(format!("bad eoa address: {}", opp.eoa)))?;

        let gas_limit = sim_result.gas_used.saturating_mul(120) / 100;
        let max_fee_per_gas = gas_price_wei.saturating_mul(2);
        let max_priority_fee_per_gas = 1_000_000_000u128.min(max_fee_per_gas);

        // Gas pre-flight: ensure the EOA can cover gas_limit * max_fee_per_gas + safety budget.
        let gas_cost = U256::from(gas_limit.max(200_000)) * U256::from(max_fee_per_gas);
        if !check_eoa_gas_sufficient(&*self.provider, eoa_addr, gas_cost).await? {
            self.pacing
                .engine()
                .record_outcome(opp, Decimal::ZERO, Decimal::ZERO, false);
            return Ok(());
        }

        // Derive min_profit from simulated expected profit, denominated in the debt asset's units.
        // The Executor profit gate compares debt-token balances:
        //   balanceAfter > balanceBefore + minProfit + tip
        //
        // Conversion: only supported for WETH debt assets (matching the routing config pairs).
        // For non-WETH debt assets (e.g. USDC, WBTC), the profit gate is disabled with a warning
        // until multi-asset oracle support is added.
        let weth_addr: alloy::primitives::Address = if self.chain_id == 42161 {
            WETH_ARBITRUM
        } else {
            WETH_BASE
        }
        .parse()
        .expect("valid WETH address constant");

        let is_weth_debt = candidate.debt_asset == weth_addr;
        let is_stable_debt = is_stablecoin(&candidate.debt_asset);

        let min_profit = if is_weth_debt && sim_result.expected_profit_usd > Decimal::ZERO {
            let eth_price = self
                .pacing
                .engine()
                .cached_eth_price()
                .unwrap_or(self.pacing_cfg.eth_price_usd_fallback);

            if eth_price > Decimal::ZERO {
                let target_profit_usd =
                    sim_result.expected_profit_usd * self.risk_config.min_profit_fraction;
                let profit_eth = target_profit_usd / eth_price;
                let wei_per_eth = Decimal::from(1_000_000_000_000_000_000u128);
                let profit_wei_dec = profit_eth * wei_per_eth;
                profit_wei_dec
                    .to_u128()
                    .map(U256::from)
                    .unwrap_or(U256::ZERO)
            } else {
                U256::ZERO
            }
        } else if is_stable_debt && sim_result.expected_profit_usd > Decimal::ZERO {
            let target_profit_usd =
                sim_result.expected_profit_usd * self.risk_config.min_profit_fraction;
            let units_per_usd = Decimal::from(1_000_000u128);
            let profit_units = target_profit_usd * units_per_usd;
            profit_units.to_u128().map(U256::from).unwrap_or(U256::ZERO)
        } else {
            if !is_weth_debt && sim_result.expected_profit_usd > Decimal::ZERO {
                warn!(
                    target = "chimera::orchestrator",
                    debt_asset = %candidate.debt_asset,
                    expected_profit = %sim_result.expected_profit_usd,
                    "Debt asset is not WETH or a known stablecoin — profit gate disabled (min_profit = 0). \
                     Non-WETH/non-stablecoin oracle support is deferred to a future release."
                );
            }
            U256::ZERO
        };

        // Safety: refuse live submission when the profit gate is disabled (min_profit == 0).
        // Without a calibrated min_profit, the Executor profit gate is a no-op and the
        // strategy could submit a transaction that loses money after flash-loan premium.
        if min_profit == U256::ZERO && sim_result.expected_profit_usd > Decimal::ZERO {
            warn!(
                target = "chimera::orchestrator",
                id = %opp.id,
                debt_asset = %candidate.debt_asset,
                "Refusing live submission: min_profit == 0 with positive expected profit. \
                 The profit gate would not protect against adverse execution."
            );
            self.pacing
                .engine()
                .record_outcome(opp, Decimal::ZERO, gas_spent_eth, false);
            return Ok(());
        }

        let tip = U256::ZERO;
        let deadline = chrono::Utc::now().timestamp() as u64 + 300;

        let built = StrategyAssembler::build_transaction(
            self.executor,
            route,
            candidate.collateral_asset,
            candidate.user,
            candidate.debt_to_cover,
            candidate.receive_a_token,
            candidate.debt_asset,
            min_profit,
            tip,
            deadline,
            0,
        );

        let mut tx = built;
        tx.gas_limit = gas_limit.max(200_000);
        tx.max_fee_per_gas = max_fee_per_gas;
        tx.max_priority_fee_per_gas = max_priority_fee_per_gas;

        let worker_signer = self.signer_registry.get_signer(&eoa_addr).ok_or_else(|| {
            ChimeraError::ConfigError(format!(
                "live execution requires a registered worker signer for {eoa_addr}"
            ))
        })?;
        worker_signer.sync_from_chain(&*self.provider).await?;
        tx.nonce = worker_signer.next_nonce();

        let alloy_tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce: tx.nonce,
            max_fee_per_gas: tx.max_fee_per_gas,
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas,
            gas_limit: tx.gas_limit,
            to: alloy::primitives::TxKind::Call(tx.to),
            value: tx.value,
            input: tx.data.clone(),
            ..Default::default()
        };
        let sig_hash = alloy_tx.signature_hash();
        let sig = worker_signer
            .signer
            .sign_hash(&sig_hash)
            .await
            .map_err(|e| ChimeraError::RpcError(format!("worker signing: {e}")))?;
        let signed = alloy_tx.into_signed(sig);
        let raw = signed.encoded_2718();
        let tx_hash_result = (*self.provider)
            .send_raw_transaction(&raw)
            .await
            .map(|pending| *pending.tx_hash())
            .map_err(|e| ChimeraError::RpcError(format!("raw send: {e}")));

        match tx_hash_result {
            Ok(tx_hash) => {
                info!(
                    target = "chimera::orchestrator",
                    id = %opp.id,
                    %tx_hash,
                    "Liquidation submitted"
                );
                self.pacing.engine().record_outcome(
                    opp,
                    opp.expected_net_usd,
                    gas_spent_eth,
                    false,
                );
            }
            Err(e) => {
                warn!(
                    target = "chimera::orchestrator",
                    id = %opp.id,
                    error = %e,
                    "Liquidation submission failed"
                );
                return Err(e);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn production_token_addresses_parse_and_are_nonzero() {
        for (name, value, expected) in [
            (
                "Base WETH",
                WETH_BASE,
                address!("0x4200000000000000000000000000000000000006"),
            ),
            (
                "Arbitrum WETH",
                WETH_ARBITRUM,
                address!("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"),
            ),
            (
                "Base USDC",
                USDC_BASE,
                address!("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"),
            ),
            (
                "Arbitrum USDC",
                USDC_ARBITRUM,
                address!("0xaf88d065e77c8cC2239327C5EDb3A432268e5831"),
            ),
            (
                "Base USDT",
                USDT_BASE,
                address!("0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2"),
            ),
            (
                "Arbitrum USDT",
                USDT_ARBITRUM,
                address!("0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9"),
            ),
        ] {
            let address = value
                .parse::<Address>()
                .unwrap_or_else(|e| panic!("{name} address {value:?} did not parse: {e}"));
            assert_ne!(address, Address::ZERO, "{name} address was zero");
            assert_eq!(address, expected, "{name} address changed");
        }
    }

    #[test]
    fn eip1559_priority_fee_never_exceeds_max_fee() {
        let one_gwei: u128 = 1_000_000_000;
        for gas_price_gwei in &[0.001_f64, 0.1, 1.0, 10.0] {
            let gas_price_wei = (*gas_price_gwei * 1e9) as u128;
            let max_fee = gas_price_wei.saturating_mul(2);
            let priority_fee = one_gwei.min(max_fee);
            assert!(
                priority_fee <= max_fee,
                "priority_fee {} > max_fee {} for gas_price={} gwei",
                priority_fee,
                max_fee,
                gas_price_gwei
            );
            if max_fee > 0 {
                assert!(
                    priority_fee > 0,
                    "priority_fee should be > 0 when max_fee > 0; gas_price={} gwei",
                    gas_price_gwei
                );
            }
        }
    }
}
