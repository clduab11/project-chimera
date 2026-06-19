// Production orchestrator entrypoint for Chimera core.

use chimera_core::{
    start_metrics_server, Orchestrator, OrchestratorConfig, PacingConfig,
};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("chimera=info".parse().expect("valid env directive")),
        )
        .init();

    let pacing_cfg = PacingConfig::load_with_env("config/pacing.yaml")?;
    let metrics = Arc::new(chimera_core::Metrics::new());
    let _metrics_handle = start_metrics_server(pacing_cfg.metrics_port).await?;

    // TODO: replace with real Alloy provider wired from RPC_URL env or config
    let snapshot = chimera_core::MarketSnapshot::default();
    let rpc_url: url::Url = std::env::var("RPC_URL")
        .unwrap_or_else(|_| "http://localhost:8545".into())
        .parse()
        .expect("valid RPC URL");
    let provider = Arc::new(alloy::providers::ProviderBuilder::new().connect_http(rpc_url));

    let orch_cfg = OrchestratorConfig::default();
    let orchestrator = Orchestrator::new(
        orch_cfg,
        pacing_cfg.clone(),
        metrics,
        provider,
        snapshot,
        pacing_cfg.chain_id,
    );

    Ok(orchestrator.run().await?)
}
