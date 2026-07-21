//! Live snapshot refresh: paced oracle repricing + discovery reload.
//!
//! The detector historically scored positions from a snapshot frozen at startup, so
//! a near-miss position (HF just above 1.05) could never be observed crossing the
//! liquidation threshold. This module makes the snapshot live along two axes:
//!
//! 1. **Repricing** (seconds cadence): every reserve's `price_usd` is refreshed from
//!    the Aave price oracle — the same source Aave's own `liquidationCall` consults —
//!    through a [`ReservePriceSource`]. One batched call reprices ALL users through
//!    the existing detector math (O(reserves), not O(users)).
//! 2. **Discovery reload** (minutes cadence): `scripts/snapshot_generator.py`
//!    atomically replaces the snapshot file (temp + `os.replace`); the refresher
//!    detects the mtime change, validates the new snapshot, and swaps it in whole.
//!
//! Both run inline from the orchestrator scan loop via [`SnapshotRefresher::tick`] —
//! a single writer, no background task. All failure paths retain last-known-good
//! state: prices are never zeroed, and a failed reload never regresses the
//! in-memory snapshot (the empty-snapshot fallback in `main.rs` is boot-only).
//!
//! Lock discipline: every lock section is synchronous and scoped; no guard is ever
//! held across an `.await`.

use crate::detector::liquidation::MarketSnapshot;
use crate::{ChimeraError, Metrics};
use alloy::primitives::{Address, U256};
use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{debug, info, warn};

/// Consecutive whole-batch reprice failures before a per-asset isolation pass
/// runs to identify (and quarantine) dead price feeds.
const QUARANTINE_AFTER_FAILURES: u64 = 5;
/// Minimum seconds between repeated reprice/reload failure warns (mirrors the
/// pacing engine's oracle-warn rate limiting).
const WARN_INTERVAL_SECS: u64 = 60;
/// Backoff cap: consecutive failures stretch the reprice interval by up to 2^3,
/// bounded by max(60, configured interval).
const MAX_BACKOFF_SHIFT: u64 = 3;

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Cheap-to-clone shared handle over the live detector snapshot.
///
/// The orchestrator, the refresher, and tests all hold clones of this handle;
/// the inner snapshot is swapped/patched under a short synchronous write lock.
#[derive(Clone)]
pub struct SharedSnapshot {
    inner: Arc<RwLock<MarketSnapshot>>,
}

impl SharedSnapshot {
    pub fn new(initial: MarketSnapshot) -> Self {
        Self {
            inner: Arc::new(RwLock::new(initial)),
        }
    }

    /// Clone the current snapshot (short read lock). Each scan takes ONE clone and
    /// uses it for both detector construction and the block number handed to the
    /// simulator, so a mid-scan refresh can never produce a mixed-epoch scan.
    pub fn snapshot(&self) -> MarketSnapshot {
        self.inner.read().clone()
    }

    pub fn block_number(&self) -> u64 {
        self.inner.read().block_number
    }

    /// Age of the loaded snapshot (generation wall-clock time), in seconds.
    pub fn age_secs(&self) -> u64 {
        let ts = self.inner.read().timestamp;
        (chrono::Utc::now() - ts).num_seconds().max(0) as u64
    }

    /// Addresses of all reserves currently in the snapshot.
    pub fn reserve_addresses(&self) -> Vec<Address> {
        self.inner.read().reserves.keys().copied().collect()
    }

    /// Overwrite `price_usd` (8-decimal USD, raw oracle `U256`) for reserves present
    /// in `prices`. Zero/non-positive prices and unknown addresses are ignored —
    /// a zero price would silently mute debt (HF -> MAX) or vaporize collateral
    /// (false-candidate storm), so last-known-good always wins. Returns the number
    /// of reserves actually updated. This is the ONLY field repricing touches.
    pub fn apply_prices(&self, prices: &HashMap<Address, U256>) -> usize {
        let mut snap = self.inner.write();
        let mut updated = 0;
        for (asset, price) in prices {
            if price.is_zero() {
                continue;
            }
            if let Some(reserve) = snap.reserves.get_mut(asset) {
                reserve.price_usd = *price;
                updated += 1;
            }
        }
        updated
    }

    /// Replace the whole snapshot (validated discovery reload).
    pub fn replace(&self, fresh: MarketSnapshot) {
        *self.inner.write() = fresh;
    }
}

/// Raw 8-decimal USD price source — the exact unit `ReserveData.price_usd` stores.
/// No `Decimal`/`f64` anywhere on this path. The production implementation batches
/// the Aave oracle's `getAssetsPrices`; tests inject scripted sources.
#[async_trait]
pub trait ReservePriceSource: Send + Sync {
    /// Fetch prices for `assets`. Implementations should be all-or-nothing per
    /// call (one atomic eth_call); entries absent from the returned map simply
    /// retain their previous snapshot price.
    async fn get_prices_raw(
        &self,
        assets: &[Address],
    ) -> Result<HashMap<Address, U256>, ChimeraError>;
}

/// What a [`SnapshotRefresher::tick`] did, so the orchestrator can react
/// (a reload requires re-prewarming the simulator).
#[derive(Debug, Clone, Copy, Default)]
pub struct RefreshOutcome {
    pub reloaded: bool,
    pub repriced: bool,
}

/// Inline (scan-loop-driven) snapshot refresher. See module docs.
pub struct SnapshotRefresher {
    shared: SharedSnapshot,
    price_source: Arc<dyn ReservePriceSource>,
    snapshot_path: PathBuf,
    chain_id: u64,
    price_refresh_secs: u64,
    last_reprice_ok_epoch: AtomicU64,
    last_reprice_attempt_epoch: AtomicU64,
    consecutive_failures: AtomicU64,
    last_warn_epoch: AtomicU64,
    last_mtime: Mutex<Option<SystemTime>>,
    quarantined: Mutex<HashSet<Address>>,
}

impl SnapshotRefresher {
    pub fn new(
        shared: SharedSnapshot,
        price_source: Arc<dyn ReservePriceSource>,
        snapshot_path: PathBuf,
        chain_id: u64,
        price_refresh_secs: u64,
    ) -> Self {
        // The boot snapshot's prices are as fresh as its generation time, so the
        // price age starts from the snapshot timestamp, not from "now": a stale
        // file at boot reads as stale until the first successful live reprice.
        let boot_price_epoch = {
            let snap = shared.inner.read();
            snap.timestamp.timestamp().max(0) as u64
        };
        // Record the file's current mtime so the first tick does not "reload" the
        // very file the engine just booted from.
        let boot_mtime = std::fs::metadata(&snapshot_path)
            .and_then(|m| m.modified())
            .ok();
        Self {
            shared,
            price_source,
            snapshot_path,
            chain_id,
            price_refresh_secs: price_refresh_secs.max(1),
            last_reprice_ok_epoch: AtomicU64::new(boot_price_epoch),
            last_reprice_attempt_epoch: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            last_warn_epoch: AtomicU64::new(0),
            last_mtime: Mutex::new(boot_mtime),
            quarantined: Mutex::new(HashSet::new()),
        }
    }

    pub fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    /// Seconds since the last successful reprice (or since snapshot generation if
    /// no live reprice has succeeded yet).
    pub fn price_age_secs(&self) -> u64 {
        epoch_now().saturating_sub(self.last_reprice_ok_epoch.load(Ordering::Relaxed))
    }

    /// Per-scan entry point. Order: (1) discovery reload check (a free `stat()`
    /// unless the file changed); (2) reprice if due under pacing/backoff;
    /// (3) freshness gauges. Never errors — every failure path retains
    /// last-known-good state.
    pub async fn tick(&self, metrics: &Metrics, chain_label: &str) -> RefreshOutcome {
        let reloaded = self.check_reload(metrics, chain_label);

        let now = epoch_now();
        let mut repriced = false;
        if self.reprice_due(now) {
            self.last_reprice_attempt_epoch.store(now, Ordering::Relaxed);
            repriced = self.reprice(metrics, chain_label).await;
        }

        metrics.set_price_refresh_age(chain_label, self.price_age_secs());
        metrics.set_snapshot_age(chain_label, self.shared.age_secs());
        metrics.set_snapshot_block(chain_label, self.shared.block_number());

        RefreshOutcome { reloaded, repriced }
    }

    /// Validation gate applied to a freshly parsed snapshot BEFORE it replaces the
    /// in-memory one. Equal block numbers are accepted (a same-block regeneration
    /// can add users); the mtime trigger prevents re-apply loops.
    fn validate_reload(
        current_block: u64,
        chain_id: u64,
        fresh: &MarketSnapshot,
    ) -> Result<(), String> {
        if fresh.reserves.is_empty() {
            return Err("no reserves".into());
        }
        if fresh.chain_id != chain_id {
            return Err(format!(
                "chain_id {} does not match engine chain {}",
                fresh.chain_id, chain_id
            ));
        }
        if fresh.block_number == 0 {
            return Err("block_number 0 (mock generator output?)".into());
        }
        if fresh.block_number < current_block {
            return Err(format!(
                "block_number went backwards ({} < {})",
                fresh.block_number, current_block
            ));
        }
        Ok(())
    }

    /// Check the snapshot file for an atomic replacement and swap it in if valid.
    /// Returns true only when a new snapshot was applied.
    fn check_reload(&self, metrics: &Metrics, chain_label: &str) -> bool {
        let Ok(meta) = std::fs::metadata(&self.snapshot_path) else {
            return false; // no file: boot may have run snapshot-less; nothing to do
        };
        let Ok(mtime) = meta.modified() else {
            return false;
        };
        if *self.last_mtime.lock() == Some(mtime) {
            return false;
        }

        match MarketSnapshot::load_from_file(&self.snapshot_path) {
            Err(e) => {
                // Read/parse failure. On Windows this can be a transient sharing
                // violation racing the generator's os.replace, indistinguishable
                // here from a real parse error — so do NOT record the mtime;
                // retry on the next tick. Warn rate-limited.
                metrics.observe_snapshot_reload(chain_label, "read_error");
                if self.should_warn() {
                    warn!(
                        target: "chimera::refresh",
                        path = %self.snapshot_path.display(),
                        error = %e,
                        "Snapshot reload read/parse failed; keeping current snapshot"
                    );
                }
                false
            }
            Ok(fresh) => {
                // Evaluation complete for this file version either way; only a
                // future rewrite should trigger another attempt.
                *self.last_mtime.lock() = Some(mtime);
                let current_block = self.shared.block_number();
                match Self::validate_reload(current_block, self.chain_id, &fresh) {
                    Err(reason) => {
                        metrics.observe_snapshot_reload(chain_label, "rejected");
                        warn!(
                            target: "chimera::refresh",
                            path = %self.snapshot_path.display(),
                            reason = %reason,
                            "Snapshot reload REJECTED; keeping current snapshot"
                        );
                        false
                    }
                    Ok(()) => {
                        if fresh.users.is_empty() {
                            warn!(
                                target: "chimera::refresh",
                                "Reloaded snapshot has zero users (legitimate but suspicious)"
                            );
                        }
                        let (users, reserves, block) =
                            (fresh.users.len(), fresh.reserves.len(), fresh.block_number);
                        self.shared.replace(fresh);
                        // New reserves may not overlap the old quarantine set.
                        self.quarantined.lock().clear();
                        metrics.observe_snapshot_reload(chain_label, "applied");
                        info!(
                            target: "chimera::refresh",
                            block_from = current_block,
                            block_to = block,
                            users,
                            reserves,
                            "Discovery snapshot reloaded"
                        );
                        true
                    }
                }
            }
        }
    }

    /// Reprice pacing: due when the configured interval (stretched exponentially by
    /// consecutive failures, capped at max(60, interval)) has elapsed since the last
    /// attempt. A sustained 429 storm self-throttles without operator action.
    fn reprice_due(&self, now: u64) -> bool {
        let last_attempt = self.last_reprice_attempt_epoch.load(Ordering::Relaxed);
        let failures = self
            .consecutive_failures
            .load(Ordering::Relaxed)
            .min(MAX_BACKOFF_SHIFT);
        let interval = (self.price_refresh_secs << failures).min(self.price_refresh_secs.max(60));
        now.saturating_sub(last_attempt) >= interval
    }

    /// One batched price fetch, applied all-or-nothing (the batch is one atomic
    /// eth_call, so applied prices are same-block consistent).
    async fn reprice(&self, metrics: &Metrics, chain_label: &str) -> bool {
        let assets: Vec<Address> = {
            let quarantined = self.quarantined.lock();
            self.shared
                .reserve_addresses()
                .into_iter()
                .filter(|a| !quarantined.contains(a))
                .collect()
        };
        if assets.is_empty() {
            return false;
        }

        match self.price_source.get_prices_raw(&assets).await {
            Ok(prices) if !prices.is_empty() => {
                let updated = self.shared.apply_prices(&prices);
                self.mark_reprice_success(metrics, chain_label, updated);
                true
            }
            other => {
                if let Err(e) = other {
                    if self.should_warn() {
                        warn!(
                            target: "chimera::refresh",
                            error = %e,
                            "Reserve reprice failed; retaining last prices"
                        );
                    }
                }
                metrics.observe_price_refresh(chain_label, "error");
                let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                if failures >= QUARANTINE_AFTER_FAILURES {
                    return self.isolate_and_quarantine(&assets, metrics, chain_label).await;
                }
                false
            }
        }
    }

    /// After repeated whole-batch failures, probe each asset individually: healthy
    /// feeds get their prices applied; dead feeds are quarantined (excluded from
    /// future batches, stale price retained — the simulator gate rejects any
    /// candidate touching an asset whose on-chain feed reverts). If EVERY probe
    /// fails this is an RPC outage, not dead feeds — quarantine nothing.
    async fn isolate_and_quarantine(
        &self,
        assets: &[Address],
        metrics: &Metrics,
        chain_label: &str,
    ) -> bool {
        let mut healthy: HashMap<Address, U256> = HashMap::new();
        let mut dead: Vec<Address> = Vec::new();
        for asset in assets {
            match self.price_source.get_prices_raw(std::slice::from_ref(asset)).await {
                Ok(prices) if prices.values().any(|p| !p.is_zero()) => {
                    healthy.extend(prices);
                }
                _ => dead.push(*asset),
            }
        }

        if healthy.is_empty() {
            // Whole-RPC outage: keep counting failures (backoff continues), touch nothing.
            if self.should_warn() {
                warn!(
                    target: "chimera::refresh",
                    probed = assets.len(),
                    "Isolation pass: every probe failed (RPC outage); no quarantine"
                );
            }
            return false;
        }

        if !dead.is_empty() {
            let mut quarantined = self.quarantined.lock();
            for asset in &dead {
                quarantined.insert(*asset);
            }
            warn!(
                target: "chimera::refresh",
                quarantined = ?dead,
                "Quarantined dead price feeds (stale price retained, excluded from batches)"
            );
            metrics.observe_price_refresh(chain_label, "quarantined");
        }

        let updated = self.shared.apply_prices(&healthy);
        self.mark_reprice_success(metrics, chain_label, updated);
        true
    }

    fn mark_reprice_success(&self, metrics: &Metrics, chain_label: &str, updated: usize) {
        self.last_reprice_ok_epoch
            .store(epoch_now(), Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
        metrics.observe_price_refresh(chain_label, "success");
        debug!(
            target: "chimera::refresh",
            updated,
            "Reserve prices refreshed from live oracle"
        );
    }

    /// Rate-limited warn gate: first failure always warns, subsequent ones at most
    /// once per [`WARN_INTERVAL_SECS`] (mirrors `PacingEngine::should_emit_oracle_warn`).
    fn should_warn(&self) -> bool {
        let now = epoch_now();
        let last = self.last_warn_epoch.load(Ordering::Relaxed);
        if last == 0 || now.saturating_sub(last) >= WARN_INTERVAL_SECS {
            self.last_warn_epoch.store(now, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::liquidation::ReserveData;

    fn addr(n: u8) -> Address {
        Address::with_last_byte(n)
    }

    fn reserve(price_8dec: u64) -> ReserveData {
        ReserveData {
            price_usd: U256::from(price_8dec),
            ..Default::default()
        }
    }

    fn snapshot_with_reserves(block: u64, prices: &[(Address, u64)]) -> MarketSnapshot {
        let mut snap = MarketSnapshot {
            block_number: block,
            chain_id: 8453,
            timestamp: chrono::Utc::now(),
            ..Default::default()
        };
        for (a, p) in prices {
            snap.reserves.insert(*a, reserve(*p));
        }
        snap
    }

    struct ScriptedSource {
        /// Per-asset responses; assets absent here make any batch containing them fail.
        prices: Mutex<HashMap<Address, U256>>,
        /// When true, batch calls (len > 1) always fail — isolates the quarantine path.
        fail_batches: std::sync::atomic::AtomicBool,
    }

    impl ScriptedSource {
        fn new(prices: &[(Address, u64)]) -> Self {
            Self {
                prices: Mutex::new(
                    prices
                        .iter()
                        .map(|(a, p)| (*a, U256::from(*p)))
                        .collect(),
                ),
                fail_batches: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl ReservePriceSource for ScriptedSource {
        async fn get_prices_raw(
            &self,
            assets: &[Address],
        ) -> Result<HashMap<Address, U256>, ChimeraError> {
            if assets.len() > 1 && self.fail_batches.load(Ordering::Relaxed) {
                return Err(ChimeraError::OracleError("batch reverted".into()));
            }
            let prices = self.prices.lock();
            let mut out = HashMap::new();
            for a in assets {
                match prices.get(a) {
                    Some(p) => {
                        out.insert(*a, *p);
                    }
                    // All-or-nothing semantics: one unknown asset fails the call.
                    None => return Err(ChimeraError::OracleError(format!("no feed for {a}"))),
                }
            }
            Ok(out)
        }
    }

    fn refresher_over(
        snapshot: MarketSnapshot,
        source: Arc<dyn ReservePriceSource>,
        path: PathBuf,
    ) -> (SharedSnapshot, SnapshotRefresher) {
        let shared = SharedSnapshot::new(snapshot);
        let refresher =
            SnapshotRefresher::new(shared.clone(), source, path, 8453, 8);
        (shared, refresher)
    }

    // ---- SharedSnapshot -------------------------------------------------------

    #[test]
    fn apply_prices_updates_only_known_nonzero() {
        let (a, b, ghost) = (addr(1), addr(2), addr(9));
        let shared = SharedSnapshot::new(snapshot_with_reserves(10, &[(a, 100), (b, 200)]));

        let mut prices = HashMap::new();
        prices.insert(a, U256::from(150u64)); // valid update
        prices.insert(b, U256::ZERO); // zero: must be ignored
        prices.insert(ghost, U256::from(999u64)); // unknown reserve: ignored

        assert_eq!(shared.apply_prices(&prices), 1);
        let snap = shared.snapshot();
        assert_eq!(snap.reserves[&a].price_usd, U256::from(150u64));
        assert_eq!(snap.reserves[&b].price_usd, U256::from(200u64), "zero must not clobber");
    }

    #[test]
    fn replace_swaps_whole_snapshot() {
        let a = addr(1);
        let shared = SharedSnapshot::new(snapshot_with_reserves(10, &[(a, 100)]));
        shared.replace(snapshot_with_reserves(11, &[(a, 300)]));
        assert_eq!(shared.block_number(), 11);
        assert_eq!(shared.snapshot().reserves[&a].price_usd, U256::from(300u64));
    }

    // ---- validate_reload ------------------------------------------------------

    #[test]
    fn validate_reload_reject_matrix() {
        let a = addr(1);
        // empty reserves
        let empty = MarketSnapshot {
            block_number: 11,
            chain_id: 8453,
            ..Default::default()
        };
        assert!(SnapshotRefresher::validate_reload(10, 8453, &empty).is_err());
        // chain mismatch
        let mut wrong_chain = snapshot_with_reserves(11, &[(a, 1)]);
        wrong_chain.chain_id = 42161;
        assert!(SnapshotRefresher::validate_reload(10, 8453, &wrong_chain).is_err());
        // zero block (mock output)
        let zero_block = snapshot_with_reserves(0, &[(a, 1)]);
        assert!(SnapshotRefresher::validate_reload(10, 8453, &zero_block).is_err());
        // backwards block
        let backwards = snapshot_with_reserves(9, &[(a, 1)]);
        assert!(SnapshotRefresher::validate_reload(10, 8453, &backwards).is_err());
        // equal block: accepted (same-block regeneration may add users)
        let equal = snapshot_with_reserves(10, &[(a, 1)]);
        assert!(SnapshotRefresher::validate_reload(10, 8453, &equal).is_ok());
        // forward block: accepted
        let forward = snapshot_with_reserves(11, &[(a, 1)]);
        assert!(SnapshotRefresher::validate_reload(10, 8453, &forward).is_ok());
    }

    // ---- reprice pacing / backoff --------------------------------------------

    #[tokio::test]
    async fn reprice_backoff_stretches_with_failures() {
        let a = addr(1);
        let source = Arc::new(ScriptedSource::new(&[])); // every call fails
        let (_shared, refresher) = refresher_over(
            snapshot_with_reserves(10, &[(a, 100)]),
            source,
            PathBuf::from("nonexistent.json"),
        );

        // No failures yet: due immediately.
        assert!(refresher.reprice_due(epoch_now()));

        let now = epoch_now();
        refresher.last_reprice_attempt_epoch.store(now, Ordering::Relaxed);
        // 0 failures: 8s interval.
        assert!(!refresher.reprice_due(now + 7));
        assert!(refresher.reprice_due(now + 8));
        // 2 failures: 32s interval.
        refresher.consecutive_failures.store(2, Ordering::Relaxed);
        assert!(!refresher.reprice_due(now + 31));
        assert!(refresher.reprice_due(now + 32));
        // 10 failures: capped at max(60, 8) = 60s, not 8 << 10.
        refresher.consecutive_failures.store(10, Ordering::Relaxed);
        assert!(!refresher.reprice_due(now + 59));
        assert!(refresher.reprice_due(now + 60));
    }

    #[tokio::test]
    async fn reprice_failure_retains_prices_and_counts() {
        let a = addr(1);
        let metrics = Metrics::new();
        let source = Arc::new(ScriptedSource::new(&[])); // fails: no feeds at all
        let (shared, refresher) = refresher_over(
            snapshot_with_reserves(10, &[(a, 100)]),
            source,
            PathBuf::from("nonexistent.json"),
        );

        let outcome = refresher.tick(&metrics, "base").await;
        assert!(!outcome.repriced);
        assert_eq!(shared.snapshot().reserves[&a].price_usd, U256::from(100u64));
        assert_eq!(refresher.consecutive_failures.load(Ordering::Relaxed), 1);
        assert_eq!(
            metrics
                .price_refresh_total
                .with_label_values(&["base", "error"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn reprice_success_applies_and_resets_failures() {
        let (a, b) = (addr(1), addr(2));
        let metrics = Metrics::new();
        let source = Arc::new(ScriptedSource::new(&[(a, 150), (b, 250)]));
        let (shared, refresher) = refresher_over(
            snapshot_with_reserves(10, &[(a, 100), (b, 200)]),
            source,
            PathBuf::from("nonexistent.json"),
        );
        refresher.consecutive_failures.store(3, Ordering::Relaxed);

        let outcome = refresher.tick(&metrics, "base").await;
        assert!(outcome.repriced);
        assert!(!outcome.reloaded);
        assert_eq!(shared.snapshot().reserves[&a].price_usd, U256::from(150u64));
        assert_eq!(shared.snapshot().reserves[&b].price_usd, U256::from(250u64));
        assert_eq!(refresher.consecutive_failures.load(Ordering::Relaxed), 0);
        assert!(refresher.price_age_secs() <= 1);
    }

    // ---- quarantine -----------------------------------------------------------

    #[tokio::test]
    async fn dead_feed_quarantined_after_repeated_batch_failures() {
        let (healthy, dead) = (addr(1), addr(2));
        let metrics = Metrics::new();
        // Batches always fail; individual probes succeed only for `healthy`.
        let source = Arc::new(ScriptedSource::new(&[(healthy, 150)]));
        source.fail_batches.store(true, Ordering::Relaxed);
        let (shared, refresher) = refresher_over(
            snapshot_with_reserves(10, &[(healthy, 100), (dead, 200)]),
            source,
            PathBuf::from("nonexistent.json"),
        );

        // Failures 1..4: batch fails, nothing else happens.
        for _ in 0..4 {
            refresher.reprice(&metrics, "base").await;
        }
        assert!(refresher.quarantined.lock().is_empty());

        // Failure 5 triggers the isolation pass.
        let ok = refresher.reprice(&metrics, "base").await;
        assert!(ok, "isolation pass with a healthy feed counts as success");
        assert!(refresher.quarantined.lock().contains(&dead));
        assert!(!refresher.quarantined.lock().contains(&healthy));
        // Healthy price applied; dead feed's stale price retained (never zeroed).
        assert_eq!(shared.snapshot().reserves[&healthy].price_usd, U256::from(150u64));
        assert_eq!(shared.snapshot().reserves[&dead].price_usd, U256::from(200u64));
        assert_eq!(refresher.consecutive_failures.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn full_rpc_outage_quarantines_nothing() {
        let (a, b) = (addr(1), addr(2));
        let metrics = Metrics::new();
        let source = Arc::new(ScriptedSource::new(&[])); // every probe fails
        let (_shared, refresher) = refresher_over(
            snapshot_with_reserves(10, &[(a, 100), (b, 200)]),
            source,
            PathBuf::from("nonexistent.json"),
        );
        refresher.consecutive_failures.store(QUARANTINE_AFTER_FAILURES - 1, Ordering::Relaxed);

        let ok = refresher.reprice(&metrics, "base").await;
        assert!(!ok);
        assert!(refresher.quarantined.lock().is_empty(), "outage must not quarantine");
    }

    // ---- discovery reload -----------------------------------------------------

    /// Minimal snapshot JSON in the generator's wire format (see the RawSnapshot
    /// header in detector/liquidation.rs).
    fn snapshot_json(block: u64, price_usd: f64) -> String {
        format!(
            r#"{{
  "chain": "base",
  "block_number": {block},
  "pool": "0xA238Dd80C259a72e81d7e4664a9801593F98d1c5",
  "timestamp": {ts},
  "reserves": [
    {{
      "address": "0x4200000000000000000000000000000000000006",
      "symbol": "WETH",
      "decimals": 18,
      "liquidation_threshold": 8300,
      "liquidation_bonus": 10500,
      "price_usd": {price_usd},
      "a_token": "0xD4a0e0b9149BCee3C920d2E00b5dE09138fd8bb7",
      "variable_debt_token": "0x24e6e0795b3c7c71D965fCc4f371803d1c1DcA1E",
      "active": true
    }}
  ],
  "users": {{
    "0x1111111111111111111111111111111111111111": {{
      "collateral": {{"0x4200000000000000000000000000000000000006": "1000000000000000000"}},
      "debt": {{}},
      "emode_category": 0,
      "is_in_isolation": false
    }}
  }}
}}"#,
            ts = chrono::Utc::now().timestamp(),
        )
    }

    fn write_atomically(path: &Path, content: &str) {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, content).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    #[tokio::test]
    async fn reload_applies_newer_file_and_rejects_stale() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("snapshot.json");
        write_atomically(&path, &snapshot_json(100, 2500.0));

        let boot = MarketSnapshot::load_from_file(&path).unwrap();
        let metrics = Metrics::new();
        let source = Arc::new(ScriptedSource::new(&[]));
        let (shared, refresher) = refresher_over(boot, source, path.clone());

        // Tick without a rewrite: mtime recorded at construction, no reload.
        assert!(!refresher.check_reload(&metrics, "base"));

        // Atomic replacement with a newer block: applied.
        write_atomically(&path, &snapshot_json(101, 2600.0));
        assert!(refresher.check_reload(&metrics, "base"));
        assert_eq!(shared.block_number(), 101);
        assert_eq!(
            metrics
                .snapshot_reload_total
                .with_label_values(&["base", "applied"])
                .get(),
            1
        );

        // Replacement with a BACKWARDS block: rejected, snapshot untouched.
        write_atomically(&path, &snapshot_json(50, 2700.0));
        assert!(!refresher.check_reload(&metrics, "base"));
        assert_eq!(shared.block_number(), 101);
        assert_eq!(
            metrics
                .snapshot_reload_total
                .with_label_values(&["base", "rejected"])
                .get(),
            1
        );

        // Rejected file's mtime was recorded: no re-evaluation churn next tick.
        assert!(!refresher.check_reload(&metrics, "base"));
    }

    #[tokio::test]
    async fn reload_parse_error_keeps_snapshot_and_retries() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("snapshot.json");
        write_atomically(&path, &snapshot_json(100, 2500.0));

        let boot = MarketSnapshot::load_from_file(&path).unwrap();
        let metrics = Metrics::new();
        let source = Arc::new(ScriptedSource::new(&[]));
        let (shared, refresher) = refresher_over(boot, source, path.clone());

        write_atomically(&path, "{ this is not json");
        assert!(!refresher.check_reload(&metrics, "base"));
        assert_eq!(shared.block_number(), 100, "corrupt file must not wipe snapshot");
        assert_eq!(
            metrics
                .snapshot_reload_total
                .with_label_values(&["base", "read_error"])
                .get(),
            1
        );

        // A subsequent good rewrite recovers.
        write_atomically(&path, &snapshot_json(102, 2800.0));
        assert!(refresher.check_reload(&metrics, "base"));
        assert_eq!(shared.block_number(), 102);
    }
}
