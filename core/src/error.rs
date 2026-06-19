//! Domain errors for Chimera Core.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ChimeraError {
    #[error("Pacing violation: {0}")]
    PacingViolation(String),

    #[error("Circuit breaker tripped: {reason:?}")]
    BreakerTripped {
        reason: crate::pacing_engine::BreakerReason,
    },

    #[error("Simulation failed: {0}")]
    SimulationFailed(String),

    #[error("Forensic block: {0}")]
    ForensicBlock(String),

    #[error("Insufficient profit after all costs: {0}")]
    InsufficientProfit(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Simulation timeout: {0}ms exceeded")]
    SimulationTimeout(u64),

    #[error("Conversion error: {0}")]
    ConversionError(String),

    #[error("Persistence error: {0}")]
    PersistenceError(String),

    #[error("Fee query error: {0}")]
    FeeQueryError(String),

    #[error("Oracle error: {0}")]
    OracleError(String),

    #[error("RPC error: {0}")]
    RpcError(String),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Timeout error: {0}")]
    TimeoutError(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ChimeraError {
    /// Returns true if the error is transient and the operation may be retried.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ChimeraError::RpcError(_)
                | ChimeraError::TimeoutError(_)
                | ChimeraError::FeeQueryError(_)
        )
    }
}
