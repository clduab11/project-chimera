# T7 — SignerRegistry + Sweep/Refund Scheduler: Epic Brief

**Date**: 2026-07-02
**Status**: Requirements clarified
**Dependencies**: T3 (complete), T5 (complete)
**Scope-in**: `core/src/signer_registry/`, `core/src/sweep_scheduler.rs`, `core/src/refund_scheduler.rs`, wiring in `core/src/main.rs`

---

## 1. Problem Statement

The current architecture has a gap: worker EOAs accumulate profits from successful liquidations but there is no in-engine mechanism to consolidate those funds back to the treasury or to re-fund under-funded workers. Today this is handled externally by Python scripts (`scripts/sweep_profits.py`, `scripts/fund_eoa.py`) that an operator must run manually. Additionally, the engine only supports a single signer (one keystore loaded via `CHIMERA_KEYSTORE_PATH`), yet the EOA pool (`config/eoa_pool.json`) and pacing engine expect multi-worker rotation — this is gated behind T7.

T7 closes the loop: an in-engine scheduler sweeps worker EOAs → treasury and re-funds workers, driven by a signer registry with per-address nonce isolation. The engine becomes fully autonomous — no manual fund/sweep steps remain in the daily operator checklist.

---

## 2. Architecture Overview

```
main.rs (boot)
├── load SignerRegistry from pacing.yaml (worker_keystore_dir + treasury_keystore)
├── spawn Scheduler tokio task
│   ├── Sweep loop (interval = sweep_interval_secs, default 300s)
│   │   ├── For each worker EOA: balance check → build native ETH tx (balance - min_keep) → treasury
│   │   ├── ERC20 sweep: for each configured token, transfer full balance → treasury
│   │   ├── Nonce-safe: fetch nonce once, local increment across workers
│   │   ├── USDT-safe: check receipt.status, never rely on bool return from transfer()
│   │   ├── Shadow: log tx data, do NOT broadcast
│   │   └── Live: sign with SignerRegistry, broadcast via provider
│   └── Refund loop (interval = refund_interval_secs, default 3600s)
│       ├── For each worker EOA below min_worker_balance_eth: send refund_topup_eth from treasury
│       ├── Nonce-safe: fetch treasury nonce once, local increment
│       ├── Shadow: log tx data, do NOT broadcast
│       └── Live: sign with treasury keystore, broadcast via provider
└── existing Orchestrator loop (unchanged, runs in parallel)
```

### Component Boundaries

| Component | Crate/Module | Owns |
|-----------|-------------|------|
| `SignerRegistry` | `core::signer_registry` | Loads keystores, maps address→signer, per-address nonce counters |
| `SweepScheduler` | `core::sweep_scheduler` | Builds worker→treasury sweep txns, honors breaker/pacing, logs in shadow |
| `RefundScheduler` | `core::sweep_scheduler` (same module) | Builds treasury→worker refund txns, honors breaker/pacing, logs in shadow |
| `PacingEngine` (existing) | `core::pacing_engine` | Scheduler queries `is_breaker_active()` before building txns |
| `RpcSubmitter` (existing) | `core::executor::submitter` | Scheduler uses for broadcast in live mode (with `dry_run=false`) |
| `main.rs` (modified) | `core` | Spawns scheduler tokio task, passes SignerRegistry arc |

---

## 3. Component Specifications

### 3.1 SignerRegistry

**Module**: `core/src/signer_registry/mod.rs`
**Public API** (re-exported via `lib.rs`):

```rust
pub struct SignerRegistry { /* private fields */ }

impl SignerRegistry {
    /// Load from pacing.yaml config + env vars.
    /// - worker_keystore_dir: directory of encrypted keystores (one per EOA)
    /// - treasury_keystore: path to treasury encrypted keystore
    /// - Password: CHIMERA_KEYSTORE_PASSWORD env var (single password for all)
    /// Shadow mode: if keystores are missing, creates placeholder registry (no-op).
    /// Live mode: errors if any required keystore is missing.
    pub fn load(cfg: &PacingConfig, execute_mode: &str) -> Result<Self, ChimeraError>;

    /// Return a signer for the given address, or None if not registered.
    /// The returned signer carries a reference to the per-address nonce counter.
    pub fn get_signer(&self, address: &Address) -> Option<Arc<ManagedSigner>>;

    /// Iterate all registered worker addresses (excludes treasury).
    pub fn worker_addresses(&self) -> Vec<Address>;

    /// The treasury signer address (or None in shadow without keystore).
    pub fn treasury_address(&self) -> Option<Address>;

    /// Whether this registry is in shadow mode (no real keys loaded).
    pub fn is_shadow(&self) -> bool;
}

/// A signer with per-address nonce isolation.
pub struct ManagedSigner {
    signer: PrivateKeySigner,
    address: Address,
    nonce: AtomicU64,  // per-address nonce counter, separate from on-chain
}

impl ManagedSigner {
    /// Read the current local nonce (may be ahead of on-chain if txs are pending).
    pub fn current_nonce(&self) -> u64;

    /// Advance the local nonce by 1, returning the previous value (for use as tx nonce).
    pub fn next_nonce(&self) -> u64;

    /// Sync the local nonce from on-chain (called at scheduler loop start).
    pub async fn sync_from_chain(&self, provider: &impl Provider) -> Result<u64, ChimeraError>;
}
```

**Key design decisions**:
- One `CHIMERA_KEYSTORE_PASSWORD` for all keystores (matches existing `load_signer` pattern)
- Atomic nonce counters — thread-safe, no Mutex needed since the scheduler is single-threaded per signer
- Shadow mode: `SignerRegistry` is constructed with zero addresses, `get_signer` returns `None`, `is_shadow() == true` — scheduler logs only
- No keystore creation in T7 scope (deferred to T10 runbook)

**Validation**:
- If `execute_mode == "live"` AND `worker_keystore_dir` is empty or has no valid keystores → error on boot
- If `execute_mode == "live"` AND `treasury_keystore` is empty or invalid → error on boot
- If `execute_mode == "shadow"` AND keystores are missing → warn, create shadow registry

**Test cases** (in `core/tests/signer_registry_test.rs` or inline `#[cfg(test)]`):
1. Load keystores from temp dir → all addresses resolvable
2. Load with missing dir in shadow → returns empty registry, no error
3. Load with missing dir in live → returns `Err(ConfigError(...))`
4. Per-address nonce isolation: two ManagedSigners with different addresses have independent nonce counters
5. `next_nonce()` atomic increments correctly under concurrent access
6. `sync_from_chain` resets local nonce to on-chain value

---

### 3.2 Sweep Scheduler

**Module**: `core/src/sweep_scheduler.rs`
**Public API**:

```rust
pub struct SweepScheduler<P> {
    registry: Arc<SignerRegistry>,
    provider: Arc<P>,
    config: PacingConfig,
    chain_id: u64,
}

impl<P: Provider<Ethereum> + Clone + Send + Sync + 'static> SweepScheduler<P> {
    pub fn new(
        registry: Arc<SignerRegistry>,
        provider: Arc<P>,
        config: PacingConfig,
        chain_id: u64,
    ) -> Self;

    /// Run the sweep loop. Never returns (intended as a tokio task).
    /// On each tick:
    /// 1. Check breaker (skip if active)
    /// 2. For each worker: query native balance, compute spendable = balance - gas_cost - min_keep
    /// 3. If spendable > 0, build and (in live) broadcast tx
    /// 4. Nonce-safe: sync nonce from chain at loop start, local increment per tx
    pub async fn run(&self) -> !;
}
```

**Sweep algorithm** (mirrors `scripts/sweep_profits.py`):
1. Fetch `gas_price` from provider (once per loop iteration)
2. For each worker in `registry.worker_addresses()`:
   a. Query `provider.get_balance(worker)` → `balance`
   b. Compute `gas_cost = 21000 * gas_price`
   c. Compute `min_keep_wei` = `web3.to_wei(config.min_worker_balance_eth)` (via Decimal conversion to wei)
   d. `spendable = balance - min_keep_wei - gas_cost`
   e. If `spendable <= 0`: skip this worker
   f. Get `treasury = config.treasury_address`
   g. If shadow: log `[sweep-shadow] worker -> treasury: {spendable} wei native`
   h. If live: get signer from registry, build `BuiltTransaction { to: treasury, value: spendable, gas: 21000, ... }`, submit via `RpcSubmitter`
3. ERC20 sweep (per configured token list — initial T7 scope: native ETH only; ERC20 deferred to T7.1 unless explicitly requested):
   - Query `balanceOf(worker)` for each token
   - Build `transfer(treasury, balance)` transaction
   - USDT-safe: do NOT assert on `transfer()` return value; validate via receipt.status

**Breaker integration**:
- At the top of each loop iteration: call `pacing_engine.is_breaker_active()`
- If active → log `"Sweep skipped: breaker active"` → sleep → retry

**Pacing integration**:
- Sweep does NOT go through `CrossProcessPacing` (sweep/refund is treasury management, not liquidation pacing)
- Sweep does NOT count against daily/weekly caps (these are profit caps, not internal transfers)
- Sweep DOES honor the breaker (no internal fund movement during emergencies)

**Nonce safety** (mirrors `scripts/fund_eoa.py` line 131-132):
- At loop start: call `managed_signer.sync_from_chain()` to reset local nonce to on-chain
- Per tx: `nonce = managed_signer.next_nonce()` (local increment, no per-tx RPC call)
- This avoids the classic nonce-collision bug where sequential `get_transaction_count()` calls return same nonce

**ERC20 token list** — source of tokens to sweep:
- Initial T7: read from a new config field `sweep_tokens` (list of ERC20 addresses) in `pacing.yaml`
- Default: empty list (native ETH only)
- Future T7.1: auto-discover from active venues in routing.yaml + worker balances

**Test cases**:
1. Shadow sweep: no broadcast, correct calldata logged
2. Live sweep: signer used, tx broadcast, receipt confirmed
3. Breaker active → sweep skipped
4. Worker below threshold → sweep skipped (nothing to sweep)
5. Worker above threshold → correct spendable computed (balance - min_keep - gas)
6. Nonce increments correctly across multiple workers
7. Nonce resyncs from chain at loop start

---

### 3.3 Refund Scheduler

**Module**: same `core/src/sweep_scheduler.rs` (or `core/src/refund_scheduler.rs`)

**Refund algorithm** (mirrors `scripts/fund_eoa.py`):
1. Fetch `gas_price` from provider (once per loop iteration)
2. Compute `min_balance_wei` = `config.min_worker_balance_eth` in wei
3. Compute `fund_wei` = `config.refund_topup_eth` in wei
4. For each worker in `registry.worker_addresses()`:
   a. Query `provider.get_balance(worker)` → `balance`
   b. If `balance >= min_balance_wei`: skip
   c. Get `treasury_address` from config
   d. If shadow: log `[refund-shadow] treasury -> worker: {fund_wei} wei`
   e. If live: get treasury signer from registry, build `BuiltTransaction { to: worker, value: fund_wei, gas: 21000, ... }`, submit
5. Nonce-safe: fetch treasury nonce once at loop start, local increment per tx

**Breaker integration**: Same as sweep — skip if breaker active.

**Treasury balance guard**:
- Before sending refunds, query treasury balance
- If treasury balance < (number_of_underfunded_workers * fund_wei * 2) → log warning, skip refund
- This prevents the treasury from being drained by refund loops

**Test cases**:
1. Shadow refund: no broadcast, correct calldata logged
2. Live refund: treasury signer used, tx broadcast
3. Breaker active → refund skipped
4. Worker above min → skipped
5. Worker below min → funded with correct amount
6. Treasury too low → refunds skipped with warning
7. Nonce-safe: three under-funded workers → nonce 5,6,7 (if starting nonce=5)

---

### 3.4 Wiring in `main.rs`

**Changes to main.rs**:

```rust
// After orchestrator construction, before orchestrator.run():

// (h) Build SignerRegistry from pacing config
let signer_registry = Arc::new(
    SignerRegistry::load(&pacing_cfg, &execute_mode)?
);

// (i) Spawn sweep/refund scheduler task (if not shadow-only with empty registry)
if !signer_registry.is_empty() || execute_mode == "shadow" {
    let sweep_provider = Arc::new(ProviderBuilder::new().connect_http(rpc_url.clone()));
    let scheduler = SweepScheduler::new(
        signer_registry.clone(),
        sweep_provider,
        pacing_cfg.clone(),
        chain_id,
    );
    tokio::spawn(async move {
        scheduler.run().await;
    });
    info!("Sweep/refund scheduler spawned");
}

// Existing: orchestrator.run().await
```

**Provider sharing**: The sweep/refund scheduler uses its own read-only provider (same RPC URL). In shadow mode this is fine. In live mode, the scheduler needs a wallet-backed provider for the treasury + each worker. This means either:
- A) The scheduler creates its own wallet-backed provider per signer, OR
- B) The scheduler uses a read-only provider + signs transactions locally using the `ManagedSigner` directly (not through the provider's wallet)

**Decision**: Option B is cleaner. The scheduler builds `BuiltTransaction`, signs using `ManagedSigner.signer.sign_transaction()`, and sends raw tx via the read-only provider. This matches how `sweep_profits.py` works (it calls `w3.eth.send_raw_transaction(raw)` after `acct.sign_transaction(tx)`).

This requires exposing `sign_transaction` on `ManagedSigner` or adding a method:
```rust
impl ManagedSigner {
    pub fn sign_transaction(&self, tx: BuiltTransaction) -> Result<Bytes, ChimeraError>;
}
```

---

## 4. Config Changes

### 4.1 Existing fields (already in `pacing.yaml` and `config.rs` defaults)

| Field | Default | Used by |
|-------|---------|---------|
| `treasury_address` | `""` | Sweep destination + refund source |
| `treasury_keystore` | `""` | SignerRegistry (treasury signer) |
| `worker_keystore_dir` | `""` | SignerRegistry (worker signers) |
| `sweep_interval_secs` | `300` | SweepScheduler tick interval |
| `refund_interval_secs` | `3600` | RefundScheduler tick interval |
| `min_worker_balance_eth` | `0.01` | Minimum balance threshold (Decimal ETH) |
| `refund_topup_eth` | `0.05` | Amount to send per under-funded worker (Decimal ETH) |

### 4.2 New fields to add

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `sweep_tokens` | `Vec<String>` | `[]` | ERC20 addresses to sweep (T7: native only; T7.1+: multi-token) |
| `sweep_min_keep_eth` | `Decimal` | `0.005` | Native ETH to leave in worker for gas (separate from min_worker_balance_eth) |

**Addition to `config/pacing.yaml`**:
```yaml
sweep_tokens: []              # ERC20 addresses to sweep (T7 native-only)
sweep_min_keep_eth: 0.005     # ETH left in worker for gas after sweep
```

**Addition to `PacingConfig`** (`core/src/config.rs`):
```rust
#[serde(default)]
pub sweep_tokens: Vec<String>,

#[serde(default = "default_sweep_min_keep_eth")]
pub sweep_min_keep_eth: Decimal,
```

**Default function**:
```rust
fn default_sweep_min_keep_eth() -> Decimal {
    Decimal::from_str("0.005").expect("valid literal")
}
```

**Config sync**: `core/tests/config_sync_test.rs` must be updated to assert new fields match between YAML and Rust defaults.

---

## 5. Non-Functional Requirements

### 5.1 Safety invariants
- Sweep/refund MUST NOT execute if breaker is active (`pacing_engine.is_breaker_active()`)
- Sweep/refund MUST NOT broadcast in shadow mode (log only, zero side effects)
- Treasury private key MUST NEVER be logged (only address is logged)
- Worker private keys MUST NEVER be logged (only addresses are logged)
- Live mode MUST refuse to start without valid keystores for all configured EOAs

### 5.2 Performance
- Sweep loop: lightweight — at most N balance queries + N potential transactions per tick
- Refund loop: lightweight — same pattern
- Default intervals (5min sweep, 60min refund) keep RPC load negligible
- No concurrency between sweep and refund: they share the same tokio task, sequential

### 5.3 Observability
- Metrics: `chimera_sweep_total` (counter), `chimera_refund_total` (counter), `chimera_sweep_skipped_breaker` (counter)
- Logs: `[sweep-shadow]`, `[refund-shadow]`, `[sweep-live]`, `[refund-live]` with addresses and amounts (never keys)
- Existing `metrics.rs` gauges for EOA balances can be updated by the scheduler

### 5.4 Error handling
- Per-worker isolation: a failing worker sweep/refund does not block other workers
- RPC errors: retry with exponential backoff (reuse RpcSubmitter patterns)
- Gas estimation failures: fall back to 21000 for native transfers, 120000 for ERC20
- Nonce desync: `sync_from_chain()` at loop start resets to ground truth
- Treasury dry-up: log warning, skip refund, do not panic

---

## 6. Test Plan

### 6.1 Unit tests (Rust)
- `SignerRegistry::load` with valid keystores → all addresses resolvable
- `SignerRegistry::load` with missing dir + shadow → empty registry, Ok
- `SignerRegistry::load` with missing dir + live → Err
- `ManagedSigner::next_nonce` atomic increment correctness
- `ManagedSigner::sync_from_chain` mock
- `SweepScheduler::compute_spendable` edge cases (zero balance, negative spendable, gas > balance)
- `RefundScheduler::needs_refund` edge cases (at threshold, below, above)
- Decimal→wei conversion correctness (`min_worker_balance_eth`, `refund_topup_eth`, `sweep_min_keep_eth`)

### 6.2 Integration tests
- Shadow sweep loop: spawn scheduler with empty registry, verify logs contain `[sweep-shadow]` with correct format
- Breaker integration: trip breaker, verify sweep/refund skip with log message
- Nonce isolation: two workers, verify nonces are independent

### 6.3 Config sync test
- `core/tests/config_sync_test.rs`: assert new fields (`sweep_tokens`, `sweep_min_keep_eth`) match between `pacing.yaml` and `PacingConfig::default()`

### 6.4 Python script compatibility
- Existing `scripts/sweep_profits.py` and `scripts/fund_eoa.py` remain functional and unchanged
- T7 does NOT remove the Python scripts — they remain as operational fallback tooling

---

## 7. Out of Scope (explicitly)

| Item | Reason |
|------|--------|
| Keystore creation / credential management | Deferred to T10 (Phase 6 runbook) |
| ERC20 sweep in scheduler | Deferred to T7.1 (native ETH only in initial T7) |
| Auto-discovery of sweep tokens from venues | Deferred to T7.1 |
| Treasury multi-sig signing | Existing single-key model preserved |
| Cross-chain sweep/refund | Single-chain scheduler (chain_id from pacing config) |
| Sweep/refund through CrossProcessPacing | Treasury management, not liquidation pacing |
| Removing Python scripts | Scripts remain as operational fallback |

---

## 8. Files Changed

| File | Change |
|------|--------|
| `core/src/signer_registry/mod.rs` | **New**: SignerRegistry + ManagedSigner |
| `core/src/sweep_scheduler.rs` | **New**: SweepScheduler + RefundScheduler |
| `core/src/lib.rs` | Add `pub mod signer_registry; pub mod sweep_scheduler;` + re-exports |
| `core/src/config.rs` | Add `sweep_tokens`, `sweep_min_keep_eth` fields + defaults |
| `core/src/main.rs` | Load SignerRegistry, spawn scheduler tokio task |
| `config/pacing.yaml` | Add `sweep_tokens`, `sweep_min_keep_eth` |
| `core/tests/config_sync_test.rs` | Add assertions for new fields |
| `docs/snapshot-schema.md` | No change (unaffected) |
| `scripts/sweep_profits.py` | No change |
| `scripts/fund_eoa.py` | No change |

---

## 9. Acceptance Criteria (from original ticket, refined)

1. **Scheduler builds correct sweep/re-fund txs in shadow** — dry-run, no broadcast, logs contain: worker address, treasury address, amount in wei, nonce, gas price. Respects `sweep_min_keep_eth` and `min_worker_balance_eth` thresholds.

2. **SweepScheduler honors breaker** — when `pacing_engine.is_breaker_active()` returns true, sweep and refund loops skip with a log message. Meter counter `chimera_sweep_skipped_breaker` increments.

3. **SignerRegistry resolves signers by address with isolated nonces** — `registry.get_signer(&worker_addr)` returns a `ManagedSigner` whose `next_nonce()` is independent from other signers. Two workers signing in sequence: worker A nonce 5, worker B nonce 7 (each from their own on-chain starting points).

4. **Live mode refuses to start without keystores** — `SignerRegistry::load(cfg, "live")` returns `Err(ChimeraError::ConfigError("..."))` if `worker_keystore_dir` is empty or `treasury_keystore` is empty/missing.

5. **Nonce safety mirrors Python semantics** — nonce fetched once at loop start via `sync_from_chain()`, local `next_nonce()` increment per tx. No per-tx RPC nonce fetch.

6. **Config sync invariant maintained** — `cargo test -p chimera-core` passes, including the config_sync_test asserting parity between `pacing.yaml` defaults and `PacingConfig::default()`.
