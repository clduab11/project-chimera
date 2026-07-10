use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use tracing::{info, warn};

use crate::{ChimeraError, PacingConfig};

pub struct SignerRegistry {
    signers: HashMap<Address, Arc<ManagedSigner>>,
    treasury: Option<Address>,
    active_workers: Vec<Address>,
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

    pub async fn sync_from_chain<P>(&self, provider: &P) -> Result<u64, ChimeraError>
    where
        P: alloy::providers::Provider<alloy::network::Ethereum>,
    {
        let count = provider
            .get_transaction_count(self.address)
            .await
            .map_err(|e| ChimeraError::RpcError(format!("get_transaction_count: {e}")))?;
        let current = self.nonce.load(Ordering::SeqCst);
        if count > current {
            self.nonce.store(count, Ordering::SeqCst);
        }
        let effective = self.nonce.load(Ordering::SeqCst);
        info!(
            address = %self.address,
            nonce = effective,
            chain_count = count,
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

        if execute_mode == "shadow" && treasury_dir_empty && worker_dir_empty {
            warn!("Shadow mode: keystore paths not configured; creating empty SignerRegistry");
            return Ok(Self {
                signers: HashMap::new(),
                treasury: None,
                active_workers: Vec::new(),
                is_shadow: true,
            });
        }
        if execute_mode == "shadow" && treasury_dir_empty != worker_dir_empty {
            warn!("Shadow mode: signer paths are incomplete; creating empty SignerRegistry");
            return Ok(Self {
                signers: HashMap::new(),
                treasury: None,
                active_workers: Vec::new(),
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
            if execute_mode == "live" || !cfg.treasury_address.is_empty() {
                let configured_treasury: Address = cfg.treasury_address.parse().map_err(|e| {
                    ChimeraError::ConfigError(format!(
                        "invalid treasury_address {:?}: {e}",
                        cfg.treasury_address
                    ))
                })?;
                if configured_treasury == Address::ZERO {
                    return Err(ChimeraError::ConfigError(
                        "treasury_address must not be the zero address".into(),
                    ));
                }
                if addr != configured_treasury {
                    return Err(ChimeraError::ConfigError(format!(
                        "treasury_address mismatch: configured {}, decrypted treasury keystore address {}",
                        configured_treasury, addr
                    )));
                }
            }
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
                        if treasury == Some(addr) {
                            return Err(ChimeraError::ConfigError(format!(
                                "worker keystore {} decrypts to treasury address {}; treasury must not be a worker",
                                path_str, addr
                            )));
                        }
                        if signers.contains_key(&addr) {
                            return Err(ChimeraError::ConfigError(format!(
                                "duplicate worker signer address {} from keystore {}",
                                addr, path_str
                            )));
                        }
                        signers.insert(addr, managed);
                        worker_count += 1;
                        info!(address = %addr, "Worker signer loaded");
                    }
                    Err(e) => {
                        if execute_mode == "live" {
                            return Err(ChimeraError::ConfigError(format!(
                                "failed to decrypt worker keystore {}: {e}",
                                path_str
                            )));
                        }
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

        let mut registry = Self {
            signers,
            treasury,
            active_workers: Vec::new(),
            is_shadow: execute_mode == "shadow",
        };

        if execute_mode == "live" {
            registry.active_workers = registry.active_workers_from_eoa_pool(&cfg.eoa_pool_path)?;
        }

        Ok(registry)
    }

    pub fn get_signer(&self, address: &Address) -> Option<Arc<ManagedSigner>> {
        self.signers.get(address).cloned()
    }

    pub fn worker_addresses(&self) -> Vec<Address> {
        if !self.active_workers.is_empty() {
            return self.active_workers.clone();
        }
        self.signers
            .keys()
            .filter(|a| self.treasury.map_or(true, |t| *a != &t))
            .copied()
            .collect()
    }

    pub fn treasury_address(&self) -> Option<Address> {
        self.treasury
    }

    pub fn treasury_signer(&self) -> Option<Arc<ManagedSigner>> {
        self.treasury.and_then(|t| self.signers.get(&t).cloned())
    }

    pub fn is_empty(&self) -> bool {
        self.signers.is_empty()
    }

    pub fn is_shadow(&self) -> bool {
        self.is_shadow
    }

    pub fn validate_against_eoa_pool(&self, eoa_pool_path: &str) -> Result<(), ChimeraError> {
        self.active_workers_from_eoa_pool(eoa_pool_path).map(|_| ())
    }

    fn active_workers_from_eoa_pool(
        &self,
        eoa_pool_path: &str,
    ) -> Result<Vec<Address>, ChimeraError> {
        use serde_json::Value;
        let content = std::fs::read_to_string(eoa_pool_path).map_err(|e| {
            ChimeraError::ConfigError(format!("cannot read EOA pool {}: {e}", eoa_pool_path))
        })?;
        let pool: Value = serde_json::from_str(&content)
            .map_err(|e| ChimeraError::ConfigError(format!("invalid EOA pool JSON: {e}")))?;
        let wallets = pool["wallets"]
            .as_array()
            .ok_or_else(|| ChimeraError::ConfigError("EOA pool missing 'wallets' array".into()))?;

        let mut active = Vec::new();
        let mut seen = HashSet::new();
        for (index, wallet) in wallets.iter().enumerate() {
            let excluded = wallet["excluded"].as_bool().unwrap_or(false);
            if excluded {
                continue;
            }
            let addr_str = wallet["address"].as_str().ok_or_else(|| {
                ChimeraError::ConfigError(format!("EOA pool entry {index} missing 'address' field"))
            })?;
            let addr: Address = addr_str.parse().map_err(|e| {
                ChimeraError::ConfigError(format!(
                    "invalid active EOA address in pool at entry {index}: {addr_str}: {e}"
                ))
            })?;
            if addr == Address::ZERO {
                return Err(ChimeraError::ConfigError(format!(
                    "active EOA pool entry {index} must not use the zero address"
                )));
            }
            if !seen.insert(addr) {
                return Err(ChimeraError::ConfigError(format!(
                    "duplicate active EOA pool address: {addr}"
                )));
            }
            if self.treasury == Some(addr) {
                return Err(ChimeraError::ConfigError(format!(
                    "active EOA pool address {addr} is the treasury; treasury must not be a worker"
                )));
            }
            if self.signers.get(&addr).is_none() {
                return Err(ChimeraError::ConfigError(format!(
                    "Live mode: EOA pool entry {} has no registered worker signer in {}",
                    addr_str, eoa_pool_path
                )));
            }
            active.push(addr);
        }
        if active.is_empty() {
            return Err(ChimeraError::ConfigError(format!(
                "live mode requires at least one non-excluded worker in EOA pool {}",
                eoa_pool_path
            )));
        }
        Ok(active)
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
        let cfg = PacingConfig {
            treasury_keystore: String::new(),
            worker_keystore_dir: String::new(),
            ..PacingConfig::default()
        };

        let result = SignerRegistry::load(&cfg, "shadow");
        assert!(
            result.is_ok(),
            "shadow mode should succeed with empty paths"
        );
        let reg = result.unwrap();
        assert!(reg.is_shadow);
        assert!(reg.is_empty());
        assert_eq!(reg.worker_addresses().len(), 0);
        assert_eq!(reg.treasury_address(), None);
    }

    #[test]
    fn test_registry_live_requires_keystores() {
        let cfg = PacingConfig {
            treasury_keystore: String::new(),
            worker_keystore_dir: String::new(),
            ..PacingConfig::default()
        };

        let result = SignerRegistry::load(&cfg, "live");
        assert!(result.is_err(), "live mode without treasury should fail");
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
            active_workers: Vec::new(),
            is_shadow: false,
        };

        let result = reg.validate_against_eoa_pool(&path);
        assert!(
            result.is_err(),
            "should reject EOA without registered signer"
        );
    }

    #[test]
    fn test_validate_eoa_pool_requires_active_entry_after_exclusions() {
        use std::io::Write;

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, r#"{{"version":"2.0","wallets":[{{"address":"0xDead000000000000000000000000000000000000","label":"excluded","chain_balances":{{}},"last_used":0,"use_count":0,"excluded":true}}]}}"#).unwrap();
        let path = tmp.path().to_string_lossy().to_string();

        let reg = SignerRegistry {
            signers: HashMap::new(),
            treasury: None,
            active_workers: Vec::new(),
            is_shadow: false,
        };

        let result = reg.validate_against_eoa_pool(&path);
        assert!(
            result.is_err(),
            "live EOA pool must contain an active worker"
        );
    }

    #[test]
    fn test_validate_eoa_pool_rejects_zero_and_duplicates() {
        use std::io::Write;

        let signer = PrivateKeySigner::from_bytes(&[7u8; 32].into()).unwrap();
        let address = signer.address();
        let managed = Arc::new(ManagedSigner::new(signer));
        let mut signers = HashMap::new();
        signers.insert(address, managed);
        let reg = SignerRegistry {
            signers,
            treasury: None,
            active_workers: Vec::new(),
            is_shadow: false,
        };

        let mut zero = tempfile::NamedTempFile::new().unwrap();
        writeln!(zero, r#"{{"wallets":[{{"address":"0x0000000000000000000000000000000000000000","excluded":false}}]}}"#).unwrap();
        assert!(reg
            .validate_against_eoa_pool(zero.path().to_str().unwrap())
            .is_err());

        let mut duplicate = tempfile::NamedTempFile::new().unwrap();
        writeln!(duplicate, r#"{{"wallets":[{{"address":"{address}","excluded":false}},{{"address":"{address}","excluded":false}}]}}"#).unwrap();
        assert!(reg
            .validate_against_eoa_pool(duplicate.path().to_str().unwrap())
            .is_err());
    }

    #[test]
    fn test_sync_from_chain_never_moves_nonce_backward() {
        let key = [99u8; 32];
        let signer = PrivateKeySigner::from_bytes(&key.into()).unwrap();
        let m = ManagedSigner::new(signer);

        assert_eq!(m.next_nonce(), 0);
        assert_eq!(m.next_nonce(), 1);
        assert_eq!(m.next_nonce(), 2);
        assert_eq!(m.current_nonce(), 3);

        let chain_count_lower: u64 = 1;
        let current = m.nonce.load(Ordering::SeqCst);
        if chain_count_lower > current {
            m.nonce.store(chain_count_lower, Ordering::SeqCst);
        }
        assert_eq!(
            m.current_nonce(),
            3,
            "nonce must not move backward when chain reports a lower count"
        );

        let chain_count_higher: u64 = 5;
        let current = m.nonce.load(Ordering::SeqCst);
        if chain_count_higher > current {
            m.nonce.store(chain_count_higher, Ordering::SeqCst);
        }
        assert_eq!(
            m.current_nonce(),
            5,
            "nonce must advance when chain reports a higher count"
        );

        assert_eq!(m.next_nonce(), 5);
        assert_eq!(m.next_nonce(), 6);
        assert_eq!(m.current_nonce(), 7);
    }
}
