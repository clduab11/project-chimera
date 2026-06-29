// Production orchestrator entrypoint for Chimera core.
//
// Boot sequence:
//   1. File logging (daily rotation) + stdout, layered via tracing_subscriber registry.
//   2. Load pacing config (with CHIMERA_* env overrides).
//   3. Start Prometheus metrics server.
//   4. validate_mode_transition() against core/state/mode.json BEFORE the loop.
//   5. Resolve RPC endpoint (BASE_RPC_URL / ARB_RPC_URL / RPC_URL / localhost).
//   6. Hydrate the detector snapshot (graceful empty fallback if missing).
//   7. Load keystore signer (required for live, optional for shadow).
//   8. Build the provider (wallet-backed or read-only) and run.
//
// Safety: defaults to "shadow"; never flips to "live"; never logs secrets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::network::{Ethereum, EthereumWallet};
use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use chimera_core::{
    start_metrics_server, AaveOracle, ChainlinkOracle, JsonlPersistence, L2ChainType,
    LiquidationSimulator, MarketSnapshot, Metrics, Orchestrator, OrchestratorConfig, PacingConfig,
    PacingEngine, PriceOracle, RpcSubmitter,
};

/// Persistent execute-mode state used to enforce the shadow→live transition rule.
/// Stored at `core/state/mode.json`.
#[derive(Deserialize, Serialize, Debug)]
struct ModeState {
    #[serde(default = "default_previous_mode")]
    previous_mode: String,
    /// Unix seconds when shadow mode began (0 == unknown / not stamped).
    #[serde(default)]
    shadow_since: u64,
}

fn default_previous_mode() -> String {
    "shadow".into()
}

/// Resolved Aave V3 addresses for the active chain.
struct AaveAddresses {
    pool: Address,
    oracle: Address,
    pool_data_provider: Address,
    /// Canonical WETH — used as the simulator's ETH oracle asset.
    weth: Address,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // (a) File logging with daily rotation + stdout. `_guard` MUST live for the
    //     whole program or buffered log lines are dropped on exit.
    let file_appender = tracing_appender::rolling::daily("logs", "chimera.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("chimera=info".parse().expect("valid env directive"));
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false),
        )
        .init();

    let pacing_cfg = PacingConfig::load_with_env("config/pacing.yaml")?;
    let metrics = Arc::new(Metrics::new());
    let _metrics_handle = start_metrics_server(pacing_cfg.metrics_port).await?;

    // (e) Mode-transition gate. Load core/state/mode.json (default: shadow / no timestamp).
    let mode_path = PathBuf::from("core/state/mode.json");
    let (previous_mode, shadow_since) = match load_mode_state(&mode_path) {
        Some(m) => {
            let ss = if m.shadow_since > 0 {
                Some(UNIX_EPOCH + Duration::from_secs(m.shadow_since))
            } else {
                None
            };
            (m.previous_mode, ss)
        }
        None => ("shadow".to_string(), None),
    };
    pacing_cfg.validate_mode_transition(&previous_mode, shadow_since)?;

    // If we are (and remain) in shadow and no mode file exists, stamp shadow_since = now.
    if pacing_cfg.execute_mode == "shadow" && !mode_path.exists() {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let state = ModeState {
            previous_mode: "shadow".into(),
            shadow_since: now_secs,
        };
        if let Some(parent) = mode_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&state) {
            Ok(s) => match std::fs::write(&mode_path, s) {
                Ok(()) => info!(path = %mode_path.display(), "Stamped shadow_since in mode.json"),
                Err(e) => warn!(error = %e, "Failed to write mode.json"),
            },
            Err(e) => warn!(error = %e, "Failed to serialize mode.json"),
        }
    }

    // (b) RPC endpoint resolution. Log only the env-var SOURCE name (URLs may carry creds).
    let (rpc_url, rpc_source) = resolve_rpc_url(pacing_cfg.chain_id);
    info!(source = %rpc_source, chain_id = pacing_cfg.chain_id, "Resolved RPC endpoint");

    // (d) Detector snapshot. Missing/invalid => empty snapshot (zero candidates = safe).
    let snapshot_path = PathBuf::from(
        std::env::var("CHIMERA_SNAPSHOT_PATH").unwrap_or_else(|_| "config/snapshot.json".into()),
    );
    let snapshot = match MarketSnapshot::load_from_file(&snapshot_path) {
        Ok(s) => {
            info!(
                path = %snapshot_path.display(),
                reserves = s.reserves.len(),
                users = s.users.len(),
                "Loaded market snapshot"
            );
            s
        }
        Err(e) => {
            warn!(
                path = %snapshot_path.display(),
                error = %e,
                "Snapshot unavailable; starting with empty snapshot (zero candidates)"
            );
            MarketSnapshot::default()
        }
    };

    // (c) Signer. Required for live; optional (read-only) for shadow. Never logs secrets.
    let signer = load_signer(&pacing_cfg.execute_mode)?;

    // (d) Provider-type divergence is resolved by monomorphizing `run_with_provider`
    //     over the two concrete provider types (wallet-backed vs read-only).
    match signer {
        Some(signer) => {
            info!("Signer loaded; building wallet-backed provider");
            let wallet = EthereumWallet::from(signer);
            let provider = Arc::new(ProviderBuilder::new().wallet(wallet).connect_http(rpc_url));
            run_with_provider(provider, pacing_cfg, metrics, snapshot, snapshot_path).await
        }
        None => {
            info!("No signer configured; building read-only provider (shadow)");
            let provider = Arc::new(ProviderBuilder::new().connect_http(rpc_url));
            run_with_provider(provider, pacing_cfg, metrics, snapshot, snapshot_path).await
        }
    }
}

/// Construct every runtime component for a concrete provider type and run the loop.
///
/// Monomorphized once per concrete `P`, so the wallet-backed and read-only providers
/// (which have different concrete types) both flow through here without trait objects.
async fn run_with_provider<P>(
    provider: Arc<P>,
    pacing_cfg: PacingConfig,
    metrics: Arc<Metrics>,
    snapshot: MarketSnapshot,
    snapshot_path: PathBuf,
) -> anyhow::Result<()>
where
    P: Provider<Ethereum> + Clone + Send + Sync + 'static,
{
    let chain_id = pacing_cfg.chain_id;
    let execute_mode = pacing_cfg.execute_mode.clone();
    let addrs = resolve_aave_addresses(chain_id);
    let staleness = Duration::from_secs(pacing_cfg.oracle_staleness_seconds);

    // Oracles. Aave is the simulator's primary USD price source; Chainlink is wired
    // with an empty feed map (no feeds configured yet) and kept available.
    let feeds: HashMap<Address, Address> = HashMap::new();
    let feeds_configured = feeds.len();
    let _chainlink = ChainlinkOracle::new((*provider).clone(), feeds, staleness);
    let aave_oracle = AaveOracle::new((*provider).clone(), addrs.oracle, staleness);
    let oracle: Arc<dyn PriceOracle> = Arc::new(aave_oracle);
    info!(
        target = "chimera::oracle",
        feeds_configured, "Oracles wired (Aave primary, Chainlink available)"
    );

    // Simulator (REVM warm DB) with chain-specific L1 data-fee model.
    let chain_type = if chain_id == 42161 {
        L2ChainType::Arbitrum
    } else {
        L2ChainType::Base
    };
    let mut simulator = LiquidationSimulator::new(
        provider.clone(),
        addrs.pool,
        addrs.oracle,
        oracle,
        addrs.pool_data_provider,
        addrs.weth,
    )
    .await?
    .with_l2_chain_type(chain_type);

    // Optional prewarm of the simulator's REVM DB from the same snapshot file (non-fatal).
    if snapshot_path.exists() {
        if let Err(e) = simulator.load_snapshot(&snapshot_path).await {
            warn!(
                target = "chimera::simulator",
                error = %e,
                "Simulator prewarm failed; continuing cold"
            );
        }
    }

    // Submitter: dry_run unless explicitly live.
    let submitter =
        RpcSubmitter::new((*provider).clone(), chain_id).with_dry_run(execute_mode != "live");

    // Pacing engine with JSONL outcome persistence.
    let persistence = Arc::new(JsonlPersistence::new(
        PathBuf::from("core/state/outcomes.jsonl"),
        50,
        5,
    ));
    let pacing = PacingEngine::new(pacing_cfg.clone()).with_state_persistence(persistence);

    let orchestrator = Orchestrator::new(
        OrchestratorConfig::default(),
        pacing,
        pacing_cfg,
        metrics,
        provider,
        snapshot,
        chain_id,
        simulator,
        submitter,
        execute_mode,
        addrs.pool,
    );

    orchestrator.run().await?;
    Ok(())
}

/// Resolve the RPC endpoint URL and the env-var name it came from.
///
/// Order: chain-specific (`BASE_RPC_URL` for Base, `ARB_RPC_URL` for Arbitrum),
/// then `RPC_URL`, then `http://localhost:8545`.
fn resolve_rpc_url(chain_id: u64) -> (url::Url, String) {
    let order: [&str; 2] = if chain_id == 42161 {
        ["ARB_RPC_URL", "RPC_URL"]
    } else {
        ["BASE_RPC_URL", "RPC_URL"]
    };
    for key in order {
        if let Ok(v) = std::env::var(key) {
            match v.parse::<url::Url>() {
                Ok(u) => return (u, key.to_string()),
                Err(_) => warn!(env = key, "RPC URL env var failed to parse; trying next source"),
            }
        }
    }
    let fallback = "http://localhost:8545"
        .parse::<url::Url>()
        .expect("valid default RPC URL");
    (fallback, "default(localhost:8545)".to_string())
}

/// Per-chain Aave V3 addresses (mirrors `config/pools.toml`), each overridable via env.
/// Invalid/empty values degrade to `Address::ZERO` so the binary never panics on boot.
fn resolve_aave_addresses(chain_id: u64) -> AaveAddresses {
    let addr = |default: &str, env_key: &str| -> Address {
        let s = std::env::var(env_key).unwrap_or_else(|_| default.to_string());
        Address::from_str(&s).unwrap_or(Address::ZERO)
    };
    if chain_id == 42161 {
        AaveAddresses {
            pool: addr("0x794a61358D6845594F94dc1DB02A252b5b4814aD", "CHIMERA_AAVE_POOL"),
            oracle: addr("0xb56c2F0B8Be1bd3C6f81A24fe035B91ea9E14711", "CHIMERA_AAVE_ORACLE"),
            pool_data_provider: addr(
                "0x69FA688f1Dc47d4B5d8029D5a35FB7a5480d4d96",
                "CHIMERA_POOL_DATA_PROVIDER",
            ),
            weth: addr(
                "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
                "CHIMERA_ETH_ORACLE_ASSET",
            ),
        }
    } else {
        AaveAddresses {
            pool: addr("0xA238Dd80C259a72e81d7e4666a2CEDEcD3cA5cB", "CHIMERA_AAVE_POOL"),
            oracle: addr(
                "0x2DaD3A13EF0C636622150F51bA8b404Fd4c56B98c",
                "CHIMERA_AAVE_ORACLE",
            ),
            pool_data_provider: addr(
                "0x0F43731EB8d45A581f4a36DD74F5f358bc90C73A",
                "CHIMERA_POOL_DATA_PROVIDER",
            ),
            weth: addr(
                "0x4200000000000000000000000000000000000006",
                "CHIMERA_ETH_ORACLE_ASSET",
            ),
        }
    }
}

/// Load and decrypt a keystore signer.
///
/// - Both `CHIMERA_KEYSTORE_PATH` + `CHIMERA_KEYSTORE_PASSWORD` present => decrypt.
/// - Missing AND shadow => `Ok(None)` (read-only).
/// - Missing AND live => error (refuse live without a signer).
///
/// Never logs the password or key material.
fn load_signer(execute_mode: &str) -> anyhow::Result<Option<PrivateKeySigner>> {
    let path = std::env::var("CHIMERA_KEYSTORE_PATH").ok();
    let password = std::env::var("CHIMERA_KEYSTORE_PASSWORD").ok();
    match (path, password) {
        (Some(p), Some(pw)) => {
            let signer = PrivateKeySigner::decrypt_keystore(&p, &pw)
                .map_err(|e| anyhow::anyhow!("keystore decrypt failed: {e}"))?;
            Ok(Some(signer))
        }
        _ => {
            if execute_mode == "live" {
                anyhow::bail!(
                    "execute_mode=live requires CHIMERA_KEYSTORE_PATH and CHIMERA_KEYSTORE_PASSWORD"
                );
            }
            warn!("No keystore configured (CHIMERA_KEYSTORE_PATH/PASSWORD); running read-only shadow");
            Ok(None)
        }
    }
}

fn load_mode_state(path: &Path) -> Option<ModeState> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}
