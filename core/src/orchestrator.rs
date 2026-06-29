//! Continuous detection → simulation → execution orchestrator loop.

use crate::{
    check_eoa_gas_sufficient, BuiltTransaction, ChimeraError, LiquidationCandidate,
    LiquidationDetector, LiquidationSimulator, MarketSnapshot, Metrics, Opportunity, PacingConfig,
    PacingDecision, PacingEngine, RpcSubmitter, TransactionExecutor,
};
use alloy::network::Ethereum;
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::Provider;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

/// Baseline OP-stack / Arbitrum L1 fee scalar (scalar / 1e6 == 1.0). The simulator's
/// chain-specific fee model refines this; a neutral baseline is used per scan.
const DEFAULT_L1_FEE_SCALAR: u64 = 1_000_000;

/// Fallback EOA used only for shadow logging when no clean EOA pool is loaded.
const FALLBACK_EOA: &str = "0x0000000000000000000000000000000000000000";

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
/// and a pre-built [`PacingEngine`]. Execution is gated on
/// `execute_mode == "live"` AND pacing-allowed AND profitable.
pub struct Orchestrator<P: Provider<Ethereum> + Clone + Send + Sync + 'static> {
    config: OrchestratorConfig,
    pacing: PacingEngine,
    pacing_cfg: PacingConfig,
    metrics: Arc<Metrics>,
    provider: Arc<P>,
    snapshot: MarketSnapshot,
    chain_id: u64,
    simulator: Mutex<LiquidationSimulator<P>>,
    submitter: RpcSubmitter<P>,
    execute_mode: String,
    aave_pool: Address,
}

impl<P: Provider<Ethereum> + Clone + Send + Sync + 'static> Orchestrator<P> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: OrchestratorConfig,
        pacing: PacingEngine,
        pacing_cfg: PacingConfig,
        metrics: Arc<Metrics>,
        provider: Arc<P>,
        snapshot: MarketSnapshot,
        chain_id: u64,
        simulator: LiquidationSimulator<P>,
        submitter: RpcSubmitter<P>,
        execute_mode: String,
        aave_pool: Address,
    ) -> Self {
        Self {
            config,
            pacing,
            pacing_cfg,
            metrics,
            provider,
            snapshot,
            chain_id,
            simulator: Mutex::new(simulator),
            submitter,
            execute_mode,
            aave_pool,
        }
    }

    pub async fn run(&self) -> Result<(), ChimeraError> {
        info!(
            target = "chimera::orchestrator",
            chain_id = self.chain_id,
            execute_mode = %self.execute_mode,
            "Orchestrator starting"
        );
        let detector = LiquidationDetector::new(self.snapshot.clone(), self.chain_id);
        let chain_label = if self.chain_id == 8453 { "base" } else { "arbitrum" };

        // Resolve the emergency flag path once. Operators (scripts/emergency_pause.py)
        // create this file to halt and DELETE it to resume.
        let emergency_flag_path = std::path::PathBuf::from(
            std::env::var("CHIMERA_EMERGENCY_FLAG")
                .unwrap_or_else(|_| "core/state/emergency.flag".into()),
        );

        loop {
            // Emergency check at the TOP of each scan: if active, force-trip the breaker
            // and skip the entire detect/simulate/submit pipeline this iteration.
            if let Some(reason) = Self::read_emergency_flag(&emergency_flag_path) {
                self.handle_emergency(&reason);
            } else {
                let candidates = detector.find_at_risk_positions();
                self.metrics.observe_candidate(chain_label);

                for candidate in candidates.iter().take(self.config.max_opportunities_per_scan) {
                    if let Err(e) = self.process_candidate(candidate).await {
                        warn!(target = "chimera::orchestrator", error = %e, "Candidate failed");
                    }
                }
            }

            // Update rolling gauges + breaker state every scan (Decimal -> f64 only here).
            let rs = self.pacing.current_risk_state();
            self.metrics
                .set_daily_net_usd(rs.daily_net_usd.to_f64().unwrap_or(0.0));
            self.metrics
                .set_weekly_net_usd(rs.weekly_net_usd.to_f64().unwrap_or(0.0));
            self.metrics
                .set_breaker_state(self.pacing.is_breaker_active());

            // Inter-scan wait, polled in EMERGENCY_POLL_SECS chunks so an emergency flag
            // raised mid-wait is detected within <= 3s rather than after a full scan interval.
            let mut waited = 0u64;
            while waited < self.config.scan_interval_secs {
                if let Some(reason) = Self::read_emergency_flag(&emergency_flag_path) {
                    // Handle immediately and break out to re-evaluate at the top of the loop.
                    self.handle_emergency(&reason);
                    break;
                }
                let chunk =
                    EMERGENCY_POLL_SECS.min(self.config.scan_interval_secs - waited);
                sleep(Duration::from_secs(chunk)).await;
                waited += chunk;
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
        self.pacing.trip_emergency(reason);
        self.metrics.set_breaker_state(true);
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

    async fn process_candidate(
        &self,
        candidate: &LiquidationCandidate,
    ) -> Result<(), ChimeraError> {
        // 1. Current gas price (drives both pacing gas estimate and the simulation env).
        let gas_price_wei = self
            .provider
            .get_gas_price()
            .await
            .map_err(|e| ChimeraError::RpcError(format!("get_gas_price failed: {e}")))?;
        let gas_price_u256 = U256::from(gas_price_wei);
        // Ceiling division: any non-zero wei price yields >= 1 gwei, so a 0 estimate
        // means the node returned a literally-zero gas price (degenerate/unsynced).
        // This avoids wrongly skipping every candidate on L2 sub-gwei base fees.
        let gas_estimate_gwei = gas_price_wei.div_ceil(1_000_000_000u128) as u64;

        // Reject zero gas estimate (degenerate / unsynced node) — never simulate or act on it.
        if gas_estimate_gwei == 0 {
            warn!(
                target = "chimera::orchestrator",
                user = %candidate.user,
                "Skipping candidate: gas estimate is 0 gwei"
            );
            return Ok(());
        }

        // 2. Real high-fidelity simulation (simulator owns &mut self via the Mutex).
        let sim_start = std::time::Instant::now();
        let sim_result = {
            let mut sim = self.simulator.lock().await;
            sim.simulate_liquidation(
                candidate,
                gas_price_u256,
                U256::from(DEFAULT_L1_FEE_SCALAR),
                self.snapshot.block_number,
                &self.pacing_cfg,
            )
            .await
        };
        let sim_result = match sim_result {
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
        };

        // Metrics boundary: Decimal -> f64 only for the histogram observation.
        let result_label = if sim_result.profitable {
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

        // 3. Build the opportunity from REAL candidate + simulation fields.
        let eoa = self
            .pacing
            .select_next_eoa()
            .unwrap_or_else(|| FALLBACK_EOA.to_string());
        let opp = Opportunity {
            id: format!("liq-{}", candidate.user),
            expected_net_usd: sim_result.expected_profit_usd,
            gas_estimate_gwei,
            venue: "aerodrome".into(),
            eoa,
            timestamp: chrono::Utc::now(),
        };

        // 4. Pacing gate (caps, jitter, breaker, venue/EOA rotation).
        let decision = self.pacing.check(&opp)?;
        let allowed = matches!(decision, PacingDecision::Allow { .. });
        if let PacingDecision::Deny { reason } = &decision {
            info!(
                target = "chimera::orchestrator",
                id = %opp.id,
                reason = %reason,
                "Denied by pacing"
            );
        }

        // 5. Unprofitable candidates never execute; record a non-revert, zero-profit outcome.
        if !sim_result.profitable {
            self.pacing
                .record_outcome(&opp, Decimal::ZERO, Decimal::ZERO, false);
            return Ok(());
        }

        // Gas actually attributable to this liquidation (Decimal, never f64).
        let gas_spent_eth = Decimal::from(sim_result.gas_used) * Decimal::from(gas_estimate_gwei)
            / Decimal::from(1_000_000_000u64);

        // 6. Execution gate: live + pacing-allowed + profitable => submit. Else shadow-log.
        if self.execute_mode == "live" && allowed {
            self.execute_live(&opp, &sim_result, gas_price_wei, gas_price_u256, gas_spent_eth)
                .await
        } else {
            info!(
                target = "chimera::orchestrator",
                id = %opp.id,
                mode = %self.execute_mode,
                allowed = allowed,
                expected_profit_usd = %opp.expected_net_usd,
                to = %self.aave_pool,
                calldata_len = sim_result.calldata.len(),
                "SHADOW: would submit liquidation (not broadcasting)"
            );
            // Shadow accounting: record the would-be profit, no gas spent, no revert.
            self.pacing
                .record_outcome(&opp, opp.expected_net_usd, Decimal::ZERO, false);
            Ok(())
        }
    }

    /// Live broadcast path. Only reached when `execute_mode == "live"`, pacing allowed
    /// and the simulation was profitable. Performs a gas pre-flight, builds and submits.
    async fn execute_live(
        &self,
        opp: &Opportunity,
        sim_result: &crate::SimulationResult,
        gas_price_wei: u128,
        gas_price_u256: U256,
        gas_spent_eth: Decimal,
    ) -> Result<(), ChimeraError> {
        let eoa_addr: Address = opp
            .eoa
            .parse()
            .map_err(|_| ChimeraError::ConfigError(format!("bad eoa address: {}", opp.eoa)))?;

        // Gas pre-flight: ensure the EOA can cover the estimated gas + safety budget.
        let gas_cost = gas_price_u256 * U256::from(sim_result.gas_used.max(200_000));
        if !check_eoa_gas_sufficient(&*self.provider, eoa_addr, gas_cost).await? {
            self.pacing
                .record_outcome(opp, Decimal::ZERO, Decimal::ZERO, true);
            return Ok(());
        }

        let nonce = self.submitter.fetch_nonce(eoa_addr).await?;
        let gas_limit = sim_result.gas_used.saturating_mul(120) / 100;
        let built = BuiltTransaction {
            to: self.aave_pool,
            data: Bytes::from(sim_result.calldata.clone()),
            value: U256::ZERO,
            gas_limit: gas_limit.max(200_000),
            max_fee_per_gas: gas_price_wei.saturating_mul(2),
            max_priority_fee_per_gas: 1_000_000_000u128,
            nonce,
        };

        match self.submitter.submit(built).await {
            Ok(tx_hash) => {
                info!(
                    target = "chimera::orchestrator",
                    id = %opp.id,
                    %tx_hash,
                    "Liquidation submitted"
                );
                self.pacing
                    .record_outcome(opp, opp.expected_net_usd, gas_spent_eth, false);
            }
            Err(e) => {
                warn!(
                    target = "chimera::orchestrator",
                    id = %opp.id,
                    error = %e,
                    "Liquidation submission failed"
                );
                self.pacing
                    .record_outcome(opp, Decimal::ZERO, gas_spent_eth, true);
            }
        }
        Ok(())
    }
}
