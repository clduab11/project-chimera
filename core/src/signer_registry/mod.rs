use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use tracing::{info, warn};

use crate::{ChimeraError, PacingConfig};

pub struct SignerRegistry {
    signers: HashMap<Address, Arc<ManagedSigner>>,
    treasury: Option<Address>,
    pub is_shadow: bool,
}

pub struct ManagedSigner {
    pub signer: PrivateKeySigner,
    pub address: Address,
    nonce: AtomicU64,
}

impl ManagedSigner {
    pub fn new(signer: PrivateKeySigner) -> Self {
        let address = signer.address();
        Self {
            signer,
            address,
            nonce: AtomicU64::new(0),
        }
    }

    pub fn current_nonce(&self) -> u64 {
        self.nonce.load(Ordering::SeqCst)
    }

    pub fn next_nonce(&self) -> u64 {
        self.nonce.fetch_add(1, Ordering::SeqCst)
    }

    pub async fn sync_from_chain<P>(
        &self,
        provider: &P,
    ) -> Result<u64, ChimeraError>
    where
        P: alloy::providers::Provider<alloy::network::Ethereum>,
    {
        let count = provider
            .get_transaction_count(self.address)
            .await
            .map_err(|e| ChimeraError::RpcError(format!("get_transaction_count: {e}")))?;
        self.nonce.store(count, Ordering::SeqCst);
        info!(
            address = %self.address,
            nonce = count,
            "ManagedSigner synced from chain"
        );
        Ok(count)
    }
}

impl SignerRegistry {
    pub fn load(cfg: &PacingConfig, execute_mode: &str) -> Result<Self, ChimeraError> {
        let password = std::env::var("CHIMERA_KEYSTORE_PASSWORD").ok();

        let treasury_dir_empty = cfg.treasury_keystore.is_empty();
        let worker_dir_empty = cfg.worker_keystore_dir.is_empty();

        if execute_mode == "shadow" && (treasury_dir_empty || worker_dir_empty) {
            warn!(
                "Shadow mode: keystore paths not configured; creating empty SignerRegistry"
            );
            return Ok(Self {
                signers: HashMap::new(),
                treasury: None,
                is_shadow: true,
            });
        }

        if execute_mode == "live" {
            if treasury_dir_empty {
                return Err(ChimeraError::ConfigError(
                    "live mode requires treasury_keystore in pacing.yaml".into(),
                ));
            }
            if worker_dir_empty {
                return Err(ChimeraError::ConfigError(
                    "live mode requires worker_keystore_dir in pacing.yaml".into(),
                ));
            }
        }

        let password = password.ok_or_else(|| {
            ChimeraError::ConfigError(
                "CHIMERA_KEYSTORE_PASSWORD env var required to load keystores".into(),
            )
        })?;

        let mut signers: HashMap<Address, Arc<ManagedSigner>> = HashMap::new();
        let mut treasury: Option<Address> = None;

        if !cfg.treasury_keystore.is_empty() {
            let signer = PrivateKeySigner::decrypt_keystore(&cfg.treasury_keystore, &password)
                .map_err(|e| {
                    ChimeraError::ConfigError(format!(
                        "failed to decrypt treasury keystore {}: {e}",
                        cfg.treasury_keystore
                    ))
                })?;
            let managed = Arc::new(ManagedSigner::new(signer));
            let addr = managed.address;
            signers.insert(addr, managed);
            treasury = Some(addr);
            info!(address = %addr, "Treasury signer loaded");
        }

        if !cfg.worker_keystore_dir.is_empty() {
            let dir = std::path::Path::new(&cfg.worker_keystore_dir);
            let entries = std::fs::read_dir(dir).map_err(|e| {
                ChimeraError::ConfigError(format!(
                    "cannot read worker_keystore_dir {}: {e}",
                    cfg.worker_keystore_dir
                ))
            })?;

            let mut worker_count = 0u32;
            for entry in entries {
                let entry = entry.map_err(|e| {
                    ChimeraError::ConfigError(format!(
                        "error reading entry in worker_keystore_dir: {e}"
                    ))
                })?;
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let path_str = path.to_string_lossy().to_string();
                match PrivateKeySigner::decrypt_keystore(&path_str, &password) {
                    Ok(signer) => {
                        let managed = Arc::new(ManagedSigner::new(signer));
                        let addr = managed.address;
                        signers.insert(addr, managed);
                        worker_count += 1;
                        info!(address = %addr, "Worker signer loaded");
                    }
                    Err(e) => {
                        warn!(
                            path = %path_str,
                            error = %e,
                            "Skipping unreadable keystore file"
                        );
                    }
                }
            }

            if execute_mode == "live" && worker_count == 0 {
                return Err(ChimeraError::ConfigError(
                    "live mode: no valid worker keystores found in worker_keystore_dir".into(),
                ));
            }
        }

        let registry = Self {
            signers,
            treasury,
            is_shadow: false,
        };

        if execute_mode == "live" && !cfg.eoa_pool_path.is_empty() {
            registry.validate_against_eoa_pool(&cfg.eoa_pool_path)?;
        }

        Ok(registry)
    }

    pub fn get_signer(&self, address: &Address) -> Option<Arc<ManagedSigner>> {
        self.signers.get(address).cloned()
    }

    pub fn worker_addresses(&self) -> Vec<Address> {
        self.signers
            .keys()
            .filter(|a| self.treasury.is_none_or(|t| *a != &t))
            .copied()
            .collect()
    }

    pub fn treasury_address(&self) -> Option<Address> {
        self.treasury
    }

    pub fn treasury_signer(&self) -> Option<Arc<ManagedSigner>> {
        self.treasury
            .and_then(|t| self.signers.get(&t).cloned())
    }

    pub fn is_empty(&self) -> bool {
        self.signers.is_empty()
    }

    pub fn is_shadow(&self) -> bool {
        self.is_shadow
    }

    pub fn validate_against_eoa_pool(&self, eoa_pool_path: &str) -> Result<(), ChimeraError> {
        use serde_json::Value;
        let content = std::fs::read_to_string(eoa_pool_path).map_err(|e| {
            ChimeraError::ConfigError(format!("cannot read EOA pool {}: {e}", eoa_pool_path))
        })?;
        let pool: Value = serde_json::from_str(&content).map_err(|e| {
            ChimeraError::ConfigError(format!("invalid EOA pool JSON: {e}"))
        })?;
        let wallets = pool["wallets"].as_array().ok_or_else(|| {
            ChimeraError::ConfigError("EOA pool missing 'wallets' array".into())
        })?;

        for wallet in wallets {
            let excluded = wallet["excluded"].as_bool().unwrap_or(false);
            if excluded {
                continue;
            }
            let addr_str = wallet["address"].as_str().ok_or_else(|| {
                ChimeraError::ConfigError("EOA pool entry missing 'address' field".into())
            })?;
            let addr: Address = addr_str.parse().map_err(|_| {
                ChimeraError::ConfigError(format!("invalid EOA address in pool: {addr_str}"))
            })?;

            if !self.signers.contains_key(&addr) && addr != Address::ZERO {
                return Err(ChimeraError::ConfigError(format!(
                    "Live mode: EOA pool entry {} has no registered worker signer in {}",
                    addr_str, eoa_pool_path
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_managed_signer_nonce_isolation() {
        use alloy::signers::local::PrivateKeySigner;
        let key1 = [1u8; 32];
        let key2 = [2u8; 32];
        let s1 = PrivateKeySigner::from_bytes(&key1.into()).unwrap();
        let s2 = PrivateKeySigner::from_bytes(&key2.into()).unwrap();
        let m1 = ManagedSigner::new(s1);
        let m2 = ManagedSigner::new(s2);

        assert_ne!(m1.address, m2.address, "addresses must differ");

        assert_eq!(m1.current_nonce(), 0);
        assert_eq!(m2.current_nonce(), 0);

        assert_eq!(m1.next_nonce(), 0);
        assert_eq!(m1.next_nonce(), 1);

        assert_eq!(m2.next_nonce(), 0);
        assert_eq!(m2.current_nonce(), 1);

        assert_eq!(m1.current_nonce(), 2);
    }

    #[test]
    fn test_next_nonce_atomic_increment() {
        use alloy::signers::local::PrivateKeySigner;
        let key = [42u8; 32];
        let signer = PrivateKeySigner::from_bytes(&key.into()).unwrap();
        let m = ManagedSigner::new(signer);

        let prev = m.next_nonce();
        assert_eq!(prev, 0);
        assert_eq!(m.next_nonce(), 1);
        assert_eq!(m.next_nonce(), 2);
        assert_eq!(m.current_nonce(), 3);
    }

    #[test]
    fn test_registry_shadow_empty() {
        use std::env;

        let prev = env::var("CHIMERA_KEYSTORE_PASSWORD").ok();
        env::remove_var("CHIMERA_KEYSTORE_PASSWORD");

        let cfg = PacingConfig {
            treasury_keystore: String::new(),
            worker_keystore_dir: String::new(),
            ..PacingConfig::default()
        };

        let result = SignerRegistry::load(&cfg, "shadow");
        assert!(result.is_ok(), "shadow mode should succeed with empty paths");
        let reg = result.unwrap();
        assert!(reg.is_shadow);
        assert!(reg.is_empty());
        assert_eq!(reg.worker_addresses().len(), 0);
        assert_eq!(reg.treasury_address(), None);

        if let Some(v) = prev {
            env::set_var("CHIMERA_KEYSTORE_PASSWORD", v);
        }
    }

    #[test]
    fn test_registry_live_requires_keystores() {
        use std::env;

        let prev = env::var("CHIMERA_KEYSTORE_PASSWORD").ok();
        env::remove_var("CHIMERA_KEYSTORE_PASSWORD");

        let cfg = PacingConfig {
            treasury_keystore: String::new(),
            worker_keystore_dir: String::new(),
            ..PacingConfig::default()
        };

        let result = SignerRegistry::load(&cfg, "live");
        assert!(result.is_err(), "live mode without treasury should fail");

        if let Some(v) = prev {
            env::set_var("CHIMERA_KEYSTORE_PASSWORD", v);
        }
    }

    #[test]
    fn test_validate_eoa_pool_missing_signer() {
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, r#"{{"version":"2.0","wallets":[{{"address":"0xDead000000000000000000000000000000000000","label":"missing","chain_balances":{{}},"last_used":0,"use_count":0,"excluded":false}}]}}"#).unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        let reg = SignerRegistry {
            signers: HashMap::new(),
            treasury: None,
            is_shadow: false,
        };

        let result = reg.validate_against_eoa_pool(&path);
        assert!(result.is_err(), "should reject EOA without registered signer");
    }

    #[test]
    fn test_validate_eoa_pool_excluded_entry_skipped() {
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, r#"{{"version":"2.0","wallets":[{{"address":"0xDead000000000000000000000000000000000000","label":"excluded","chain_balances":{{}},"last_used":0,"use_count":0,"excluded":true}}]}}"#).unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        let reg = SignerRegistry {
            signers: HashMap::new(),
            treasury: None,
            is_shadow: false,
        };

        let result = reg.validate_against_eoa_pool(&path);
        assert!(result.is_ok(), "should skip excluded EOA entries");
    }
}
