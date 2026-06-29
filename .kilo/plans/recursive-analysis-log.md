# Project Chimera — Recursive Analysis Log

> This document is the audit trail for the recursive R1-R6 analysis applied to drive Project Chimera to 100% funds-usable.
> Every refinement, rejected hypothesis, and adversary scenario is logged here with timestamp, layer, leaf, root-cause, change-applied, and evidence.

---

## Wave 0 — DISCOVERY (2026-06-29T11:13Z)

**Objective:** Verify or invalidate prior audit findings against actual codebase state.

### L1 — Contract Layer

| Leaf | Root Cause | Severity | Status |
|------|-----------|----------|--------|
| Executor.yul structural corruption | Duplicate content from bad merge/copy-paste; lines 1-372 are orphaned code outside `object` wrappers | CRITICAL | CONFIRMED |
| `exec(bytes)` no caller auth | Design assumed caller arrangement was sufficient; no owner check exists | CRITICAL | CONFIRMED |
| No withdrawal function | Never implemented; ERC20 tokens stuck permanently | CRITICAL | CONFIRMED |
| Profit-gate overflow | `add(add(balanceBefore, minProfit), tip)` uses unchecked Yul `add`; near-max values wrap to bypass check | HIGH | CONFIRMED |
| `executeOperation` no pool validation | `caller()` used as pool without validation against stored address | HIGH | CONFIRMED |
| FundDistributor no reentrancy guard | External `.call{value}` in loop without mutex | MEDIUM | CONFIRMED |
| `run_forge_tests.sh` corrupted | Windows path mangling in bash script | MEDIUM | CONFIRMED |

### L2 — Runtime Layer

| Leaf | Root Cause | Severity | Status |
|------|-----------|----------|--------|
| `MarketSnapshot::default()` placeholder | main.rs has TODO comment; real snapshot loading never wired | CRITICAL | CONFIRMED |
| No LiquidationSimulator instantiated | Types exist but never constructed in main.rs or orchestrator.rs | HIGH | CONFIRMED |
| No signer/wallet | No `alloy-signer-local` crate in Cargo.toml; provider created without wallet | CRITICAL | CONFIRMED |
| No CalldataBuilder usage | Fully implemented in `executor/builder.rs` but never instantiated | HIGH | CONFIRMED |
| No RpcSubmitter usage | Fully implemented in `executor/submitter.rs` but never instantiated; no signer to attach | HIGH | CONFIRMED |
| `validate_mode_transition()` dead code | Called only in tests, never at startup | HIGH | CONFIRMED |
| JsonlPersistence not wired | `PacingEngine::with_state_persistence()` exists but never called from main.rs | HIGH | CONFIRMED |
| No file logging with rotation | Only `tracing_subscriber::fmt()` stdout; no `tracing-appender` | MEDIUM | CONFIRMED |
| Orchestrator uses hardcoded Opportunity | Lines 81-88 of orchestrator.rs hardcode placeholder values | HIGH | CONFIRMED |
| `expected_profit_usd` uses f64 | `SimulationResult` struct in simulator/mod.rs uses f64 for monetary value | MEDIUM | CONFIRMED |
| `RPC_URL` only, no `BASE_RPC_URL` | Generic RPC var; no chain-specific env vars | LOW | CONFIRMED |

### L3 — Operator Tooling

| Leaf | Root Cause | Severity | Status |
|------|-----------|----------|--------|
| `fund_eoa.py` schema mismatch | Reads `"workers"` but pool schema uses `"wallets"` | HIGH | CONFIRMED |
| `fund_eoa.py` nonce collision | `get_transaction_count()` called inside loop; duplicate nonces | HIGH | CONFIRMED |
| `fund_eoa.py` no web3 fallback | Bare `from web3 import Web3` | MEDIUM | CONFIRMED |
| `sweep_profits.py` dead code | No CLI, no ERC20 logic, bare web3 import | HIGH | CONFIRMED |
| `fetch_historical_liquidations.py` no web3 fallback | Bare `from web3 import Web3` | MEDIUM | CONFIRMED |
| 6 scripts missing entirely | `check_balances.py`, `health_check.py`, `dry_run.py`, `toggle_shadow.py`, `recover_state.py`, `rotate_wallet.py` | MEDIUM | CONFIRMED |
| `emergency_pause.py` excellent | Atomic writes, proper CLI, no web3 dep, good error handling | — | VERIFIED GOOD |

### L4 — Guardrails

| Leaf | Root Cause | Severity | Status |
|------|-----------|----------|--------|
| JsonlPersistence not connected to PacingEngine | `with_state_persistence()` never called | HIGH | CONFIRMED |
| Emergency flag not read by binary | No file watcher or reader in main.rs/orchestrator.rs | HIGH | CONFIRMED |
| Metrics gauges not wired | `chimera_daily_net_usd` / `chimera_weekly_net_usd` not connected | MEDIUM | CONFIRMED — partial agent report |
| `validate_mode_transition` dead code | Never called at startup | HIGH | CONFIRMED |

### L5 — Security

| Leaf | Root Cause | Severity | Status |
|------|-----------|----------|--------|
| No threat model document | Never written | HIGH | CONFIRMED |
| No deployment script | `contracts/script/Deploy.s.sol` does not exist | HIGH | CONFIRMED |
| CI has no security scanners | Only functional tests in CI; no `cargo audit`, `slither`, `pip-audit`, `osv-scanner` | HIGH | CONFIRMED |
| `osv-scanner.json` broken output | Committed non-functional scan output | MEDIUM | CONFIRMED |
| RUSTSEC-2024-0437 unresolved | Transitive protobuf DoS (Medium); documented but not fixed | MEDIUM | CONFIRMED |
| No `.env.example` | Never created | LOW | CONFIRMED |
| No `deployment-checklist.md` | Never written | MEDIUM | CONFIRMED |
| No hardcoded secrets | Good — `.gitignore` comprehensive, zero private keys found | — | VERIFIED GOOD |

### L6 — Validation

| Leaf | Root Cause | Severity | Status |
|------|-----------|----------|--------|
| PHASE6 "ALL GREEN" contradicts reality | Test count discrepancy (70 claimed vs 76 actual), but real issue is untested security properties | HIGH | CONFIRMED |
| `run_forge_tests.sh` non-functional | Path corruption | MEDIUM | CONFIRMED |
| No caller-auth tests | `exec(bytes)` has no auth, so no test exists for it | CRITICAL | CONFIRMED |
| No reentrancy tests | No reentrancy guard exists in Yul | CRITICAL | CONFIRMED |
| No overflow protection tests | Bare `add`/`sub` on caller-controlled uint256 | HIGH | CONFIRMED |
| Golden replays are placeholder | `_verified: false`, `_source: "placeholder_for_live_fetch"` | MEDIUM | CONFIRMED |
| README stale | Claims "Phase 1 Complete" but PHASE6 declared 2026-06-17 | LOW | CONFIRMED |

### Invariants Status

| # | Invariant | Status |
|---|-----------|--------|
| 1 | `config/pacing.yaml` ↔ `config.rs` defaults ↔ test fixtures — 3-way sync | ✅ IN SYNC |
| 2 | `docs/snapshot-schema.md` ↔ `ReserveData` struct | ✅ IN SYNC |
| 3 | Monetary values use `Decimal`, not `f64` | ⚠️ VIOLATED — `SimulationResult.expected_profit_usd` uses f64 |
| 4 | EVM target Cancun only | ✅ CONFIRMED |
| 5 | Python scripts importable without web3.py | ⚠️ VIOLATED — 3 scripts have bare imports |
| 6 | All executor changes need Foundry test | ⚠️ PENDING — Wave 1 will add tests |

### Adversary Scenarios Catalogued (Wave 0)

| # | Scenario | Survived by Current Code? |
|---|----------|--------------------------|
| A1 | Attacker calls `exec(bytes)` on a funded Executor to drain tokens | ❌ NO — no auth |
| A2 | Attacker calls `executeOperation` directly, impersonating Aave Pool | ❌ NO — no pool validation |
| A3 | Extreme `minProfit`/`tip` values cause profit-gate overflow | ❌ NO — unchecked arithmetic |
| A4 | Malicious ERC20 callback re-enters `exec` or `executeOperation` | ❌ NO — no mutex |
| A5 | Nonce collision from concurrent `fund_eoa.py` runs | ❌ NO — no nonce management |
| A6 | RPC timeout mid-flash-loan broadcast | ❌ NO — no retry/timeout logic |
| A7 | Restart mid-flash-loan (partial state) | ❌ NO — no crash recovery wired |
| A8 | Malicious DEX router returns extra tokens (donation attack) | ⚠️ Partial — profit gate check exists but overflow makes it unreliable |
| A9 | Empty snapshot (no reserves/users) | ❌ NO — `MarketSnapshot::default()` produces empty state |
| A10 | USDT-style no-return-value `approve` | ✅ YES — `callApprove` checks balance delta |

---

## Wave 1 — STOP-THE-BLEEDING (In Progress)

### Wave 1a — Parallel (P0 contract + test infra)

| Agent | Scope | Files Touched |
|-------|-------|---------------|
| A | Repair Executor.yul: strip corruption, add owner auth, pool validation, withdrawal, overflow protection | `contracts/src/Executor.yul` |
| B | Fix `run_forge_tests.sh` + fix failing Rust tests | `run_forge_tests.sh`, `core/src/**/*.rs` |

### Wave 1b — Foundry Tests (depends on 1a)

| Agent | Scope | Files Touched |
|-------|-------|---------------|
| C | Write Foundry tests for new auth, withdrawal, overflow, pool validation | `contracts/test/Executor.t.sol` |

---

## Wave 1 — STOP-THE-BLEEDING (COMPLETED 2026-06-29)

### Leaves Closed

| Leaf | Root Cause | Change Applied | Evidence |
|------|-----------|----------------|----------|
| Executor.yul corruption | Duplicate orphaned code (lines 1-372) from bad merge | Rewrote clean 444-line single `object "Executor"` | grep: single object at line 33 |
| exec(bytes) no auth | Direct path never gated | Added `eq(caller(), sload(0))` owner gate (line 270) | grep confirmed |
| No withdrawal | Never implemented | Added `withdraw(address,uint256)` (0xf3fef3a3), ERC20+ETH, USDT-safe | lines 100-143 |
| executeOperation no pool check | caller() used as pool unvalidated | Added `eq(caller(), sload(1))` pool gate (line 161) | grep confirmed |
| Profit-gate overflow | Unchecked `add` on minProfit/tip | Added `lt(_sum1, balanceBefore)` overflow guards (lines 243-249, 340-346) | grep confirmed |
| run_forge_tests.sh corrupt | Windows path mangling | Rewrote clean 12-line script | verified |
| Close-factor test fail | `calculate_user_account_data` integer-div→0 for avg LT/HF | `avg_lt = weighted*ray/total_collateral`; `hf = weighted*ray/total_debt` | liquidation.rs:83 |
| recovery test hardcoded /tmp | Unix-only path | `std::env::temp_dir()` | recovery.rs:188 |
| golden test single path | No CWD fallback | `find_golden_replays_path()` 3 candidates | golden.rs:84 |
| No auth/withdraw/overflow tests | Properties were untestable (didn't exist) | 15 Foundry tests (5 updated + 10 new) | Executor.t.sol 382 lines |

### Root-Cause Notes (R2)
- The Executor corruption was SYSTEMIC (bad merge), not a one-line typo — required full rewrite, not patch.
- exec(bytes) lacked auth because the original design assumed "caller arranges balances" was sufficient; the systemic fix was owner-injection via lazy-init slot 0 (EIP-7702 compatible, since constructors don't run under 7702 delegation).
- Close-factor bug was a real PRODUCTION math error (integer division yielding 0 average liquidation threshold), not a test error — fixed the production code, not the assertion.

### Adversary Scenarios Survived (R4)
| ID | Scenario | Mitigation |
|----|----------|-----------|
| A1 | Attacker calls exec(bytes) to drain | owner gate (sload 0) |
| A2 | Attacker impersonates Aave pool | pool gate (sload 1) |
| A3 | Extreme minProfit/tip overflow | overflow guards before threshold |
| A8 | Malicious DEX donation | profit gate intact; overflow path closed |
| A10 | USDT no-return approve/transfer | returndatasize checks in callApprove + withdraw |

### Rejected Hypotheses
- REJECTED: "Patch only the canonical half of Executor.yul and delete the duplicate." Reason: encoding corruption (mojibake) and missing comment delimiters pervaded both halves; a clean rewrite was lower-risk than surgical excision.
- REJECTED (deferred to Wave 5): FundDistributor reentrancy guard — medium severity, not on the funds-accessibility critical path for Wave 1.

### Build Verification Gap
`forge build/test` and `cargo test` could NOT be run (environment denies bash execution). All correctness established by code-level tracing. MUST be verified on a full-toolchain workstation. This is the single largest open risk for Wave 1.

---

## Wave 2 — WIRE THE RUNTIME (COMPLETED 2026-06-29)

### Leaves Closed

| Leaf | Root Cause | Change Applied | Evidence |
|------|-----------|----------------|----------|
| MarketSnapshot::default() placeholder | Real loader never written | `MarketSnapshot::load_from_file` via private `RawSnapshot` serde mirror; graceful empty fallback | liquidation.rs:206 |
| No signer | No keystore wiring, no signer crate | `PrivateKeySigner::decrypt_keystore` from CHIMERA_KEYSTORE_PATH/PASSWORD; live requires it, shadow optional; `signer-keystore` feature added | main.rs:332, Cargo.toml:16 |
| No simulator/oracles/submitter constructed | Types existed but unwired | All constructed in `run_with_provider<P>` | main.rs:172-251 |
| Provider+signer type divergence | wallet vs read-only providers are different concrete types | Generic `run_with_provider<P>` monomorphized per branch | main.rs:153-165 |
| validate_mode_transition dead | Never called at startup | Called at startup with shadow_since from `core/state/mode.json` | main.rs:96 |
| JsonlPersistence unwired | with_state_persistence never called | `PacingEngine::new(cfg).with_state_persistence(JsonlPersistence)` | main.rs:232-237 |
| No file logging | stdout only | `tracing_appender::rolling::daily` + non_blocking, registry layered | main.rs:64-77 |
| RPC_URL vs BASE_RPC_URL | Only RPC_URL read | `resolve_rpc_url`: BASE_RPC_URL/ARB_RPC_URL → RPC_URL → localhost | main.rs:261 |
| Orchestrator placeholder Opportunity + sim | Hardcoded values | Real `simulate_liquidation`, real candidate fields, execute_mode-gated submission, gas==0 rejection | orchestrator.rs |
| SimulationResult f64 (Invariant 3) | expected_profit_usd was f64 | Changed to Decimal; f64 only at metrics boundary | simulator/mod.rs:100 |

### Root-Cause Notes (R2)
- The dual MarketSnapshot types (detector vs prewarm) were the systemic blocker to a real snapshot. Resolved by giving the detector snapshot its own serde mirror (`RawSnapshot`) keyed to snapshot_generator.py output, rather than forcing the two types to merge (which would have rippled across the simulator).
- Provider-type divergence is a classic alloy generics trap: attaching a wallet changes the concrete `FillProvider<...>` type. Solved with monomorphization (`run_with_provider<P>`) instead of boxing into `dyn Provider` (which loses required associated types).

### Adversary Scenarios Survived (R4)
| ID | Scenario | Mitigation |
|----|----------|-----------|
| A6 | RPC timeout mid-broadcast | RpcSubmitter retry/backoff (pre-existing) + simulation gate before submit |
| A9 | Empty snapshot | Graceful fallback to MarketSnapshot::default() = zero candidates, no panic |
| Live-without-signer | Operator sets live but no keystore | `load_signer` hard-errors; refuses to boot live without a signer |
| Secret leakage | Keystore path/password in logs | Only env-var NAME logged, never values |

### Rejected Hypotheses
- REJECTED: Merge detector::MarketSnapshot and prewarm::MarketSnapshot into one type. Reason: too invasive; the simulator's prewarm type has REVM-specific concerns. Separate serde mirror was cleaner and lower-risk.
- REJECTED: Box the provider as `Arc<dyn Provider>`. Reason: LiquidationSimulator/oracles require concrete `P: Provider<Ethereum> + Clone` with associated types; trait-object erasure breaks those bounds. Monomorphization chosen.

### Verified Symbols (de-risking)
`select_next_eoa` (pacing_engine.rs:384), `fetch_nonce` (submitter.rs:70), `ChimeraError::{RpcError,ConfigError}` (error.rs) — all confirmed present, so orchestrator's calls compile.

### Build Verification Gap
Same as Wave 1 — no cargo available. Low-probability residual risks: exact alloy 1.7 method surfaces (`get_gas_price` returns u128, `connect_http`, `ProviderBuilder::wallet`, `decrypt_keystore` arg traits, `B256` Display). All consistent with pre-existing usage but UNVERIFIED by compiler.

---

## Wave 3 — CONNECT THE SAFETY SURFACE (COMPLETED 2026-06-29)

### Leaves Closed

| Leaf | Change | Evidence |
|------|--------|----------|
| `daily_loss_eth` never decays | Rolling 24h `daily_losses` deque, recomputed each outcome (was monotonic `+=`) | pacing_engine.rs |
| `clear_breaker` ungated | Gated behind `CHIMERA_OPERATOR_TOKEN`; returns `Result<(), ChimeraError>` | pacing_engine.rs |
| Emergency flag not read by binary | `read_emergency_flag` + `trip_emergency`; ≤3s sub-interval poll (meets 5s requirement) | orchestrator.rs |
| No daily/weekly USD gauges | `chimera_daily_net_usd` / `chimera_weekly_net_usd` prometheus gauges + setters | metrics.rs |
| Grafana references wrong metric names | Dashboard rewritten — 10 panels, all metric names correct | chimera-overview.json |
| No Alertmanager rules | 5 alert rules (breaker, daily loss, no candidates, high reverts, scraper down) | alert.rules.yml |
| `config_sync_test` only 4 fields | Now asserts all 20 `PacingConfig` fields, 3-way sync, triple-check test | config_sync_test.rs |

### Adversary Scenarios Survived
- `emergency.flag` written → breaker trips within 3s, processing halts, breaker persists until gated operator clear.
- Rogue `clear_breaker` attempt without/with-wrong token → refused with `ConfigError`.
- Cross-day cumulative loss no longer permanently trips `DailyLossLimit` (rolling window).

## Wave 4 — OPERATOR TOOLING (COMPLETED 2026-06-29)

### Fixed
- `fund_eoa.py`: `workers`→`wallets` key, object entries, nonce collision (fetch once, increment per send), web3 fallback.
- `sweep_profits.py`: full CLI, ERC20 + USDT-safe sweep, `--keys-file` (keys never on CLI).
- `fetch_historical_liquidations.py`: web3 fallback.

### Created (6 scripts)
`check_balances.py`, `health_check.py`, `dry_run.py` (never spawns unless shadow-enforced), `toggle_shadow.py` (mirrors the exact 7-day soak the Rust binary enforces, writes `mode.json` schema), `recover_state.py` (mirrors `CrashRecovery`, stdlib-only), `rotate_wallet.py` (docs-compat delegator).

### Docs
README `ai-audit/` framing corrected; phase-status section honest about Waves-1-4 completion, pending toolchain-validation, pending 7-day soak.

## Wave 5 — SECURITY + DEPLOYMENT READINESS (COMPLETED 2026-06-29)

### 5A — Aave V3 Edge Cases (DETECTOR + SIMULATOR)
End-to-end implementation across sync-coupled files:
- `detector::liquidation.rs`: hydrate `active`/`frozen`/`paused`/`is_isolated`/`debt_ceiling`/`e_mode_category` (no longer hardcode 0/false); add `liquidation_protocol_fee_bps`, `siloed_borrowing`, eMode LT/bonus override fields; `RawReserve` serde-mirrors new fields with `#[serde(default)]`; `find_at_risk_positions` excludes frozen/paused/inactive reserves and skips bad-debt; `calculate_user_account_data` applies eMode LT when `user.emode == reserve.e_mode != 0`.
- `simulator/mod.rs`: `parse_protocol_fee` reads config bits 152–167; `extract_profit_from_state` subtracts protocol fee from the bonus portion; 3 new unit tests (`parse_protocol_fee_*`).
- `simulator/prewarm.rs`: pack eMode category (168–175), protocol fee (152–167), siloed (62), borrowableInIsolation (61), debt ceiling (212–251), frozen (57), paused (60) bits into `pack_reserve_configuration_map`.
- `docs/snapshot-schema.md`: 8 new reserve fields documented (active, frozen, paused, siloed_borrowing, liquidation_protocol_fee, emode_category, emode_liquidation_threshold, emode_liquidation_bonus); user-side `is_in_isolation` documented; backward-compat note added.
- `scripts/snapshot_generator.py`: emits new fields in both live + `--mock` paths.
- New `core/tests/aave_edge_cases_test.rs`: 8 tests covering frozen/paused/inactive exclusion, bad-debt skipped, eMode raises LT threshold, isolation handling, siloed flag round-trip, protocol fee reduces profit.

### 5B — Executor Ownership Hardening + Deploy Script
- `Executor.yul`: constructor reads optional appended 32-byte owner arg; `transferOwnership(address)` selector 0xf2fde38b, owner-gated, zero-address rejection (prevents lazy-init re-hijack on future transfer), full backward compat (no-arg deploy still lazy-inits). 476 lines.
- `contracts/test/Executor.t.sol`: 7 new tests (constructor-owner-arg, blocks-lazy-init, transferOwnership only-owner, rejects-zero-address, transferred-owner-calls-gated, no-arg-backward-compat, lazy-init subtlety). Total: 22 Foundry tests.
- `contracts/script/Deploy.s.sol`: `forge script`-compatible; reads `CHIMERA_MULTISIG` (required, must be a contract — checked via `code.length > 0` before AND after deploy), `DEPLOYER_PRIVATE_KEY`, optional `CHIMERA_AAVE_POOL`. Deploys Executor with multisig as constructor owner, deploys FundDistributor with two-step ownership initiation, asserts `owner() == multisig`, logs ACTION REQUIRED for multisig's `setPool` and `acceptOwnership` steps. Testnet-first, never hardcodes addresses/keys.

### 5C — CI + Dependency Audit
- `.github/workflows/ci.yml`: added `cargo-audit`, `slither`, `pip-audit`, `osv-scanner` gating jobs (all fail build on high/critical), plus `shadow-guard` grep gate blocking `execute_mode: live` in committed config/fixtures.
- `.cargo/audit.toml`: RUSTSEC-2024-0437 risk-accepted with documented unreachability rationale (protobuf is only used to encode, never decode).

### 5D — Security Docs
- `docs/threat-model.md`: system overview + trust boundaries, 7 threat actors, STRIDE attack trees mapped to slot-0/1 gates, 9 DeFi attack vectors with mitigation status, residual risks section, top-10 risk matrix.
- `docs/deployment-checklist.md`: pre-flight / testnet deployment / operational setup / mandatory 7-day soak / go-live gate / rollback sections.

## Wave 6 — VALIDATION GATE RE-DERIVATION (DEFERRED)

Wave 6 is the pure-documentation phase: re-derive `PHASE6_VALIDATION_GATE.md` from a clean checkout and verify every claim is reproducible. It was deferred at the credit-cutoff point of this work session for two reasons:
1. It demands a working `cargo test` / `forge test` / `slither` / `cargo audit` / `pip-audit` / `osv-scanner` run — none of which can execute in this environment (bash execution is gated by permissions, and full toolchains are not installed).
2. The document being re-derived claims "ALL GREEN"; producing it without actually-green gates would itself be the same kind of doc/code drift the mission exists to eliminate.

**Required to re-enable Wave 6 on a developer workstation**: run the validation gate from AGENTS.md verbatim on a clean checkout after the Wave-1-5 changes; if green, re-derive PHASE6 with the new test counts (now ~98 Rust tests incl. edge cases + 22 Foundry tests), archive fresh security-scan JSONs in `audits/`, and update the README status to "Phase 6 validated". Until then, PHASE6_VALIDATION_GATE.md remains REVOKED.

### Standings build-verification risks across all waves
No `cargo` / `forge` / `slither` / `cargo audit` / `pip-audit` / `osv-scanner` run has been executed in this environment. All correctness has been established by code-level tracing, symbol verification, and signature matching. The following low-probability risks are flagged for toolchain verification on a developer workstation:
- alloy 1.7 surface: `ProviderBuilder::wallet`, `connect_http`, `get_gas_price` returns `u128`, `PrivateKeySigner::decrypt_keystore` arg traits, `B256` Display.
- alloy signer-keystore feature reachability from `full` (added explicit feature flags defensively).
- Decimal `Sum` over `&(DateTime, Decimal)` iterator in `daily_losses` recompute.
- Forge EIP-7702 test: `vm.etch` with `deployedBytecode.object` for the lazy-init owner test (staticcall-vs-call subtlety).

*Log continues (Wave 6 pending toolchain run)…*
