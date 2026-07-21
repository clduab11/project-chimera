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
//   8. Build a read-only provider; live mode validates Executor state and signer funding.
//   9. Start the simulator, scheduler, and orchestrator.
//
// Safety: defaults to "shadow"; never flips to "live"; never logs secrets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::network::Ethereum;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use chimera_core::{
    start_metrics_server, AaveOracle, BlockWatch, ChainlinkOracle, CrossProcessPacing,
    JsonlPersistence, L2ChainType, LiquidationSimulator, MarketSnapshot, MempoolWatcher, Metrics,
    Orchestrator, OrchestratorConfig, PacingConfig, PacingEngine, PriceOracle, RiskConfig,
    RoutingConfig, RpcSubmitter, SequencerFeed, SignerRegistry, SnapshotRefresher, SweepScheduler,
    MIN_GAS_BUDGET_WEI,
};

alloy::sol! {
    #[sol(rpc)]
    interface IExecutorStartup {
        function pool() external view returns (address);
        function isWorker(address worker) external view returns (bool);
    }
}

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
#[derive(Debug)]
struct AaveAddresses {
    pool: Address,
    oracle: Address,
    /// Populated at startup; read by address-parity tests and reserved for
    /// prewarm/diagnostics — not yet consumed on the hot path.
    #[allow(dead_code)]
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
    let port_offset = cli
        .metrics_port_offset
        .unwrap_or((pacing_cfg.chain_id % 1000) as u16);
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

    // CLI overrides happen after config loading; revalidate the resulting runtime config.
    pacing_cfg.validate()?;

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

    // (c) SignerRegistry loads treasury + worker keystores from pacing.yaml paths.
    let signer_registry = Arc::new(SignerRegistry::load(&pacing_cfg, &pacing_cfg.execute_mode)?);

    // Always use read-only provider. Worker signers in the orchestrator
    // sign liquidation transactions locally; the scheduler signs sweep/refund
    // transactions via ManagedSigner. The treasury signer is not used as
    // a provider wallet.
    let provider = Arc::new(ProviderBuilder::new().connect_http(rpc_url));
    run_with_provider(
        provider,
        pacing_cfg,
        routing_cfg,
        risk_cfg,
        metrics,
        snapshot,
        snapshot_path,
        signer_registry,
        &cli,
    )
    .await
}

/// Construct every runtime component for a concrete provider type and run the loop.
///
/// Monomorphized once per concrete `P`, so the wallet-backed and read-only providers
/// (which have different concrete types) both flow through here without trait objects.
#[allow(clippy::too_many_arguments)] // bootstrap wiring; each arg is an independent runtime component
async fn run_with_provider<P>(
    provider: Arc<P>,
    pacing_cfg: PacingConfig,
    routing_cfg: RoutingConfig,
    risk_cfg: RiskConfig,
    metrics: Arc<Metrics>,
    snapshot: MarketSnapshot,
    snapshot_path: PathBuf,
    signer_registry: Arc<SignerRegistry>,
    cli: &CliArgs,
) -> anyhow::Result<()>
where
    P: Provider<Ethereum> + Clone + Send + Sync + 'static,
{
    let chain_id = pacing_cfg.chain_id;
    let execute_mode = pacing_cfg.execute_mode.clone();
    let addrs = resolve_aave_addresses(chain_id)?;
    let executor = resolve_executor_address(&pacing_cfg)?;
    let active_workers = signer_registry.worker_addresses();
    validate_live_executor(
        &*provider,
        &execute_mode,
        executor,
        addrs.pool,
        &active_workers,
    )
    .await?;
    validate_live_funding(&*provider, &pacing_cfg, &signer_registry).await?;
    let staleness = Duration::from_secs(pacing_cfg.oracle_staleness_seconds);

    // Oracles. Aave is the simulator's primary USD price source; Chainlink is wired
    // with the configured ETH/USD feed so gas-cost pacing can refresh live prices.
    let aave_oracle = AaveOracle::new((*provider).clone(), addrs.oracle, staleness);
    // Second handle to the same oracle: the live snapshot repricer batches
    // getAssetsPrices through it (raw 8-dec U256, one eth_call per refresh).
    let reprice_source = aave_oracle.clone();
    let oracle: Arc<dyn PriceOracle> = Arc::new(aave_oracle);
    let feed_addr: Address = pacing_cfg.eth_usd_feed_address.parse().map_err(|e| {
        anyhow::anyhow!(
            "invalid eth_usd_feed_address {:?}: {e}",
            pacing_cfg.eth_usd_feed_address
        )
    })?;
    if feed_addr == Address::ZERO {
        anyhow::bail!("eth_usd_feed_address must not be the zero address");
    }
    let mut feeds: HashMap<Address, Address> = HashMap::new();
    feeds.insert(feed_addr, feed_addr);
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
    let mut simulator = LiquidationSimulator::new(provider.clone(), addrs.pool, oracle, addrs.weth)
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
    let resolved_ws = cli
        .ws_url
        .clone()
        .or_else(|| {
            if pacing_cfg.ws_endpoint.is_empty() {
                None
            } else {
                Some(pacing_cfg.ws_endpoint.clone())
            }
        })
        .or_else(|| derive_ws_endpoint_from_chain(pacing_cfg.chain_id));

    let mempool_watcher: Option<Arc<dyn MempoolWatcher>> = match resolved_ws {
        Some(ws_url) => match BlockWatch::new(chain_id, ws_url.clone()).await {
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
        },
        None => {
            info!(
                target = "chimera::main",
                chain_id, "No WebSocket endpoint configured; using fixed-interval polling"
            );
            None
        }
    };

    let price_refresh_secs = risk_cfg.price_refresh_secs;
    let orchestrator = Orchestrator::new(
        OrchestratorConfig::default(),
        pacing,
        pacing_cfg.clone(),
        metrics,
        provider.clone(),
        snapshot,
        chain_id,
        Some(simulator),
        submitter,
        execute_mode,
        executor,
        routing_cfg,
        risk_cfg,
        signer_registry.clone(),
        mempool_watcher,
    );

    // Live detection refresh: paced oracle repricing + discovery reload of the
    // snapshot file, ticked inline by the scan loop over the SAME shared
    // snapshot the detector reads. Without this the snapshot stays frozen at
    // boot and a near-miss position can never be observed crossing HF 1.05.
    let refresher = Arc::new(SnapshotRefresher::new(
        orchestrator.shared_snapshot(),
        Arc::new(reprice_source),
        snapshot_path.clone(),
        chain_id,
        price_refresh_secs,
    ));
    let orchestrator = orchestrator.with_snapshot_refresher(refresher);

    // Spawn sweep/refund scheduler task (must start before orchestrator.run()).
    if !signer_registry.is_empty() || pacing_cfg.execute_mode == "shadow" {
        let scheduler = SweepScheduler::new(
            signer_registry.clone(),
            provider.clone(),
            Arc::new(pacing_engine_clone),
            pacing_cfg.clone(),
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
                Err(_) => warn!(
                    env = key,
                    "RPC URL env var failed to parse; trying next source"
                ),
            }
        }
    }
    let fallback = "http://localhost:8545"
        .parse::<url::Url>()
        .expect("valid default RPC URL");
    (fallback, "default(localhost:8545)".to_string())
}

/// Per-chain Aave V3 addresses (mirrors `config/pools.toml`), each overridable via env.
/// Unknown chains cause a fatal boot error; testnet chains (84532) are explicitly rejected.
/// Every resolved address is parsed and checked for non-zero before returning.
fn resolve_aave_addresses(chain_id: u64) -> anyhow::Result<AaveAddresses> {
    let addr = |default: &str, env_key: &str| -> anyhow::Result<Address> {
        let s = std::env::var(env_key).unwrap_or_else(|_| default.to_string());
        let address = Address::from_str(&s)
            .map_err(|e| anyhow::anyhow!("invalid {env_key} address {s:?}: {e}"))?;
        if address == Address::ZERO {
            anyhow::bail!("{env_key} must not resolve to the zero address");
        }
        Ok(address)
    };
    let addresses = match chain_id {
        8453 => AaveAddresses {
            pool: addr("0xA238Dd80C259a72e81d7e4664a9801593F98d1c5", "CHIMERA_AAVE_POOL")?,
            oracle: addr(
                "0x2Cc0Fc26eD4563A5ce5e8bdcfe1A2878676Ae156",
                "CHIMERA_AAVE_ORACLE",
            )?,
            pool_data_provider: addr(
                "0x0F43731EB8d45A581f4a36DD74F5f358bc90C73A",
                "CHIMERA_POOL_DATA_PROVIDER",
            )?,
            weth: addr(
                "0x4200000000000000000000000000000000000006",
                "CHIMERA_ETH_ORACLE_ASSET",
            )?,
        },
        42161 => AaveAddresses {
            pool: addr("0x794a61358D6845594F94dc1DB02A252b5b4814aD", "CHIMERA_AAVE_POOL")?,
            oracle: addr("0xb56c2F0B653B2e0b10C9b928C8580Ac5Df02C7C7", "CHIMERA_AAVE_ORACLE")?,
            pool_data_provider: addr(
                "0x243Aa95cAC2a25651eda86e80bEe66114413c43b",
                "CHIMERA_POOL_DATA_PROVIDER",
            )?,
            weth: addr(
                "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
                "CHIMERA_ETH_ORACLE_ASSET",
            )?,
        },
        84532 => anyhow::bail!(
            "chain_id 84532 (Base Sepolia) is unsupported by the live model; use a testnet-specific process"
        ),
        _ => anyhow::bail!(
            "unknown chain_id {chain_id}; supported chains are 8453 (Base) and 42161 (Arbitrum)"
        ),
    };
    Ok(addresses)
}

fn resolve_executor_address(pacing_cfg: &PacingConfig) -> anyhow::Result<Address> {
    if pacing_cfg.executor_address.trim().is_empty() {
        if pacing_cfg.execute_mode == "shadow" {
            warn!(
                target = "chimera::main",
                "executor_address is empty in shadow mode; assembled no-op requests will target the zero address until CHIMERA_EXECUTOR_ADDRESS is configured"
            );
            return Ok(Address::ZERO);
        }
        anyhow::bail!("execute_mode=live requires executor_address");
    }

    let executor = pacing_cfg
        .executor_address
        .parse::<Address>()
        .map_err(|e| {
            anyhow::anyhow!(
                "invalid executor_address {:?}: {e}",
                pacing_cfg.executor_address
            )
        })?;
    if executor == Address::ZERO {
        anyhow::bail!("executor_address must not be the zero address");
    }
    Ok(executor)
}

async fn validate_live_executor<P: Provider<Ethereum> + Sync>(
    provider: &P,
    execute_mode: &str,
    executor: Address,
    expected_pool: Address,
    active_workers: &[Address],
) -> anyhow::Result<()> {
    if execute_mode != "live" {
        return Ok(());
    }

    let code = provider.get_code_at(executor).await.map_err(|e| {
        anyhow::anyhow!(
            "failed to query deployed bytecode for live executor {executor} (expected Aave Pool {expected_pool}): {e}"
        )
    })?;
    if code.is_empty() {
        anyhow::bail!(
            "live executor {executor} has no deployed bytecode; verify the executor deployment and chain (expected Aave Pool {expected_pool})"
        );
    }

    let contract = IExecutorStartup::new(executor, provider);
    let configured_pool = contract.pool().call().await.map_err(|e| {
        anyhow::anyhow!(
            "failed to query pool() on live executor {executor} (expected Aave Pool {expected_pool}): {e}"
        )
    })?;
    if configured_pool != expected_pool {
        anyhow::bail!(
            "live executor {executor} pool mismatch: contract returned {configured_pool}, expected Aave Pool {expected_pool}; deploy or configure the executor for this chain"
        );
    }

    for &worker in active_workers {
        let authorized = contract.isWorker(worker).call().await.map_err(|e| {
            anyhow::anyhow!(
                "failed to query isWorker({worker}) on live executor {executor} (expected Aave Pool {expected_pool}): {e}"
            )
        })?;
        if !authorized {
            anyhow::bail!(
                "live executor {executor} does not authorize active worker {worker}; expected Aave Pool {expected_pool}; authorize the worker before startup"
            );
        }
    }

    info!(
        target = "chimera::main",
        executor = %executor,
        aave_pool = %expected_pool,
        active_workers = active_workers.len(),
        "Live executor deployment and authorization validated"
    );
    Ok(())
}

async fn validate_live_funding<P: Provider<Ethereum> + Sync>(
    provider: &P,
    pacing_cfg: &PacingConfig,
    registry: &SignerRegistry,
) -> anyhow::Result<()> {
    if pacing_cfg.execute_mode != "live" {
        return Ok(());
    }

    let treasury = registry
        .treasury_address()
        .ok_or_else(|| anyhow::anyhow!("live startup requires a decrypted treasury signer"))?;
    let treasury_balance = provider
        .get_balance(treasury)
        .await
        .map_err(|e| anyhow::anyhow!("failed to query treasury balance for {treasury}: {e}"))?;

    let required_treasury_wei = decimal_eth_to_wei(pacing_cfg.refund_topup_eth)?;
    let min_gas_budget_eth =
        Decimal::from(MIN_GAS_BUDGET_WEI as u64) / Decimal::from(1_000_000_000_000_000_000u64);
    let effective_worker_min_eth = pacing_cfg.min_worker_balance_eth.max(min_gas_budget_eth);
    let required_worker_wei = decimal_eth_to_wei(effective_worker_min_eth)?;
    let mut failures = Vec::new();
    if let Some(failure) = funding_shortfall(
        &format!("treasury {treasury}"),
        required_treasury_wei,
        treasury_balance,
    ) {
        failures.push(failure);
    }
    for worker in registry.worker_addresses() {
        let balance = provider.get_balance(worker).await.map_err(|e| {
            anyhow::anyhow!("failed to query active worker balance for {worker}: {e}")
        })?;
        if let Some(failure) =
            funding_shortfall(&format!("worker {worker}"), required_worker_wei, balance)
        {
            failures.push(failure);
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "live startup funding validation failed:\n{}",
            failures.join("\n")
        );
    }
    Ok(())
}

fn funding_shortfall(subject: &str, required_wei: U256, current_wei: U256) -> Option<String> {
    (current_wei < required_wei)
        .then(|| format!("{subject}: required {required_wei} wei, current {current_wei} wei"))
}

fn decimal_eth_to_wei(eth: Decimal) -> anyhow::Result<U256> {
    if eth < Decimal::ZERO {
        anyhow::bail!("ETH funding threshold must not be negative: {eth}");
    }
    let scaled = eth * Decimal::from(1_000_000_000_000_000_000u64);
    if scaled.fract() != Decimal::ZERO {
        anyhow::bail!("ETH funding threshold has precision below one wei: {eth} ETH");
    }
    scaled.to_u128().map(U256::from).ok_or_else(|| {
        anyhow::anyhow!("ETH funding threshold is outside the supported wei range: {eth} ETH")
    })
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
        8453 => Some("wss://mainnet.base.org".to_string()),
        42161 => Some("wss://arb1.arbitrum.io/ws".to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Bytes};
    use alloy::providers::ProviderBuilder;
    use alloy::sol_types::{SolCall, SolValue};
    use alloy::transports::mock::Asserter;
    use std::sync::{Mutex, MutexGuard};

    static ADDRESS_ENV_LOCK: Mutex<()> = Mutex::new(());

    const ADDRESS_ENV_KEYS: [&str; 4] = [
        "CHIMERA_AAVE_POOL",
        "CHIMERA_AAVE_ORACLE",
        "CHIMERA_POOL_DATA_PROVIDER",
        "CHIMERA_ETH_ORACLE_ASSET",
    ];

    struct AddressEnvGuard {
        _lock: MutexGuard<'static, ()>,
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl AddressEnvGuard {
        fn new() -> Self {
            let lock = ADDRESS_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = ADDRESS_ENV_KEYS
                .iter()
                .map(|&key| (key, std::env::var(key).ok()))
                .collect();
            for key in ADDRESS_ENV_KEYS {
                std::env::remove_var(key);
            }
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for AddressEnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.previous {
                match value {
                    Some(value) => std::env::set_var(*key, value),
                    None => std::env::remove_var(*key),
                }
            }
        }
    }

    fn without_address_overrides<T>(test: impl FnOnce() -> T) -> T {
        let _guard = AddressEnvGuard::new();
        test()
    }

    #[test]
    fn base_aave_defaults_match_pinned_address_book() {
        without_address_overrides(|| {
            let addresses = resolve_aave_addresses(8453).unwrap();
            assert_eq!(
                addresses.pool,
                "0xA238Dd80C259a72e81d7e4664a9801593F98d1c5"
                    .parse::<Address>()
                    .unwrap()
            );
            assert_eq!(
                addresses.oracle,
                "0x2Cc0Fc26eD4563A5ce5e8bdcfe1A2878676Ae156"
                    .parse::<Address>()
                    .unwrap()
            );
            assert_eq!(
                addresses.pool_data_provider,
                "0x0F43731EB8d45A581f4a36DD74F5f358bc90C73A"
                    .parse::<Address>()
                    .unwrap()
            );
            assert_eq!(
                addresses.weth,
                "0x4200000000000000000000000000000000000006"
                    .parse::<Address>()
                    .unwrap()
            );
        });
    }

    #[test]
    fn every_supported_chain_default_address_is_nonzero() {
        without_address_overrides(|| {
            for chain_id in [8453, 42161] {
                let addresses = resolve_aave_addresses(chain_id).unwrap();
                for (name, address) in [
                    ("pool", addresses.pool),
                    ("oracle", addresses.oracle),
                    ("pool_data_provider", addresses.pool_data_provider),
                    ("weth", addresses.weth),
                ] {
                    assert_ne!(address, Address::ZERO, "{name} was zero for {chain_id}");
                }
            }
        });
    }

    #[test]
    fn malformed_and_zero_address_overrides_are_actionable_errors() {
        let _guard = AddressEnvGuard::new();

        std::env::set_var("CHIMERA_AAVE_POOL", "not-an-address");
        assert!(resolve_aave_addresses(8453)
            .unwrap_err()
            .to_string()
            .contains("CHIMERA_AAVE_POOL"));

        std::env::set_var(
            "CHIMERA_AAVE_POOL",
            "0x0000000000000000000000000000000000000000",
        );
        assert!(resolve_aave_addresses(8453)
            .unwrap_err()
            .to_string()
            .contains("zero address"));
    }

    #[tokio::test]
    async fn live_executor_validation_accepts_deployed_matching_authorized_contract() {
        let executor = address!("0x1111111111111111111111111111111111111111");
        let expected_pool = address!("0x2222222222222222222222222222222222222222");
        let workers = [
            address!("0x3333333333333333333333333333333333333333"),
            address!("0x4444444444444444444444444444444444444444"),
        ];
        let asserter = Asserter::new();
        asserter.push_success(&Bytes::from_static(&[0x60, 0x00]));
        asserter.push_success(&Bytes::from(expected_pool.abi_encode()));
        for _ in &workers {
            asserter.push_success(&Bytes::from(true.abi_encode()));
        }
        let provider = ProviderBuilder::new().connect_mocked_client(asserter.clone());

        validate_live_executor(&provider, "live", executor, expected_pool, &workers)
            .await
            .expect("matching deployed executor must pass live startup validation");
        assert!(asserter.read_q().is_empty());
    }

    #[tokio::test]
    async fn shadow_executor_validation_makes_no_rpc_calls() {
        let asserter = Asserter::new();
        let provider = ProviderBuilder::new().connect_mocked_client(asserter.clone());
        let configured_executor = address!("0x1111111111111111111111111111111111111111");
        let expected_pool = address!("0x2222222222222222222222222222222222222222");
        let worker = address!("0x3333333333333333333333333333333333333333");

        validate_live_executor(
            &provider,
            "shadow",
            configured_executor,
            expected_pool,
            &[worker],
        )
        .await
        .expect("shadow startup must skip executor RPC validation");
        assert!(asserter.read_q().is_empty());
    }

    #[test]
    fn shadow_executor_resolution_only_allows_empty_as_zero() {
        let empty = PacingConfig::default();
        assert_eq!(resolve_executor_address(&empty).unwrap(), Address::ZERO);

        let configured_executor = address!("0x1111111111111111111111111111111111111111");
        let configured = PacingConfig {
            executor_address: configured_executor.to_string(),
            ..PacingConfig::default()
        };
        assert_eq!(
            resolve_executor_address(&configured).unwrap(),
            configured_executor
        );

        let invalid = PacingConfig {
            executor_address: "not-an-address".into(),
            ..PacingConfig::default()
        };
        let invalid_error = resolve_executor_address(&invalid).unwrap_err().to_string();
        assert!(invalid_error.contains("invalid executor_address"));
        assert!(invalid_error.contains("not-an-address"));

        let zero = PacingConfig {
            executor_address: format!("{:#x}", Address::ZERO),
            ..PacingConfig::default()
        };
        assert!(resolve_executor_address(&zero)
            .unwrap_err()
            .to_string()
            .contains("must not be the zero address"));
    }

    #[test]
    fn executor_startup_calls_use_expected_abi_selectors() {
        let worker = address!("0x3333333333333333333333333333333333333333");
        let pool_call = IExecutorStartup::poolCall {}.abi_encode();
        assert_eq!(
            &pool_call[..4],
            &alloy::primitives::keccak256("pool()")[..4]
        );
        assert_eq!(pool_call.len(), 4);

        let worker_call = IExecutorStartup::isWorkerCall { worker }.abi_encode();
        assert_eq!(
            &worker_call[..4],
            &alloy::primitives::keccak256("isWorker(address)")[..4]
        );
        assert_eq!(worker_call.len(), 4 + 32);
        assert_eq!(Address::from_slice(&worker_call[16..36]), worker);
    }

    #[tokio::test]
    async fn live_executor_validation_rejects_missing_bytecode_actionably() {
        let executor = address!("0x1111111111111111111111111111111111111111");
        let expected_pool = address!("0x2222222222222222222222222222222222222222");
        let asserter = Asserter::new();
        asserter.push_success(&Bytes::new());
        let provider = ProviderBuilder::new().connect_mocked_client(asserter);

        let error = validate_live_executor(&provider, "live", executor, expected_pool, &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(&executor.to_string()));
        assert!(error.contains(&expected_pool.to_string()));
        assert!(error.contains("no deployed bytecode"));
    }

    #[tokio::test]
    async fn live_executor_validation_rejects_pool_mismatch_actionably() {
        let executor = address!("0x1111111111111111111111111111111111111111");
        let expected_pool = address!("0x2222222222222222222222222222222222222222");
        let configured_pool = address!("0x5555555555555555555555555555555555555555");
        let asserter = Asserter::new();
        asserter.push_success(&Bytes::from_static(&[0x60, 0x00]));
        asserter.push_success(&Bytes::from(configured_pool.abi_encode()));
        let provider = ProviderBuilder::new().connect_mocked_client(asserter);

        let error = validate_live_executor(&provider, "live", executor, expected_pool, &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(&executor.to_string()));
        assert!(error.contains(&configured_pool.to_string()));
        assert!(error.contains(&expected_pool.to_string()));
        assert!(error.contains("pool mismatch"));
    }

    #[tokio::test]
    async fn live_executor_validation_rejects_unauthorized_worker_actionably() {
        let executor = address!("0x1111111111111111111111111111111111111111");
        let expected_pool = address!("0x2222222222222222222222222222222222222222");
        let worker = address!("0x3333333333333333333333333333333333333333");
        let asserter = Asserter::new();
        asserter.push_success(&Bytes::from_static(&[0x60, 0x00]));
        asserter.push_success(&Bytes::from(expected_pool.abi_encode()));
        asserter.push_success(&Bytes::from(false.abi_encode()));
        let provider = ProviderBuilder::new().connect_mocked_client(asserter);

        let error = validate_live_executor(&provider, "live", executor, expected_pool, &[worker])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(&executor.to_string()));
        assert!(error.contains(&worker.to_string()));
        assert!(error.contains(&expected_pool.to_string()));
        assert!(error.contains("does not authorize active worker"));
    }

    #[test]
    fn decimal_eth_conversion_is_exact_and_rejects_subwei_precision() {
        assert_eq!(
            decimal_eth_to_wei(Decimal::from_str("0.01").unwrap()).unwrap(),
            U256::from(10_000_000_000_000_000u64)
        );
        assert_eq!(
            decimal_eth_to_wei(Decimal::from_str("0.05").unwrap()).unwrap(),
            U256::from(50_000_000_000_000_000u64)
        );
        assert!(decimal_eth_to_wei(Decimal::from_str("0.0000000000000000001").unwrap()).is_err());
    }

    #[test]
    fn funding_shortfall_reports_required_and_current_wei() {
        let required = U256::from(50_000_000_000_000_000u64);
        let current = U256::from(49_999_999_999_999_999u64);

        assert_eq!(
            funding_shortfall("treasury 0x1234", required, current).as_deref(),
            Some("treasury 0x1234: required 50000000000000000 wei, current 49999999999999999 wei")
        );
        assert_eq!(
            funding_shortfall("treasury 0x1234", required, required),
            None
        );
    }
}
