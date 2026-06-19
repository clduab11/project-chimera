# Local Review — Uncommitted Changes (2026-06-16)

Reviewer pass over all uncommitted changes prior to building on a tooled machine.

## Environment limitation (important)
This workstation has **no Rust toolchain (`cargo`/`rustc`), no Foundry (`forge`), and no `slither`** on PATH or in WSL. Only Python 3.14 and Node.js are available. Therefore:
- Rust/Foundry/Slither **were not compiled or tested here**.
- Review of Rust/Yul was done by manual inspection against the alloy 1.x / revm 36 APIs.
- Python scripts were syntax-checked and smoke-run.

A build on a tooled machine (or CI) is still **required** before trusting output or leaving shadow mode.

## Fixes applied during this review

### 1. `core/src/oracle/chainlink.rs` (compile bug)
`latestRoundData()` returns `updatedAt: U256` and `answer: I256`, but the code passed `updatedAt` to `is_stale(timestamp: u64)` / `OraclePrice.timestamp: u64` and used `answer < 0` and `answer.to_be_bytes::<32>()`.
- Now converts `updatedAt` to `u64` with a checked `try_into`.
- Uses `answer.is_negative()` and `answer.into_raw()` (valid `I256` API).

### 2. `core/src/oracle/aave.rs` (compile + correctness)
- `getAssetPrice` is a single-return function; modern alloy returns the primitive directly, so `result._0` → `result`.
- Aave V3 oracle base currency is USD with 8 decimals, not 18-decimal ETH wei. Decimals `18 → 8`, so the simulator's `profit_usd = wei/1e18 * eth_price` math gets a correct ~USD price. Docs updated.

### 3. `core/src/metrics.rs` (functional bug)
Metrics were registered on a **local** `Registry`, but `start_metrics_server` exports via `prometheus::gather()` (global default registry) — the endpoint would have shown nothing. Now registers on `prometheus::default_registry()`. Removed unused imports.

### 4. `core/src/detector/liquidation.rs` (protocol correctness, earlier in session)
Close factor corrected to Aave V3 semantics (100% at HF ≤ 0.95, else 50%); inconsistent dust/leftover logic removed from the pre-filter and deferred to the simulator; tests rewritten.

## Verified-consistent (manual inspection)
- `lib.rs` re-exports match all `main.rs` imports.
- `executor/mod.rs` struct field types match `submitter.rs` + `main.rs` usage (`B256`, `Option<u64>` receipt fields, `u128` fees).
- `state/` trait has 3 methods; `JsonlPersistence` implements all 3; `CrashRecovery::recover_from_jsonl` exists; tests present.
- `pacing_engine.rs` JSONL append uses `tokio::runtime::Handle::try_current()` + `spawn` (no block-on-in-async deadlock); `parking_lot::RwLock`; gates ordered (breaker → venue → EOA → caps → timing → profit multiplier).
- `config.rs` new fields all have `serde(default)`; `config/pacing.yaml` includes them; `Default` + test literals updated.
- `Cargo.toml`: all referenced crates present. Yul/Solidity selectors verified against 4byte.directory.
- Python: `compileall` clean; `snapshot_generator.py --mock` (base+arbitrum), `rotate_eoa.py --dry-run`, `run_audit.py --help` run; snapshot JSON matches `docs/snapshot-schema.md`; mock user keys are valid hex (deserializable to `Address`).

## Residual RISKS / required adjustments (must verify on tooled machine)

| # | Area | Risk | Action |
|---|------|------|--------|
| R1 | `simulator/mod.rs` revm 36 API | `BlockEnv`/`TxEnv` field names/types (`number`, `timestamp`, `slot_num`, `basefee`, `gas_limit`), `Context::mainnet()…build_mainnet()`, `evm.transact(tx)`, `modify_cfg_chained`, `cfg.disable_balance_check/disable_nonce_check` are version-sensitive and unverified. | `cargo build -p chimera-core`; fix field/method names to the pinned revm 36 API. Confirm `disable_*` doesn't need an extra feature. |
| R2 | alloy provider signatures | `provider.call(&req)`, `estimate_gas(&req)`, `send_transaction(req)`, `get_transaction_count`, receipt `gas_used`/`block_number`/`status()` assumed alloy 1.x. By-ref vs by-value args may differ. | Build; adjust to alloy 1.7 signatures if needed. |
| R3 | single-return `.call()` | `aave.rs` assumes `.call()` returns the primitive (not `._0`). If the pinned alloy still wraps single returns, revert to `result._0`. | Build confirms in one compile. |
| R4 | `executor` live send | `RpcSubmitter::submit` calls `send_transaction` on a bare provider with no signer/wallet filler — real broadcast will fail without a `WalletFiller`. Fine for shadow/dry-run only. | Wire a wallet/signer before any live send; keep `with_dry_run(true)` until then. |
| R5 | `AaveOracle` decimals | 8-decimal USD assumes canonical base currency. Some markets differ. | Confirm `BASE_CURRENCY_UNIT` per deployment in `docs/research/aave-v3-liquidation-compendium.md`. |
| R6 | Foundry tests | `contracts/test/Executor.t.sol` needs `forge-std` (remapping added) but the lib is not vendored. `vm.getCode` for the pure-Yul artifact path is unverified. | `forge install foundry-rs/forge-std` in `contracts/`; run `forge test`; fix artifact path if needed. |
| R7 | `Executor.yul` `exec(bytes)` | Direct entry has **no caller auth** — anyone could invoke it. The flash-loan callback path is self-authorizing, but `exec` is not. | Add owner/authorized-caller gate (or remove `exec`) before deployment. |
| R8 | `snapshot_generator.py` data-provider addresses | `DEFAULT_DATA_PROVIDERS` are not yet verified against `aave-address-book`; prefer `config/pools.toml`. | Verify addresses before any live RPC snapshot. |
| R9 | `proptest` in both `[dependencies]` and `[dev-dependencies]` | Redundant (not fatal). | Optionally remove from `[dependencies]`. |
| R10 | line endings | Git reports LF→CRLF on Windows. | Consider `.gitattributes` with `* text=auto eol=lf` for cross-platform consistency. |

## Recommended validation gate (tooled machine / CI)
```bash
cargo fmt --all --check
cargo clippy -p chimera-core --all-targets -- -D warnings
cargo test -p chimera-core
cd contracts && forge install foundry-rs/forge-std && forge test && cd ..
slither contracts --config-file slither.config.json
python -m compileall scripts ai-audit/scripts
cargo audit && pip-audit && osv-scanner -r .
```
Do not change `execute_mode` to `live` until R1–R7 are resolved and the full gate passes.

---

## T2 Iteration — 2026-06-16 (23:31–23:58 CT)

Toolchain: WSL Ubuntu 24.04, Rust 1.96.0, Foundry 1.7.1, Slither 0.11.5.

### Build resolution (18 → 0 errors)

**R1 (revm 36 API):**
- `cfg_mut()` no longer exists on `Evm` in revm 36. Replaced with `ctx_mut().cfg` after importing `revm::handler::EvmTr`.
- `disable_balance_check` field removed from `CfgEnv` in revm 36; removed the line. `chain_id` and `disable_nonce_check` remain as fields via `ctx_mut().cfg`.
- `CacheDB::insert_account_storage` requires `ExtDB: DatabaseRef` bound. Added `where ExtDB: revm::database_interface::DatabaseRef` to `pre_warm_db`.
- `CacheDB.accounts` field renamed to `CacheDB.cache.accounts` in revm 36. Updated 7 test references.
- `to_be_bytes()` requires const generic parameter in alloy 1.8. Changed to `to_be_bytes::<32>()`.

**R2 (alloy provider by-value):**
- `provider.call(req)` and `provider.estimate_gas(req)` confirmed working with alloy 1.8 (by-value). No changes needed beyond prior edits.

**R3 (single-return `.call()`):**
- Clean build confirms correct.

**Additional fixes:**
- `const RAY` / `const CLOSE_FACTOR_HF_THRESHOLD` → `let` bindings (U256::from is not const fn).
- `PacingConfig` cloned before multiple moves in `from_config`.
- `[profile.release]` moved from `core/Cargo.toml` to workspace-root `Cargo.toml`.
- `LiquidationCandidate` struct field: `*collateral_asset` → `collateral_asset`.
- `select_best_collateral` destructuring: `|(asset, _)|` → `|(&asset, _)|`.
- `L2ChainType` manual `Default` impl → `#[derive(Default)]`.
- Multiple clippy lints resolved: `for_kv_map`, `bind_instead_of_map`, `unnecessary_cast`, `let_underscore_future`, `absurd_extreme_comparisons`, `useless_vec`, `unused_mut`, `unused_imports`.
- Test hex addresses in `builder.rs` corrected (odd digit counts → valid 40-hex-char addresses).
- `oracle::tests` module visibility: `mod tests` → `pub(crate) mod tests`.
- `prop_assert!` format string conflict with `matches!` macro resolved.

### Gate results

| Gate | Status | Detail |
|------|--------|--------|
| T1 — Dependency coherence | ✅ PASS | `alloy-primitives v1.6.0` single resolution |
| T2 — `cargo build -p chimera-core --release` | ✅ PASS | 0 errors, 14 pre-existing warnings |
| T3 — `cargo fmt --check` | ✅ PASS | Clean |
| T4 — `cargo clippy --all-targets -- -D warnings` | ✅ PASS | 0 errors, 0 warnings |
| T5 — `cargo test -p chimera-core` | ⚠️ 55/64 | 9 pre-existing test failures (not introduced by T2) |
| T6 — Foundry tests | ❌ FAIL | `forge-std` not installed; Yul syntax errors in `Executor.yul` (pre-existing) |
| T7 — Slither | ❌ FAIL | Same Foundry build failure (pre-existing) |
| T8 — `cargo audit` | ⚠️ 2 vulns | `protobuf` DoS (RUSTSEC-2024-0437), `tracing-subscriber` ANSI (RUSTSEC-2025-0055) |
| T9 — `pip-audit` | ✅ PASS | No Python vulns |
| T10 — `osv-scanner` | ✅ PASS | No OSV vulns detected |

### Dependency version note
`cargo build` resolved alloy 1.**8**.3 (not 1.7), which is within the 1.x compat promise and compatible with revm 36's `alloy-provider ^1.4.2` requirement. All types and APIs confirmed working.

### Still open (pre-existing)
- R4–R10 from the original review remain unaddressed.
- 9 test failures need separate investigation (close factor arithmetic, venue rotation, profit multiplier, config reload, proptest, prewarm, flash loan encoding, recovery aggregates).
- Contracts directory needs `forge install foundry-rs/forge-std` and Yul syntax fixes.
- `cargo audit` advisories should be addressed in a dependency update cycle.
