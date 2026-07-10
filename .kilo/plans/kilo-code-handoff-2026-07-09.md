# Project Chimera — Completion Re-Assessment & Kilo Code / Traycer Handoff

> **Analysis date:** 2026-07-09
> **Supersedes:** `.kilo/plans/1782916617990-chimera-completion-analysis-traycer-handoff.md` (2026-07-01, ~58%)
> **Definition of 100%:** Fully autonomous, actively generating revenue, ready for liquidation — strictly fulfilling all requirements in README.md.

---

## Part 1: What Changed Since the July 1 Assessment

Every Tier-1 compile blocker and four of five Tier-2 functional gaps from the
July 1 analysis are **fixed and verified** in the current tree:

| July-1 blocker | Status | Evidence |
|---|---|---|
| Async trait object safety | **FIXED** | `async-trait` in `core/Cargo.toml:17`, applied to all 4 shared traits |
| Missing `#[sol(rpc)]` on Alloy interfaces | **FIXED** | `oracle/chainlink.rs:21`, `oracle/aave.rs:26`, `simulator/mod.rs:37` |
| forge-std not installed | **FIXED** | Pinned submodule (v1.16.1, `.gitmodules` + `foundry.lock`); run `git submodule update --init` |
| DEX swap calldata not wired / hardcoded "aerodrome" | **FIXED** (V2 scope) | `RoutingResolver` + `StrategyAssembler` build real flashLoanSimple calldata; venue comes from resolved route (`orchestrator.rs:451`). V3/Aerodrome-native routing still deferred (`resolver.rs:7`) |
| Snapshot ↔ prewarm zero placeholders | **FIXED** | 27-field parity, u128-as-string deserializer (`prewarm.rs:42-71,127-143`), guarded by `core/tests/snapshot_roundtrip_test.rs` |
| Hardcoded $1800 ETH price | **FIXED** | Chainlink ETH/USD wired into pacing (`main.rs:336-338`, refresh at `orchestrator.rs:146`); $1800 is fallback-only |
| Mempool module empty | **FIXED** (block-trigger scope) | 416 lines: `BlockWatch` WS newHeads + `SequencerFeed` fallback, wired at `orchestrator.rs:176-226`. True pending-tx feed still a pluggable stub (`feed.rs:19-20`) |
| Grafana dashboard missing | **FIXED** (content) | `monitoring/grafana/dashboards/chimera-overview.json` (10 panels); see provisioning-envelope bug below |

**Regression (now repaired):** CI on `main` was green on 2026-06-30 (commit
`90b9329`). The dead-code cleanup `51a368a` and HEAD `1a64390` broke all three
build-and-test CI jobs. Repaired on branch
`claude/repo-completion-next-steps-gfezgl`:

- Restored `CrashRecovery` import in `core/tests/shadow_e2e_test.rs` (E0433).
- Removed stray `}` at `contracts/test/Executor.t.sol:331` that closed the
  contract early and orphaned 11 tests + the final brace.
- Replaced illegal `bytes memory` index-range slices with the `_slice` helper.
- Declared the missing `SEL_OWNER/SEL_SET_POOL/SEL_WITHDRAW/SEL_TRANSFER_OWNERSHIP/ERR_INVALID_POOL`
  constants (values taken from `src/Executor.yul`).
- Fixed the shadow e2e race: `record_outcome` fire-and-forget JSONL append vs.
  synchronous assertion on the current-thread test runtime.
- Corrected 4 stale/malformed Aave defaults in `core/src/main.rs`
  `resolve_aave_addresses` to mirror `config/pools.toml` (Base pool was
  39 hex chars → parsed as `Address::ZERO` at boot).

**Validated in this pass (2026-07-09):** `cargo test --workspace` = 169 passed / 0 failed;
`python3 -m compileall scripts ai-audit/scripts` exit 0; all `.t.sol`/`.s.sol`
files type-check under solc 0.8.26; `Executor.yul` compiles under
`solc --strict-assembly --evm-version cancun`. `forge test` (runtime) could not
be run in this environment (network-restricted); it runs in CI on PR to main.

## Part 2: Updated Completion Assessment

| Layer | Jul 1 | **Jul 9** | Notes |
|-------|------|------|-------|
| Rust core engine (structure) | ~80% | **~90%** | All modules real; mempool implemented (block-trigger) |
| Rust core engine (validated) | ~15% | **~85%** | 169 tests green incl. 11-test shadow e2e loop |
| Yul contracts (code) | ~75% | **~80%** | Executor.yul coherent & compiles; selector + self-init issues below |
| Yul contracts (validated) | ~20% | **~50%** | solc-validated; forge runtime pass still needs CI/workstation |
| Python scripts | ~85% | **~90%** | + wallet provisioning (`provision_wallets.py`, `testnet_harness.py`, 15 pytest tests) |
| Config & data contracts | ~70% | **~85%** | 3-way pacing sync test green; snapshot contract closed |
| Monitoring | ~40% | **~70%** | Dashboard exists; compose wiring bugs below |
| CI/CD | ~60% | **~75%** | Was green 6/30; repaired; fmt/clippy/pytest jobs missing |
| Live revenue generation | 0% | **0%** | No testnet deploy, no soak, no keystore, no real EOAs |
| **Overall (weighted)** | **~58%** | **~72%** | Code ~88%, validated ~65%, operational 0% |

## Part 3: Confirmed Remaining Defects (verified 2026-07-09, file:line)

### A. Contracts (need Foundry locally or CI loop)
1. **`setPool` selector is wrong**: Yul/tests/Deploy.s.sol all use `0xa51b62c1`,
   but `keccak256("setPool(address)")[0:4] = 0x4437152a`. Internally consistent
   (tests pass), but any ABI-encoding caller — **including the multisig that
   Deploy.s.sol §115-119 instructs to call `setPool`** — hits the fallback and
   reverts. Decide: canonicalize all selectors (recommended) or document the
   non-standard ABI and give the multisig raw calldata.
2. **5 of 6 custom error selectors don't match their claimed signatures**
   (`Executor.yul:51-56`; only `Unauthorized()=0x82b42900` is correct). Same
   decision as above; affects explorers/tooling/ABI decoders.
3. **EIP-7702 worker self-init advertised but not implemented**: README and
   `testEIP7702WorkerSelfInitSucceeds` (`Executor.t.sol`) expect a worker
   self-call to `setPool` to succeed when owner-slot is unset, but the
   dispatcher (`Executor.yul:188-195`) only allows `caller()==sload(0)` →
   etched workers are unusable and the test fails at runtime. Implement a
   strictly-gated self-init (`slot0==0 && caller()==address()`) or drop the
   worker-as-executor claim from README + tests.
4. **Profit gate omits the flash-loan premium** (`Executor.yul:155-166`): gate
   checks `balanceAfter > balanceBefore + minProfit + tip` but Aave pulls
   `amount+premium` after return → realized profit can undershoot `minProfit`
   by up to the premium. Add premium to the gate.
5. `vm.expectRevert` + low-level `.call` semantics in
   `testEIP7702ArbitraryCallerCannotClaim` (`assertFalse(ok)` after
   expectRevert is a known Foundry footgun) — review once forge runs.

### B. Rust core
6. **`resolve_aave_addresses` silently degrades to `Address::ZERO`**
   (`main.rs:498`) — literals now corrected, but parse failures should fail
   loudly at boot (or read `config/pools.toml` via existing
   `pools_toml_path`), plus a boot assertion `pool != Address::ZERO`.
7. **`PacingEngine::load_eoa_pool` ignores the `excluded` flag**
   (`pacing_engine.rs:428-458`) — excluded wallets still enter rotation;
   `SignerRegistry` honors it (`signer_registry/mod.rs:218-219`) → drift.
8. **Non-WETH debt assets run with `min_profit = 0`** in live mode
   (`orchestrator.rs:625-656`): USD→token conversion only for WETH; all
   configured pairs have stable-coin debt → the on-chain profit gate is
   effectively disabled for every configured pair. Wire debt-asset oracle
   pricing before live.
9. **`aerodrome-base` venue misconfigured** (`config/routing.yaml:16-27`):
   declared `router_compatibility: "v2"` with `0x420DD381...40Da`, which is
   Aerodrome's PoolFactory, not a router, and Aerodrome's router is not
   V2-ABI-compatible. Replace with a genuine V2 router on Base or implement
   Aerodrome-native routing.

### C. Python / ops
10. **CRITICAL C-1 (from `.kilo/plans/wallet-provisioning-handoff.md`)**:
    testnet chain guard compares against the operator-editable config, not a
    hardcoded `{84532}` allowlist (`testnet_harness.py:101-147`) — a config
    pointed at mainnet passes and `fund` would move real funds.
11. Wallet-provisioning W-1…W-4 warnings from the same handoff doc.
12. Live snapshots not simulation-grade: `price_usd=0.0`, `id=0`, warn-only
    validation (`snapshot_generator.py:451,470-472,607-608`).
13. One pytest fails on web3-less machines (chain-guard exit-code expectation,
    `tests/test_testnet_harness.py`).

### D. CI / monitoring / docs
14. CI lacks `cargo fmt --check`, `clippy -D warnings`, and doesn't run the
    new `tests/` pytest suite; CI only triggers on push/PR to `main`.
15. `docker-compose.yml:36` doesn't mount `alert.rules.yml` (Prometheus
    container fails at startup); no alertmanager service; Grafana dashboard
    JSON wrapped in HTTP-API envelope so file provisioning can't load it.
16. `README.md:35` links deleted `PHASE6_VALIDATION_GATE.md`.

### E. Operational (unchanged, human-gated)
17. Placeholder EOAs (`config/eoa_pool.json` — by design; run
    `scripts/provision_wallets.py`), no keystore, no multisig, no testnet
    deploy, no 7-day soak, no live transaction ever submitted.

## Part 4: Paste-Ready Prompt (Kilo Code Orchestrator/Architect mode, or Traycer Phases mode)

```
You are continuing Project Chimera — a sovereign autonomous Aave V3 liquidation
engine (Rust core + Yul contracts + Python ops) targeting Base and Arbitrum.

START by reading, in order:
1. .kilo/plans/kilo-code-handoff-2026-07-09.md  (current state, verified defect list)
2. AGENTS.md  (invariants — never violate)
3. .kilo/plans/wallet-provisioning-handoff.md  (C-1 CRITICAL + W-1..W-4)

CURRENT STATE: branch claude/repo-completion-next-steps-gfezgl has the full
Rust suite green (169 tests) and all Solidity/Yul compiling under solc 0.8.26.
forge test has NOT yet run end-to-end. Work from that branch.

INVARIANTS: pacing.yaml ↔ config.rs defaults ↔ valid_yaml() fixture must stay
in sync (config_sync_test.rs enforces); Decimal not f64 for money; Cancun EVM
only; scripts importable without web3; every Executor.yul change needs a
Foundry test; execute_mode stays "shadow" in committed config (shadow-guard CI
job enforces); never auto-flip to live.

PHASE 1 — Make forge test green (validation gate: forge test --root contracts -vvv exits 0)
  a. git submodule update --init  (forge-std v1.16.1 is pinned)
  b. forge test; fix runtime failures. Known: testEIP7702WorkerSelfInitSucceeds
     expects a self-init path Executor.yul does not implement (dispatcher case
     0xa51b62c1 only allows caller()==sload(0)). DECISION REQUIRED: implement
     strictly-gated self-init (only when slot0==0 AND caller()==address()) OR
     delete the worker-as-executor claim from README + both EIP-7702 tests.
     Also review vm.expectRevert + low-level-call assertions (assertFalse(ok)
     after expectRevert is inverted in Foundry).
PHASE 2 — Selector canonicalization (gate: forge test green + new selector test)
  keccak-derived selectors: setPool(address)=0x4437152a, owner()=0x8da5cb5b,
  withdraw(address,uint256)=0xf3fef3a3, transferOwnership(address)=0xf2fde38b;
  errors: ProfitGateFailed()=0x9b89663c, AtomicFail()=0xc4cae92f,
  InvalidDexRouter()=0xd7c4b506, InvalidPool()=0x2083cd40,
  WithdrawFailed()=0x750b219c, Unauthorized()=0x82b42900 (already right).
  Update Executor.yul + Executor.t.sol + Deploy.s.sol together; add a Foundry
  test asserting selectors equal bytes4(keccak256(sig)).
PHASE 3 — Funds-path correctness (gate: cargo test + forge test green)
  a. Profit gate must include flash premium (Executor.yul:155-166).
  b. Wire debt-asset USD pricing so non-WETH pairs get a real min_profit
     (orchestrator.rs:625-656); refuse live submission when min_profit==0.
  c. Make resolve_aave_addresses fail loudly / read config/pools.toml; assert
     pool != Address::ZERO at boot (main.rs:495-530).
  d. Honor "excluded" in PacingEngine::load_eoa_pool (pacing_engine.rs:428).
  e. Fix aerodrome-base venue in config/routing.yaml (0x420DD381...40Da is the
     PoolFactory; Aerodrome router is not V2-compatible — substitute a real V2
     router on Base or implement Aerodrome routing).
PHASE 4 — Ops hardening (gate: pytest 15/15 with AND without web3; compose up clean)
  a. C-1: pin testnet chain guard to hardcoded {84532} in testnet_harness.py
     (load_testnet_config AND make_guarded_web3) + regression test.
  b. W-1..W-4 from wallet-provisioning-handoff.md.
  c. Live snapshot: real oracle price_usd + reserve id, make validation fail
     (not warn) on zero prices (snapshot_generator.py).
  d. Mount alert.rules.yml in docker-compose; unwrap Grafana dashboard JSON
     for file provisioning; fix README PHASE6_VALIDATION_GATE.md reference.
  e. CI: add cargo fmt --check, clippy -D warnings, pytest tests/ jobs.
PHASE 5 — Testnet deploy (HUMAN GATES: RPC keys, funded deployer, multisig)
  Run scripts/provision_wallets.py for Base Sepolia; deploy via Deploy.s.sol;
  verify owner==multisig; smoke-test setPool via ABI encoding (this validates
  Phase 2); follow docs/runbook-testnet-deploy.md.
PHASE 6 — 7-day shadow soak, then go-live per docs/runbook-7day-soak.md and
  docs/runbook-keystore-multisig-go-live.md (HUMAN GATES throughout).

Work Phases 1-4 autonomously; stop for human input at Phase 5. Run the phase
validation gate before moving on; do not skip ahead on a red gate.
```

## Part 5: Summary for the Operator

| Metric | Jul 1 | **Jul 9** |
|--------|-------|-----------|
| Code completion (all layers written) | ~75% | **~88%** |
| Validated completion (tests passing) | ~15% | **~65%** |
| Revenue-generating (live, autonomous) | 0% | **0%** |
| **Overall weighted** | **~58%** | **~72%** |

The remaining 28% is dominated by: forge runtime validation + the contract
selector/self-init decisions (Phases 1-2), funds-path economics (Phase 3),
and the human-gated operational ladder (testnet → soak → keystore/multisig →
live) that no coding agent can do alone.
