//! Chimera Core - Sovereign L2 MEV Engine
//! Local-first, flash-loan atomic execution with strict pacing & risk controls.

pub mod config;
pub mod detector;
pub mod error;
pub mod executor;
pub mod mempool;
pub mod metrics;
pub mod oracle;
pub mod orchestrator;
pub mod pacing_engine;
pub mod routing;
pub mod signer_registry;
pub mod simulator;
pub mod state;
pub mod strategy;
pub mod sweep_scheduler;

pub use simulator::golden;
pub use simulator::prewarm;

pub use config::{PacingConfig, RiskConfig, RoutingConfig, VenueEntry};
pub use detector::liquidation::{LiquidationDetector, MarketSnapshot};
pub use error::ChimeraError;
pub use executor::{
    balance::{check_eoa_gas_sufficient, get_eoa_balance, MIN_GAS_BUDGET_WEI},
    BuiltTransaction, CalldataBuilder, RpcSubmitter, SubmissionReceipt, TransactionExecutor,
};
pub use mempool::{BlockWatch, MempoolWatcher, SequencerFeed, WatchEvent};
pub use metrics::{start_metrics_server, Metrics};
pub use oracle::{AaveOracle, ChainlinkOracle, OraclePrice, PriceOracle};
pub use orchestrator::{Orchestrator, OrchestratorConfig};
pub use pacing_engine::{
    BreakerReason, CrossProcessPacing, Opportunity, PacingDecision, PacingEngine,
};
pub use routing::{ResolvedV2Route, RoutingResolver};
pub use signer_registry::{ManagedSigner, SignerRegistry};
pub use simulator::{L2ChainType, LiquidationCandidate, LiquidationSimulator, SimulationResult};
pub use state::{JsonlPersistence, ReservationRecord, ReservationStatus, StatePersistence};
pub use strategy::assembler::StrategyAssembler;
pub use sweep_scheduler::SweepScheduler;
