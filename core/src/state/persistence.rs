//! JSONL-backed persistence with rotation and atomic writes.

use async_trait::async_trait;
use std::path::PathBuf;
use tokio::fs;
use tokio::fs::File;
use tracing::{info, warn};

use crate::state::{OutcomeRecord, StatePersistence};
use crate::ChimeraError;

/// JSONL-backed persistence with configurable rotation.
///
/// Each outcome is appended as a single JSON line. Files are rotated
/// when they exceed `max_file_size_mb`.
#[derive(Debug, Clone)]
pub struct JsonlPersistence {
    /// Path to the active JSONL audit trail.
    pub file_path: PathBuf,
    /// Maximum file size in megabytes before rotation.
    pub max_file_size_mb: u64,
    /// Number of rotated files to retain.
    pub rotation_count: u32,
}

impl JsonlPersistence {
    /// Create a new `JsonlPersistence`.
    pub fn new(file_path: PathBuf, max_file_size_mb: u64, rotation_count: u32) -> Self {
        Self {
            file_path,
            max_file_size_mb,
            rotation_count,
        }
    }

    /// Rotate the audit trail if it exceeds the configured size.
    async fn rotate_if_needed(&self) -> Result<(), ChimeraError> {
        let metadata = match fs::metadata(&self.file_path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(ChimeraError::Io(e)),
        };

        let max_bytes = self.max_file_size_mb * 1024 * 1024;
        if metadata.len() <= max_bytes {
            return Ok(());
        }

        // Rotate existing backups: .2 -> .3, .1 -> .2, etc.
        for i in (1..self.rotation_count).rev() {
            let from = self.file_path.with_extension(format!("jsonl.{}", i));
            let to = self.file_path.with_extension(format!("jsonl.{}", i + 1));
            if from.exists() {
                fs::rename(&from, &to).await.map_err(ChimeraError::Io)?;
            }
        }

        let rotated = self.file_path.with_extension("jsonl.1");
        fs::rename(&self.file_path, &rotated)
            .await
            .map_err(ChimeraError::Io)?;

        info!(
            "Rotated {} -> {}",
            self.file_path.display(),
            rotated.display()
        );

        Ok(())
    }
}

#[async_trait]
impl StatePersistence for JsonlPersistence {
    async fn append_outcome(&self, outcome: OutcomeRecord) -> Result<(), ChimeraError> {
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent).await.map_err(ChimeraError::Io)?;
        }

        let line = serde_json::to_string(&outcome).map_err(|e| {
            ChimeraError::PersistenceError(format!("JSON serialization failed: {}", e))
        })?;

        // Atomic write: read existing content, append new line, write to temp,
        // fsync temp, then rename over the original.
        let existing = if self.file_path.exists() {
            fs::read_to_string(&self.file_path)
                .await
                .map_err(ChimeraError::Io)?
        } else {
            String::new()
        };

        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&line);
        content.push('\n');

        // Log the target path for debugging
        let display_path = self.file_path.display();
        let log_target = format!("Writing to file: {}", display_path);
        println!("{}", log_target);

        let temp_path = self.file_path.with_extension("jsonl.tmp");

        if let Some(parent) = temp_path.parent() {
            fs::create_dir_all(parent).await.map_err(ChimeraError::Io)?;
        }

        // Write content to temp file with error handling
        let write_result = fs::write(&temp_path, &content).await;
        match write_result {
            Ok(_) => (),
            Err(e) => {
                eprintln!("Failed to write temp file: {:?}", e);
                let error = ChimeraError::Io(e);
                // Clean up temp file if it exists
                if temp_path.exists() {
                    let _ = fs::remove_file(&temp_path).await;
                }
                return Err(error);
            }
        }

        // Ensure temp file is properly closed before opening for sync
        drop(content);

        // Try to open file for sync - handle errors gracefully
        let temp_file_result = File::open(&temp_path).await;
        match temp_file_result {
            Ok(temp_file) => {
                let sync_result = temp_file.sync_all().await;
                match sync_result {
                    Ok(_) => (),
                    Err(e) => {
                        eprintln!("Failed to sync temp file: {:?}", e);
                        // Clean up temp file
                        let _ = fs::remove_file(&temp_path).await;
                        return Err(ChimeraError::Io(e));
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to open temp file for sync: {:?}", e);
                // Clean up temp file
                if temp_path.exists() {
                    let _ = fs::remove_file(&temp_path).await;
                }
                return Err(ChimeraError::Io(e));
            }
        }

        // Try to rename temp file to final location
        let rename_result = fs::rename(&temp_path, &self.file_path).await;
        match rename_result {
            Ok(_) => {},
            Err(e) => {
                eprintln!("Failed to rename temp file: {:?}", e);
                // Try to clean up temp file even if rename fails
                if temp_path.exists() {
                    let _ = fs::remove_file(&temp_path).await;
                }
                return Err(ChimeraError::Io(e));
            }
        }

        self.rotate_if_needed().await?;
        Ok(())
    }

    async fn recover_state(&self) -> Result<crate::state::RecoveredState, ChimeraError> {
        crate::state::recovery::CrashRecovery::recover_from_jsonl(&self.file_path).await
    }

    async fn load_recent(&self, limit: usize) -> Result<Vec<OutcomeRecord>, ChimeraError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let content = match fs::read_to_string(&self.file_path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(ChimeraError::Io(e)),
        };

        let mut records = Vec::with_capacity(limit);

        for line in content.lines().filter(|l| !l.is_empty()) {
            match serde_json::from_str::<OutcomeRecord>(line) {
                Ok(record) => {
                    records.push(record);
                    if records.len() > limit {
                        records.remove(0);
                    }
                }
                Err(e) => {
                    warn!(
                        "Skipping corrupted JSONL line in {}: {}",
                        self.file_path.display(),
                        e
                    );
                }
            }
        }

        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal::Decimal;

    fn make_test_record(id: &str, net: f64, reverted: bool) -> OutcomeRecord {
        OutcomeRecord {
            id: id.to_string(),
            timestamp: Utc::now(),
            decision: "Allow".to_string(),
            realized_net_usd: Decimal::from_f64_retain(net).unwrap_or(Decimal::ZERO),
            gas_spent_eth: Decimal::from_f64_retain(0.001).unwrap_or(Decimal::ZERO),
            reverted,
            venue: "aerodrome".to_string(),
            eoa: "0xClean1".to_string(),
            chain_id: 8453,
        }
    }

    #[tokio::test]
    #[cfg_attr(target_os = "windows", ignore = "fsync not supported on Windows temp directories")]
    async fn test_append_and_load_recent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let persistence = JsonlPersistence::new(path.clone(), 10, 3);

        let r1 = make_test_record("001", 100.0, false);
        let r2 = make_test_record("002", 200.0, false);
        let r3 = make_test_record("003", 300.0, true);

        persistence.append_outcome(r1.clone()).await.unwrap();
        persistence.append_outcome(r2.clone()).await.unwrap();
        persistence.append_outcome(r3.clone()).await.unwrap();

        let recent = persistence.load_recent(2).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, "002");
        assert_eq!(recent[1].id, "003");
    }

    #[tokio::test]
    async fn test_load_recent_corrupted_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        fs::write(
            &path,
            "{\"id\":\"001\",\"timestamp\":\"2024-01-01T00:00:00Z\",\"decision\":\"Allow\",\"realized_net_usd\":\"100\",\"gas_spent_eth\":\"0.001\",\"reverted\":false,\"venue\":\"aerodrome\",\"eoa\":\"0x1\",\"chain_id\":8453}\nNOT_JSON\n{\"id\":\"003\",\"timestamp\":\"2024-01-01T00:00:00Z\",\"decision\":\"Allow\",\"realized_net_usd\":\"300\",\"gas_spent_eth\":\"0.001\",\"reverted\":true,\"venue\":\"aerodrome\",\"eoa\":\"0x1\",\"chain_id\":8453}\n",
        )
        .await
        .unwrap();

        let persistence = JsonlPersistence::new(path, 10, 3);
        let recent = persistence.load_recent(10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, "001");
        assert_eq!(recent[1].id, "003");
    }

    #[tokio::test]
    #[cfg_attr(target_os = "windows", ignore = "fsync not supported on Windows temp directories")]
    async fn test_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        // Set threshold to 0 MB so every append triggers rotation.
        let persistence = JsonlPersistence::new(path.clone(), 0, 2);

        let r = make_test_record("big", 1.0, false);
        persistence.append_outcome(r).await.unwrap();

        assert!(path.with_extension("jsonl.1").exists());
    }

    #[tokio::test]
    #[cfg_attr(target_os = "windows", ignore = "fsync not supported on Windows temp directories")]
    async fn test_recover_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let persistence = JsonlPersistence::new(path, 10, 3);

        let r1 = make_test_record("001", 50.0, false);
        let r2 = make_test_record("002", 75.0, true);
        persistence.append_outcome(r1).await.unwrap();
        persistence.append_outcome(r2).await.unwrap();

        let state = persistence.recover_state().await.unwrap();
        assert_eq!(state.consecutive_reverts, 1);
        assert!(state.daily_usage_usd > Decimal::ZERO);
        assert!(state.last_outcome_time.is_some());
    }
}
