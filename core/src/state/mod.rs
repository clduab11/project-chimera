//! Persistent state management with JSONL audit trails and crash recovery.
//!
//! Provides the [`StatePersistence`] trait and implementations for atomic,
//! rotation-aware JSONL storage with crash-tolerant recovery.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::ChimeraError;

mod persistence;
mod recovery;

pub use persistence::JsonlPersistence;
pub use recovery::CrashRecovery;

/// Trait for persisting and recovering outcome records from an audit trail.
#[async_trait]
pub trait StatePersistence: Send + Sync {
    /// Append a single outcome record to the audit trail.
    async fn append_outcome(&self, outcome: OutcomeRecord) -> Result<(), ChimeraError>;

    /// Load the most recent `limit` outcome records.
    async fn load_recent(&self, limit: usize) -> Result<Vec<OutcomeRecord>, ChimeraError>;

    /// Recover aggregated state from the audit trail.
    async fn recover_state(&self) -> Result<RecoveredState, ChimeraError>;
}

/// A single outcome record in the JSONL audit trail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutcomeRecord {
    /// Unique identifier for this outcome.
    pub id: String,
    /// Timestamp when the outcome was recorded.
    pub timestamp: DateTime<Utc>,
    /// Decision that led to this outcome (e.g. "Allow", "Deny").
    pub decision: String,
    /// Realized net profit/loss in USD.
    pub realized_net_usd: Decimal,
    /// Gas spent in ETH.
    pub gas_spent_eth: Decimal,
    /// Whether the transaction reverted.
    pub reverted: bool,
    /// Venue where the transaction was submitted.
    pub venue: String,
    /// EOA (externally owned account) used for the transaction.
    pub eoa: String,
    /// Chain ID where the transaction was executed.
    pub chain_id: u64,
}

/// Aggregated state recovered from the JSONL audit trail.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredState {
    /// Sum of `realized_net_usd` for records within the last 24 hours.
    pub daily_usage_usd: Decimal,
    /// Sum of `realized_net_usd` for records within the last 7 days.
    pub weekly_usage_usd: Decimal,
    /// Number of consecutive trailing reverts.
    pub consecutive_reverts: u32,
    /// Timestamp of the most recent outcome record.
    pub last_outcome_time: Option<DateTime<Utc>>,
}

/// A cross-process reservation record for global cap enforcement.
///
/// Written to a shared JSONL file under advisory file lock so that independent
/// per-chain processes cannot jointly exceed daily/weekly caps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReservationRecord {
    /// Unique identifier for this reservation (typically the opportunity id).
    pub id: String,
    /// Chain ID this reservation applies to.
    pub chain_id: u64,
    /// The reserved net USD amount.
    pub amount_usd: Decimal,
    /// Current lifecycle status.
    pub status: ReservationStatus,
    /// When the reservation was created.
    pub created_at: DateTime<Utc>,
    /// When the reservation expires (TTL).
    pub expires_at: DateTime<Utc>,
    /// When the reservation was settled (only set on Settled).
    #[serde(default)]
    pub settled_at: Option<DateTime<Utc>>,
}

/// Lifecycle status of a reservation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReservationStatus {
    /// Reservation is active and counting against global caps.
    Reserved,
    /// Outcome has been recorded; reservation is finalized.
    Settled,
    /// Reservation TTL expired without settlement.
    Expired,
}
