//! Crash recovery logic for JSONL audit trails.

use chrono::{DateTime, TimeDelta, Utc};
use rust_decimal::Decimal;
use std::io::{BufRead, BufReader as SyncBufReader};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::warn;

use crate::state::{OutcomeRecord, RecoveredState};
use crate::ChimeraError;

/// Crash recovery helper that rebuilds aggregated state from JSONL audit trails.
#[derive(Debug, Clone, Default)]
pub struct CrashRecovery;

impl CrashRecovery {
    /// Create a new `CrashRecovery`.
    pub fn new() -> Self {
        Self
    }

    /// Rebuild state from a JSONL file.
    ///
    /// Returns a zeroed [`RecoveredState`] if the file is missing.
    /// If a corrupted line is encountered, logs a warning and returns
    /// a zeroed state so that startup never crashes due to audit-trail corruption.
    pub async fn recover_from_jsonl(path: &Path) -> Result<RecoveredState, ChimeraError> {
        if !path.exists() {
            return Ok(RecoveredState {
                daily_usage_usd: Decimal::ZERO,
                weekly_usage_usd: Decimal::ZERO,
                consecutive_reverts: 0,
                last_outcome_time: None,
            });
        }

        let file = File::open(path).await.map_err(ChimeraError::Io)?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        let mut records = Vec::new();

        while let Some(line) = lines.next_line().await.map_err(ChimeraError::Io)? {
            match serde_json::from_str::<OutcomeRecord>(&line) {
                Ok(record) => records.push(record),
                Err(e) => {
                    warn!(
                        "Corrupted JSONL line in {}: {}. Returning zeroed state.",
                        path.display(),
                        e
                    );
                    return Ok(RecoveredState {
                        daily_usage_usd: Decimal::ZERO,
                        weekly_usage_usd: Decimal::ZERO,
                        consecutive_reverts: 0,
                        last_outcome_time: None,
                    });
                }
            }
        }

        let now = Utc::now();
        let mut daily_usage = Decimal::ZERO;
        let mut weekly_usage = Decimal::ZERO;
        let mut consecutive_reverts: u32 = 0;
        let mut last_time: Option<DateTime<Utc>> = None;

        // Compute daily and weekly aggregates
        for record in &records {
            let age = now - record.timestamp;
            if age <= TimeDelta::days(1) {
                daily_usage += record.realized_net_usd;
            }
            if age <= TimeDelta::days(7) {
                weekly_usage += record.realized_net_usd;
            }
        }

        // Count trailing consecutive reverts
        for record in records.iter().rev() {
            if record.reverted {
                consecutive_reverts += 1;
            } else {
                break;
            }
        }

        if let Some(last) = records.last() {
            last_time = Some(last.timestamp);
        }

        Ok(RecoveredState {
            daily_usage_usd: daily_usage,
            weekly_usage_usd: weekly_usage,
            consecutive_reverts,
            last_outcome_time: last_time,
        })
    }

    /// Synchronous variant of [`Self::recover_from_jsonl`] that uses blocking I/O.
    ///
    /// Callers must ensure they are NOT inside a Tokio runtime, or this will
    /// block the async worker. Preferred for call-sites that are already
    /// synchronous (e.g. inside a `std::sync::Mutex` or file-lock critical
    /// section) or for boot-time recovery before the async loop starts.
    pub fn recover_from_jsonl_sync(path: &Path) -> Result<RecoveredState, ChimeraError> {
        if !path.exists() {
            return Ok(RecoveredState {
                daily_usage_usd: Decimal::ZERO,
                weekly_usage_usd: Decimal::ZERO,
                consecutive_reverts: 0,
                last_outcome_time: None,
            });
        }

        let file = std::fs::File::open(path).map_err(ChimeraError::Io)?;
        let reader = SyncBufReader::new(file);

        let mut records = Vec::new();

        for line in reader.lines() {
            let line = line.map_err(ChimeraError::Io)?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<OutcomeRecord>(&line) {
                Ok(record) => records.push(record),
                Err(e) => {
                    warn!(
                        "Corrupted JSONL line in {}: {}. Returning zeroed state.",
                        path.display(),
                        e
                    );
                    return Ok(RecoveredState {
                        daily_usage_usd: Decimal::ZERO,
                        weekly_usage_usd: Decimal::ZERO,
                        consecutive_reverts: 0,
                        last_outcome_time: None,
                    });
                }
            }
        }

        let now = Utc::now();
        let mut daily_usage = Decimal::ZERO;
        let mut weekly_usage = Decimal::ZERO;
        let mut consecutive_reverts: u32 = 0;
        let mut last_time: Option<DateTime<Utc>> = None;

        for record in &records {
            let age = now - record.timestamp;
            if age <= TimeDelta::days(1) {
                daily_usage += record.realized_net_usd;
            }
            if age <= TimeDelta::days(7) {
                weekly_usage += record.realized_net_usd;
            }
        }

        for record in records.iter().rev() {
            if record.reverted {
                consecutive_reverts += 1;
            } else {
                break;
            }
        }

        if let Some(last) = records.last() {
            last_time = Some(last.timestamp);
        }

        Ok(RecoveredState {
            daily_usage_usd: daily_usage,
            weekly_usage_usd: weekly_usage,
            consecutive_reverts,
            last_outcome_time: last_time,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use tokio::fs;

    fn make_record(ts: DateTime<Utc>, net: f64, reverted: bool) -> OutcomeRecord {
        OutcomeRecord {
            id: "test".to_string(),
            timestamp: ts,
            decision: "Allow".to_string(),
            realized_net_usd: Decimal::from_f64_retain(net).unwrap_or(Decimal::ZERO),
            gas_spent_eth: Decimal::from_f64_retain(0.001).unwrap_or(Decimal::ZERO),
            reverted,
            venue: "aerodrome".to_string(),
            eoa: "0x1".to_string(),
            chain_id: 8453,
        }
    }

    #[tokio::test]
    async fn test_recover_from_empty_file() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let state = CrashRecovery::recover_from_jsonl(temp.path())
            .await
            .unwrap();
        assert_eq!(state.daily_usage_usd, Decimal::ZERO);
        assert_eq!(state.weekly_usage_usd, Decimal::ZERO);
        assert_eq!(state.consecutive_reverts, 0);
        assert!(state.last_outcome_time.is_none());
    }

    #[tokio::test]
    async fn test_recover_computes_aggregates() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let now = Utc::now();

        // Chronological order: r3 (oldest), r2, r1 (newest)
        let r3 = make_record(now - TimeDelta::days(2), 25.0, true);
        let r2 = make_record(now - TimeDelta::hours(2), 50.0, false);
        let r1 = make_record(now - TimeDelta::hours(1), 100.0, true);

        let lines = [
            serde_json::to_string(&r3).unwrap(),
            serde_json::to_string(&r2).unwrap(),
            serde_json::to_string(&r1).unwrap(),
        ]
        .join("\n");

        fs::write(temp.path(), lines).await.unwrap();

        let state = CrashRecovery::recover_from_jsonl(temp.path())
            .await
            .unwrap();

        // Daily: r1 + r2 = 150. Weekly: r1 + r2 + r3 = 175.
        assert_eq!(
            state.daily_usage_usd,
            Decimal::from_f64_retain(150.0).unwrap()
        );
        assert_eq!(
            state.weekly_usage_usd,
            Decimal::from_f64_retain(175.0).unwrap()
        );
        // Trailing reverts: r1 is revert, r2 is not -> 1
        assert_eq!(state.consecutive_reverts, 1);
        assert_eq!(state.last_outcome_time, Some(r1.timestamp));
    }

    #[tokio::test]
    async fn test_recover_corrupted_file() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "not json\n").await.unwrap();

        let state = CrashRecovery::recover_from_jsonl(temp.path())
            .await
            .unwrap();
        assert_eq!(state.daily_usage_usd, Decimal::ZERO);
        assert_eq!(state.weekly_usage_usd, Decimal::ZERO);
        assert_eq!(state.consecutive_reverts, 0);
        assert!(state.last_outcome_time.is_none());
    }

    #[tokio::test]
    async fn test_recover_missing_file() {
        let path = std::env::temp_dir().join(format!(
            "chimera_test_nonexistent_{}.jsonl",
            std::process::id()
        ));
        assert!(!path.exists());
        let state = CrashRecovery::recover_from_jsonl(&path).await.unwrap();
        assert_eq!(state.daily_usage_usd, Decimal::ZERO);
    }

    #[tokio::test]
    async fn test_recover_trailing_reverts() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let now = Utc::now();

        let r1 = make_record(now - TimeDelta::hours(1), 10.0, true);
        let r2 = make_record(now - TimeDelta::hours(2), 20.0, true);
        let r3 = make_record(now - TimeDelta::hours(3), 30.0, true);
        let r4 = make_record(now - TimeDelta::hours(4), 40.0, false);

        let lines = [
            serde_json::to_string(&r4).unwrap(),
            serde_json::to_string(&r3).unwrap(),
            serde_json::to_string(&r2).unwrap(),
            serde_json::to_string(&r1).unwrap(),
        ]
        .join("\n");

        fs::write(temp.path(), lines).await.unwrap();

        let state = CrashRecovery::recover_from_jsonl(temp.path())
            .await
            .unwrap();
        // 3 trailing reverts (r1, r2, r3); r4 is success, so chain breaks there.
        assert_eq!(state.consecutive_reverts, 3);
    }
}
