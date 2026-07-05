// Production orchestrator entrypoint for Chimera core.
//
// Boot sequence:
//   1. File logging (daily rotation) + stdout, layered via tracing_subscriber registry.
//   2. Load pacing config (with CHIMERA_* env overrides).
//   3. Start Prometheus metrics server.
//   4. validate_mode_transition() against core/state/mode.json BEFORE the loop.
//   5. Resolve RPC endpoint (BASE_RPC_URL / ARB_RPC_URL / RPC_URL / localhost).
//   6. Hydrate the detector snapshot (graceful empty fallback if missing).
//   7. Load SignerRegistry (worker + treasury keystores); live mode validates all EOA pool entries have signers.
//   8. Build a read-only provider; the orchestrator and scheduler sign locally with managed signers.
//
// Safety: defaults to "shadow"; never flips to "live"; never logs secrets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::network::Ethereum;
use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use chimera_core::{
    start_metrics_server, AaveOracle, BlockWatch, ChainlinkOracle, CrossProcessPacing,
    JsonlPersistence, L2ChainType, LiquidationSimulator, MarketSnapshot, MempoolWatcher, Metrics,
    Orchestrator, OrchestratorConfig, PacingConfig, PacingEngine, PriceOracle, RiskConfig,
    RoutingConfig, RpcSubmitter, SequencerFeed, SignerRegistry, SweepScheduler, WatchEvent,
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

/// CLI arguments for chain-scoped process startup.
/// Each chain runs as its own OS process, sharing the pacing store.
#[derive(Debug)]
struct CliArgs {
    /// Override the chain_id from pacing.yaml (e.g., 8453 for Base, 42161 for Arbitrum).
    chain_id: Option<u64>,
    /// WebSocket URL for the MempoolWatcher (BlockWatch). If omitted, the
    /// orchestrator falls back to fixed-interval polling.
    ws_url: Option<String>,
    /// Offset added to metrics_port so multiple chain processes don't collide.
    /// Default: chain_id % 1000.
    metrics_port_offset: Option<u16>,
}

/// Parse CLI arguments. Simple manual parsing — avoids pulling in clap for
/// a single binary with 3 optional flags.
fn parse_cli_args() -> CliArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut chain_id = None;
    let mut ws_url = None;
    let mut metrics_port_offset = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--chain-id" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    chain_id = val.parse::<u64>().ok();
                }
            }
            "--ws-url" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    ws_url = Some(val.clone());
                }
            }
            "--metrics-port-offset" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    metrics_port_offset = val.parse::<u16>().ok();
                }
            }
            _ => {}
        }
        i += 1;
    }
    CliArgs {
        chain_id,
        ws_url,
        metrics_port_offset,
    }
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

    let cli = parse_cli_args();
    let mut pacing_cfg = PacingConfig::load_with_env("config/pacing.yaml")?;

    // CLI override: chain-scoped process model. Each process can pick a
    // specific chain via --chain-id; multiple processes share the same
    // CrossProcessPacing reservations directory and outcomes audit trail.
    if let Some(cid) = cli.chain_id {
        pacing_cfg.chain_id = cid;
        info!(chain_id = cid, "CLI override: chain_id set");
    }

    // Metrics port: avoid collisions when multiple chain processes bind on
    // the same host. Default offset = chain_id % 1000, overridable via CLI.
    //
    // NOTE: The default offset can collide for chain IDs with the same
    // mod-1000 residue (e.g., 8453 and 108453). When running multiple
    // instances on the same host, use --metrics-port-offset explicitly.
    let port_offset = cli.metrics_port_offset.unwrap_or((pacing_cfg.chain_id % 1000) as u16);
    pacing_cfg.metrics_port = pacing_cfg.metrics_port.saturating_add(port_offset);
    let risk_cfg = RiskConfig::load("config/risk.yaml").unwrap_or_else(|e| {
        warn!(error = %e, "Failed to load risk.yaml; using defaults");
        RiskConfig::default()
    });
    let routing_cfg = RoutingConfig::load("config/routing.yaml").unwrap_or_else(|e| {
        warn!(error = %e, "Failed to load routing.yaml; using defaults");
        RoutingConfig::default()
    });
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

    // (c) SignerRegistry — replaces single-signer load_signer().
    //     Loads treasury + worker keystores from pacing.yaml paths.
    let signer_registry = Arc::new(
        SignerRegistry::load(&pacing_cfg, &pacing_cfg.execute_mode)?
    );

    // Treasury signer is reserved for sweep/refund management (used by the scheduler).
    // Liquidation execution in live mode uses worker signers from the registry
    // (the orchestrator signs locally). The provider itself stays read-only.
    let legacy_signer = load_signer(&pacing_cfg.execute_mode).unwrap_or(None);
    let signer_address: Option<Address> = legacy_signer.as_ref().map(|s| s.address())
        .or_else(|| signer_registry.worker_addresses().first().copied());

    // Clone signer_registry and pacing_cfg for the sweep scheduler.
    let sched_registry = signer_registry.clone();
    let sched_pacing_cfg = pacing_cfg.clone();

    // Always use read-only provider. Worker signers in the orchestrator
    // sign liquidation transactions locally; the scheduler signs sweep/refund
    // transactions via ManagedSigner. The treasury signer is not used as
    // a provider wallet.
    let provider = Arc::new(ProviderBuilder::new().connect_http(rpc_url));
    run_with_provider(provider, pacing_cfg, routing_cfg, risk_cfg, metrics, snapshot, snapshot_path, signer_address, sched_registry, sched_pacing_cfg, &cli).await
}

/// Construct every runtime component for a concrete provider type and run the loop.
///
/// Monomorphized once per concrete `P`, so the wallet-backed and read-only providers
/// (which have different concrete types) both flow through here without trait objects.
async fn run_with_provider<P>(
    provider: Arc<P>,
    pacing_cfg: PacingConfig,
    routing_cfg: RoutingConfig,
    risk_cfg: RiskConfig,
    metrics: Arc<Metrics>,
    snapshot: MarketSnapshot,
    snapshot_path: PathBuf,
    signer_address: Option<Address>,
    sched_registry: Arc<SignerRegistry>,
    sched_pacing_cfg: PacingConfig,
    cli: &CliArgs,
) -> anyhow::Result<()>
where
    P: Provider<Ethereum> + Clone + Send + Sync + 'static,
{
    let chain_id = pacing_cfg.chain_id;
    let execute_mode = pacing_cfg.execute_mode.clone();
    let addrs = resolve_aave_addresses(chain_id);
    let staleness = Duration::from_secs(pacing_cfg.oracle_staleness_seconds);

    // Oracles. Aave is the simulator's primary USD price source; Chainlink is wired
    // with the configured ETH/USD feed so gas-cost pacing can refresh live prices.
    let aave_oracle = AaveOracle::new((*provider).clone(), addrs.oracle, staleness);
    let oracle: Arc<dyn PriceOracle> = Arc::new(aave_oracle);
    let feed_addr: Address = pacing_cfg
        .eth_usd_feed_address
        .parse()
        .unwrap_or(Address::ZERO);
    let mut feeds: HashMap<Address, Address> = HashMap::new();
    if feed_addr != Address::ZERO {
        feeds.insert(feed_addr, feed_addr);
    }
    let feeds_configured = feeds.len();
    let chainlink = ChainlinkOracle::new((*provider).clone(), feeds, staleness);
    let chainlink_arc: Arc<dyn PriceOracle> = Arc::new(chainlink);
    info!(
        target = "chimera::oracle",
        feeds_configured,
        eth_usd_feed = %feed_addr,
        "Oracles wired (Aave for simulation, Chainlink for pacing)"
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
        oracle,
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

    // Pacing engine with JSONL outcome persistence and live ETH/USD oracle.
    let persistence = Arc::new(JsonlPersistence::new(
        PathBuf::from("core/state/outcomes.jsonl"),
        50,
        5,
    ));
    let pacing_engine = PacingEngine::new(pacing_cfg.clone())
        .with_state_persistence(persistence)
        .with_eth_oracle(chainlink_arc);

    // If live and the EOA pool has entries that don't match the configured signer,
    // warn that multi-worker EOA rotation is deferred to T7. The orchestrator will
    // pin all live submissions to the signer address.
    if execute_mode == "live" {
        if let Some(ref sa) = signer_address {
            if let Some(pool_eoa) = pacing_engine.select_next_eoa() {
                if pool_eoa.parse::<Address>().map_or(true, |a| a != *sa) {
                    warn!(
                        target = "chimera::main",
                        signer = %sa,
                        pool_eoa = %pool_eoa,
                        "Live mode: EOA pool entry differs from configured signer. \
                         All live submissions will use the signer address. \
                         Multi-worker EOA rotation (T7) is deferred."
                    );
                }
            }
        }
    }

    // Wrap in CrossProcessPacing so the orchestrator goes through pre-fire
    // reservation and post-outcome settlement/expiry under advisory file locks.
    let pacing_engine_clone = pacing_engine.clone();
    let pacing = CrossProcessPacing::new(
        pacing_engine,
        PathBuf::from("core/state/reservations"),
        PathBuf::from("core/state/outcomes.jsonl"),
    );

    // Build MempoolWatcher (default: BlockWatch via WS).
    //
    // Resolution order:
    //   1. CLI --ws-url flag (explicit operator override)
    //   2. CHIMERA_WS_ENDPOINT env var (deployment-level config)
    //   3. pacing.yaml ws_endpoint field (per-chain persistent config)
    //   4. Derive from chain_id for known L2 chains (deterministic default)
    //      - Base (8453):      wss://mainnet.base.org
    //      - Arbitrum (42161): wss://arb1.arbitrum.io/ws
    //      Without a match, this derivation is a no-op.
    //   5. If no endpoint is resolved, fall back to fixed-interval polling.
    //
    // When a valid ws_endpoint exists, BlockWatch is constructed by default
    // and the orchestrator uses block-driven scans. Fixed-interval polling
    // is reserved for when watcher initialization is unavailable.
    let resolved_ws = cli.ws_url.clone()
        .or_else(|| {
            if pacing_cfg.ws_endpoint.is_empty() {
                None
            } else {
                Some(pacing_cfg.ws_endpoint.clone())
            }
        })
        .or_else(|| {
            derive_ws_endpoint_from_chain(pacing_cfg.chain_id)
        });

    let mempool_watcher: Option<Arc<dyn MempoolWatcher>> = match resolved_ws {
        Some(ws_url) => {
            match BlockWatch::new(chain_id, ws_url.clone()).await {
                Ok(bw) => {
                    info!(
                        target = "chimera::main",
                        chain_id,
                        ws_host = %redact_ws_url_log(&ws_url),
                        "BlockWatch constructed — using block-driven scan trigger"
                    );
                    let sequencer = SequencerFeed::from_block_watch(bw, None);
                    Some(Arc::new(sequencer))
                }
                Err(e) => {
                    warn!(
                        target = "chimera::main",
                        chain_id,
                        error = %e,
                        "Failed to construct BlockWatch; falling back to fixed-interval polling"
                    );
                    None
                }
            }
        }
        None => {
            info!(
                target = "chimera::main",
                chain_id,
                "No WebSocket endpoint configured; using fixed-interval polling"
            );
            None
        }
    };

    let orchestrator = Orchestrator::new(
        OrchestratorConfig::default(),
        pacing,
        pacing_cfg,
        metrics,
        provider.clone(),
        snapshot,
        chain_id,
        Some(simulator),
        submitter,
        execute_mode,
        addrs.pool,
        routing_cfg,
        risk_cfg,
        sched_registry.clone(),
        signer_address,
        mempool_watcher,
    );

    // Spawn sweep/refund scheduler task (must start before orchestrator.run()).
    if !sched_registry.is_empty() || sched_pacing_cfg.execute_mode == "shadow" {
        let scheduler = SweepScheduler::new(
            sched_registry.clone(),
            provider.clone(),
            Arc::new(pacing_engine_clone),
            sched_pacing_cfg.clone(),
            chain_id,
        );
        tokio::spawn(async move {
            scheduler.run().await;
        });
        info!("Sweep/refund scheduler spawned");
    }

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

/// Derive a WebSocket endpoint from the chain ID for known L2 chains.
///
/// Returns `None` when no deterministic derivation is available (the operator
/// must provide an explicit WS URL via `--ws-url`, `CHIMERA_WS_ENDPOINT`, or
/// the `ws_endpoint` config field).
fn derive_ws_endpoint_from_chain(chain_id: u64) -> Option<String> {
    match chain_id {
        8453 => {
            Some("wss://mainnet.base.org".to_string())
        }
        42161 => {
            Some("wss://arb1.arbitrum.io/ws".to_string())
        }
        _ => None,
    }
}

/// Simple WS URL redaction for logging (strips API keys/paths).
fn redact_ws_url_log(url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(url) {
        let scheme = parsed.scheme();
        if let Some(host) = parsed.host_str() {
            let port_str = parsed.port().map(|p| format!(":{}", p)).unwrap_or_default();
            return format!("{}://{}{}/[redacted]", scheme, host, port_str);
        }
    }
    if let Some(idx) = url.find("://") {
        let after_scheme = &url[idx + 3..];
        if let Some(slash) = after_scheme.find('/') {
            return format!("{}/[redacted]", &url[..idx + 3 + slash]);
        }
        return url[..idx + 3 + after_scheme.len()].to_string();
    }
    "(invalid-url)".to_string()
}
