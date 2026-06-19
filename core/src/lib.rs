//! Chimera Core - Sovereign L2 MEV Engine
//! Local-first, flash-loan atomic execution with strict pacing & risk controls.

pub mod config;
pub mod detector;
pub mod error;
pub mod executor;
pub mod metrics;
pub mod oracle;
pub mod orchestrator;
pub mod pacing_engine;
pub mod simulator;
pub mod state;

pub use simulator::golden;
pub use simulator::prewarm;

pub use config::{PacingConfig, RiskConfig, RoutingConfig, VenueEntry};
pub use detector::liquidation::{LiquidationDetector, MarketSnapshot};
pub use error::ChimeraError;
pub use executor::{
    balance::{check_eoa_gas_sufficient, get_eoa_balance, MIN_GAS_BUDGET_WEI},
    BuiltTransaction, CalldataBuilder, RpcSubmitter, SubmissionReceipt, TransactionExecutor,
};
pub use metrics::{start_metrics_server, Metrics};
pub use orchestrator::{Orchestrator, OrchestratorConfig};
pub use oracle::{AaveOracle, ChainlinkOracle, OraclePrice, PriceOracle};
pub use pacing_engine::{BreakerReason, Opportunity, PacingDecision, PacingEngine};
pub use simulator::{LiquidationCandidate, LiquidationSimulator, SimulationResult};

