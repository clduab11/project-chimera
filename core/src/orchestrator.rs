//! Continuous detection → simulation → execution orchestrator loop.

use crate::{
    check_eoa_gas_sufficient, ChimeraError, LiquidationCandidate, LiquidationDetector,
    MarketSnapshot, Metrics, Opportunity, PacingConfig, PacingDecision, PacingEngine,
};
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

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
pub struct Orchestrator<P: Provider + Send + Sync + 'static> {
    config: OrchestratorConfig,
    pacing: PacingEngine,
    metrics: Arc<Metrics>,
    provider: Arc<P>,
    snapshot: MarketSnapshot,
    chain_id: u64,
}

impl<P: Provider + Send + Sync + 'static> Orchestrator<P> {
    pub fn new(
        config: OrchestratorConfig,
        pacing: PacingConfig,
        metrics: Arc<Metrics>,
        provider: Arc<P>,
        snapshot: MarketSnapshot,
        chain_id: u64,
    ) -> Self {
        Self {
            config,
            pacing: PacingEngine::new(pacing),
            metrics,
            provider,
            snapshot,
            chain_id,
        }
    }

    pub async fn run(&self) -> Result<(), ChimeraError> {
        info!(target = "chimera::orchestrator", chain_id = self.chain_id, "Orchestrator starting");
        let detector = LiquidationDetector::new(self.snapshot.clone(), self.chain_id);
        let chain_label = if self.chain_id == 8453 { "base" } else { "arbitrum" };

        loop {
            let candidates = detector.find_at_risk_positions();
            self.metrics.observe_candidate(chain_label);

            for candidate in candidates.iter().take(self.config.max_opportunities_per_scan) {
                if let Err(e) = self.process_candidate(candidate).await {
                    warn!(target = "chimera::orchestrator", error = %e, "Candidate failed");
                }
            }
            sleep(Duration::from_secs(self.config.scan_interval_secs)).await;
        }
    }

    async fn process_candidate(
        &self,
        candidate: &LiquidationCandidate,
    ) -> Result<(), ChimeraError> {
        // Placeholder opportunity mapping until detector emits enriched candidates.
        let opp = Opportunity {
            id: format!("liq-{}", candidate.user),
            expected_net_usd: Decimal::from_f64_retain(500.0).unwrap_or(Decimal::ZERO),
            gas_estimate_gwei: 50,
            venue: "aerodrome".into(),
            eoa: "0x1111111111111111111111111111111111111111".into(),
            timestamp: chrono::Utc::now(),
        };

        match self.pacing.check(&opp)? {
            PacingDecision::Deny { reason } => {
                info!(target = "chimera::orchestrator", id = %opp.id, reason = %reason, "Denied by pacing");
                return Ok(());
            }
            PacingDecision::Allow { .. } => {}
        }

        // Gas pre-flight
        let eoa_addr: Address = opp.eoa.parse().map_err(|_| ChimeraError::ConfigError("bad eoa".into()))?;
        let gas_cost = U256::from(opp.gas_estimate_gwei) * U256::from(200_000u64) * U256::from(1_000_000_000u128);
        if !check_eoa_gas_sufficient(&*self.provider, eoa_addr, gas_cost).await? {
            self.pacing.record_outcome(&opp, Decimal::ZERO, Decimal::ZERO, true);
            return Ok(());
        }

        // Placeholder simulation outcome
        let realized = Decimal::from_f64_retain(50.0).unwrap_or(Decimal::ZERO);
        let gas_spent = Decimal::from_f64_retain(0.0003).unwrap_or(Decimal::ZERO);
        self.pacing.record_outcome(&opp, realized, gas_spent, realized <= Decimal::ZERO);
        Ok(())
    }
}
