# Project Chimera — Completion Analysis & Traycer AI Handoff Prompt

> **Analysis date:** 2026-07-01
> **Definition of 100%:** Fully autonomous, actively generating revenue, ready for liquidation — strictly fulfilling all requirements in README.md.

---

## Part 1: Completion Assessment

### Overall Completion: **~58%**

The project has a comprehensive architecture with substantial code written across all layers (Rust engine, Yul contracts, Python scripts, monitoring, CI, documentation). However, **none of the critical path has been validated end-to-end with a real toolchain**, and the system cannot currently generate revenue. Code exists but is unvalidated and partially uncompiled.

### Completion by Layer

| Layer | Completion | Status |
|-------|-----------|--------|
| **Rust core engine (structure)** | ~80% | All modules written. Orchestrator loop exists. Compile blockers identified but not fixed. |
| **Rust core engine (validated)** | ~15% | `cargo test` has NEVER been run successfully. Multiple compile blockers documented. |
| **Yul contracts (code)** | ~75% | Executor.yul hardened, FundDistributor.sol done, 10+ Foundry tests written. |
| **Yul contracts (validated)** | ~20% | `forge test` has NEVER been run. `forge-std` dependency not installed. Artifact paths unverified. |
| **Python scripts** | ~85% | 14 scripts written. `sweep_profits.py` has 442 lines of real implementation. `fund_eoa.py` reads `wallets` key correctly. |
| **Config & data contracts** | ~70% | pacing.yaml has all fields. But snapshot schema ↔ Rust prewarm mismatch remains. |
| **Monitoring stack** | ~40% | prometheus.yml + alert.rules.yml exist. Grafana dashboard JSON may be missing. Docker compose exists. |
| **CI/CD pipeline** | ~60% | ci.yml exists with shadow-guard job. Never validated on a real GitHub Actions runner. |
| **Documentation** | ~90% | Comprehensive: architecture, deployment-checklist, first-run-onboarding, threat-model, security-research, snapshot-schema, emergency-procedures, operator-manual. |
| **Live revenue generation** | 0% | No shadow soak done. No keystore. No multisig. No real EOAs. No real RPC. No live transactions ever submitted. |
| **Autonomous operation** | 0% | The full find→validate→fire→sweep→re-fund loop has never run end-to-end. |

### What IS Complete

- **Architecture**: Full detect → simulate → pace → execute → sweep → re-fund loop designed and coded
- **Rust modules**: config, detector, executor, oracle, pacing_engine, simulator, state, orchestrator, metrics — all written
- **main.rs**: Full boot sequence with mode-transition gate, signer loading, provider construction, snapshot loading
- **orchestrator.rs**: Continuous detect→simulate→pace→submit loop with emergency polling, gas pre-flight, shadow accounting
- **Contracts**: Executor.yul with owner auth, pool validation, profit gates, overflow guards; FundDistributor.sol
- **Foundry tests**: 10+ tests covering constructor-owner, transferOwnership, pool validation, auth, withdraw paths
- **Pacing engine**: 903 lines — Decimal-based caps, jitter, breaker, EOA/venue rotation, JSONL persistence, crash recovery
- **Python scripts**: 14 scripts including sweep_profits (442 lines), fund_eoa (298 lines), emergency_pause, health_check, toggle_shadow, etc.
- **Config**: pacing.yaml with all required fields (chain_id, oracle_staleness, eoa_pool_path, pools_toml_path, eth_price_usd_fallback)
- **Security**: threat-model.md, deployment-checklist.md, security-research.md, Aave V3 liquidation compendium, dependency CVE triage
- **CI**: ci.yml with cargo test, forge test, compileall, shadow-guard job
- **Documentation**: 13 docs files covering every operational aspect

### What IS NOT Complete (Critical Path Gaps)

#### Tier 1 — Compile/Integration Blockers (Must Fix Before Anything Runs)
1. **Rust compilation never validated** — `cargo test` never run. Known issues: async trait object safety (PriceOracle, StatePersistence, TransactionExecutor traits), potential `#[sol(rpc)]` missing on Alloy interfaces
2. **Foundry tests never validated** — `forge-std` not installed. Test artifact paths for pure Yul unverified
3. **Slither never run** — Static analysis pipeline exists but not exercised

#### Tier 2 — Functional Gaps (Prevent Revenue Generation)
4. **DEX swap path not wired into executor calldata** — The orchestrator hardcodes `venue: "aerodrome"` but the actual swap calldata construction for the Executor.yul flash-loan flow is not connected to the DEX routing from `update_venues.py`
5. **Snapshot ↔ prewarm data contract** — `simulator/prewarm.rs` may still reference zero placeholders; `snapshot_generator.py` serialization gaps for reserve data fields
6. **Multi-chain RPC** — `main.rs` `resolve_rpc_url` now handles `BASE_RPC_URL`/`ARB_RPC_URL` but only for one chain at a time; no chain-switching loop
7. **Mempool module** — Empty placeholder. Competitive timing advantage not realized
8. **Eth price for gas pacing** — Uses a hardcoded fallback ($1800). Oracle-injected price path not wired

#### Tier 3 — Infrastructure (Required Before Live)
9. **7-day shadow soak** — Never performed. Mandatory per README and deployment-checklist
10. **Encrypted keystore** — Not created. Required for live mode
11. **Multisig deployment** — Not done. `Deploy.s.sol` enforces it but no deployment has happened
12. **Real EOA pool** — Still `0x1111...1111` placeholders. Need real Base/Arbitrum EOAs
13. **Real RPC endpoints** — No production Alchemy/Infura keys configured
14. **Grafana dashboard JSON** — Monitoring scaffolding exists but dashboard content unverified
15. **Testnet deployment** — Base Sepolia / Arbitrum Sepolia smoke test not done
16. **Mainnet contract deployment** — Not done

#### Tier 4 — Operational Readiness
17. **End-to-end integration test** — No test that exercises the full loop from snapshot → detection → simulation → pacing → calldata build → (dry-run) submission
18. **Historical validation** — `fetch_historical_liquidations.py` exists but soak comparison never done
19. **Crash recovery test** — `recover_state.py` exists but kill-restart-recover cycle never tested
20. **Emergency drill** — Never performed. 5s detection SLA unverified
21. **Operator runbook validation** — All scripts exist but operator has never followed the full onboarding path

---

## Part 2: Remaining Development Steps (Ordered)

### Phase A: Restore Compile & Test (1–2 sessions)
- A1. Fix async trait object safety: add `async-trait` crate or boxed futures for PriceOracle, StatePersistence, TransactionExecutor
- A2. Add `#[sol(rpc)]` to Alloy contract interfaces
- A3. Install `forge-std` dependency, verify remappings
- A4. Run `cargo test -p chimera-core` — fix all compilation errors
- A5. Run `forge test --root contracts/ -vvv` — fix all test failures
- A6. Run `python -m compileall scripts ai-audit/scripts` — fix any import issues
- A7. Run `slither contracts --config-file slither.config.json` — triage findings
- A8. Verify AGENTS.md invariant #1: pacing.yaml ↔ config.rs defaults ↔ valid_yaml() fixture

### Phase B: Close Functional Gaps (2–3 sessions)
- B1. Wire DEX swap calldata into the Executor submission path (connect `update_venues.py` routing data → calldata construction)
- B2. Fix snapshot ↔ prewarm data contract — add missing fields to `snapshot_generator.py`, update `prewarm.rs` to consume real data
- B3. Wire oracle-based ETH price into pacing engine gas cost estimation (replace/augment hardcoded $1800 fallback)
- B4. Add chain-switching or multi-chain orchestration in main.rs (or document single-chain-per-process model)
- B5. Wire EOA pool loading at startup (`PacingEngine::load_eoa_pool` must parse `wallets[].address`)

### Phase C: Toolchain Validation & Hardening (1 session)
- C1. `cargo clippy --all-targets -- -D warnings`
- C2. `cargo fmt --all --check`
- C3. `cargo audit` — document or fix advisories
- C4. Full CI green on target commit including shadow-guard
- C5. Verify Grafana dashboard auto-provisions correctly

### Phase D: Testnet Deployment (1 session)
- D1. Deploy Executor + FundDistributor to Base Sepolia via `Deploy.s.sol`
- D2. Verify owner == multisig, setPool works, unauthorized callers rejected
- D3. Smoke test one end-to-end flash-loan liquidation on testnet
- D4. Verify contracts on block explorer

### Phase E: Shadow Soak (7+ days unattended)
- E1. Configure real RPC, real EOA pool (public addresses only), real snapshot
- E2. Start shadow mode, verify metrics healthy
- E3. Run 7 continuous days with zero unexpected breaker trips
- E4. Cross-validate simulated profit against `fetch_historical_liquidations.py`
- E5. Test crash-restart recovery
- E6. Perform emergency drill (emergency_pause.py → breaker trip ≤ 5s)

### Phase F: Go-Live (1 session)
- F1. Create encrypted keystore outside repo
- F2. Run `toggle_shadow.py --set-live` (enforces 7-day rule)
- F3. Deploy contracts to mainnet via multisig
- F4. Start with minimum viable worker balances
- F5. Watch first ~10 live ops
- F6. Verify first profit sweep

---

## Part 3: Traycer AI Bootstrap & Handoff Prompt

Below is a comprehensive prompt designed for input into Traycer AI (Epic Mode or Phases Mode) to autonomously execute all remaining development steps. It is structured with Traycer's spec-driven development workflow in mind: clear phases, file-level references, validation gates, and agent-executable instructions.

---

### HANDOFF PROMPT (copy below into Traycer)

```
You are continuing development on Project Chimera — a sovereign autonomous L2 MEV liquidation engine targeting Aave V3 on Base and Arbitrum. The codebase is ~58% complete. All major architecture and code is written but NOTHING has been validated with the full toolchain. Your job is to close every remaining gap until the system is fully autonomous, actively generating revenue, and ready for liquidation.

# PROJECT CONTEXT

Repository: project-chimera (monorepo)
Stack: Rust (core engine) + Yul/Cancun (EVM contracts) + Python 3.11+ (operational scripts) + Foundry + Aave V3
Targets: Base (chain_id=8453), Arbitrum (chain_id=42161)
Revenue model: Flash-loan atomic liquidation of underwater Aave V3 positions. Zero upfront capital. Profit from Aave's liquidation bonus spread.

# DIRECTORY LAYOUT

- core/src/         — Rust engine (config, detector, executor, oracle, pacing_engine, simulator, state, orchestrator, metrics, mempool)
- core/src/main.rs  — Production entrypoint (boot sequence: logging → config → metrics → mode gate → RPC → snapshot → signer → provider → orchestrator loop)
- core/src/orchestrator.rs — Continuous detect→simulate→pace→execute loop
- config/           — YAML/TOML configs (pacing.yaml, pools.toml, risk.yaml, routing.yaml, eoa_pool.json)
- contracts/src/    — Executor.yul (flash-loan atomic execution) + FundDistributor.sol + Solidity interfaces
- contracts/test/   — Foundry tests (Executor.t.sol, FundDistributor.t.sol)
- contracts/script/ — Deploy.s.sol (multisig-enforced deployment)
- scripts/          — 14 Python operational scripts (sweep_profits, fund_eoa, emergency_pause, health_check, toggle_shadow, snapshot_generator, etc.)
- monitoring/       — Prometheus + Grafana + Alertmanager
- docker/           — docker-compose.yml + Dockerfile.rust
- ai-audit/         — Optional Slither + Ollama contract scanner (auxiliary, not part of revenue path)
- docs/             — Architecture, deployment-checklist, first-run-onboarding, threat-model, security-research, operator-manual, snapshot-schema, emergency-procedures

# CRITICAL INVARIANTS (never violate)

1. config/pacing.yaml values MUST match core/src/config.rs defaults AND the valid_yaml() test fixture
2. docs/snapshot-schema.md MUST stay synchronized with core/src/simulator/prewarm.rs ReserveData struct
3. Monetary values: Rust Decimal, never f64
4. EVM contracts target Cancun only (foundry.toml evm_version = "cancun")
5. Python scripts must remain importable without web3.py installed (graceful fallback)
6. All executor contract changes require a corresponding Foundry test in contracts/test/
7. execute_mode defaults to "shadow" — never auto-flip to live
8. The cargo-gated shadow-guard CI job refuses any PR that flips execute_mode: live in committed config

# CURRENT STATE: WHAT EXISTS BUT IS UNVALIDATED

Everything listed in docs/deployment-checklist.md §1 is blocked on first toolchain run.
Known compile blockers (from .kilo/plans/comprehensive-refactor-validation-security-research.md):
- Async trait object safety issues in oracle/mod.rs, state/mod.rs, executor/mod.rs (traits stored as Arc<dyn Trait> but traits have async/RPITIT methods)
- Possible missing #[sol(rpc)] on Alloy contract interfaces
- EOA pool loading: PacingEngine::load_eoa_pool may parse Vec<String> but config/eoa_pool.json uses {wallets: [{address, excluded}]}

# PHASED EXECUTION PLAN

## Phase 1: Restore Compile & Test Suite
Goal: Get every test green on a full-toolchain workstation.

Tasks:
1. Fix async trait object safety — use #[async_trait] crate or boxed futures for PriceOracle, StatePersistence, TransactionExecutor traits. Update all impl blocks and dyn Trait usages.
2. Add #[sol(rpc)] to Alloy contract interfaces (IAavePool, AggregatorV3Interface, IAavePriceOracle). Verify with alloy 1.7 API.
3. Install forge-std: `forge install foundry-rs/forge-std` in contracts/. Add remappings.txt.
4. Run `cargo test -p chimera-core`. Fix ALL compilation errors iteratively. Do not proceed until green.
5. Run `forge test --root contracts/ -vvv`. Fix ALL test failures. Verify Yul artifact path resolution.
6. Run `python -m compileall scripts ai-audit/scripts`. Fix import errors.
7. Run `slither contracts --config-file slither.config.json`. Triage findings.
8. Verify AGENTS.md invariant #1: compare config/pacing.yaml ↔ core/src/config.rs Default impl ↔ valid_yaml() test fixture. Fix any divergence.

Validation gate: `cargo test -p chimera-core && forge test --root contracts/ && python -m compileall scripts ai-audit/scripts` ALL exit 0.

## Phase 2: Close Functional Integration Gaps
Goal: Wire all disconnected subsystems so the full revenue loop is structurally complete.

Tasks:
1. DEX swap calldata wiring: The orchestrator sets venue="aerodrome" but the swap calldata for Executor.yul is not constructed from real DEX routing data. Connect update_venues.py output → calldata builder in executor/calldata.rs → Executor.yul swap step.
2. Snapshot ↔ prewarm data contract: Ensure snapshot_generator.py serializes ALL fields ReserveData needs (a_token, variable_debt_token, indexes, rates, timestamps, liquidation_threshold, liquidation_bonus, config bitmap). Update prewarm.rs to consume them (no zero placeholders).
3. Oracle ETH price for pacing: Wire AaveOracle or a dedicated Chainlink ETH/USD feed into PacingEngine gas cost estimation. Use the eth_price_usd_fallback ($1800) only as last resort.
4. EOA pool loading at startup: Verify PacingEngine::load_eoa_pool correctly parses config/eoa_pool.json shape ({wallets: [{address, excluded, ...}]}). The orchestrator calls select_next_eoa() — verify this path works end-to-end.
5. Chain multi-processing: Document whether Chimera runs one process per chain or switches internally. If one-per-chain, ensure config/ deployment supports this cleanly.

Validation gate: Can run `cargo build --release` cleanly. Can generate a mock snapshot, load it, run the detector, simulate a candidate, and get a pacing decision — all without panicking.

## Phase 3: End-to-End Integration Test
Goal: Prove the full loop works in shadow mode with mock data.

Tasks:
1. Write an integration test (or test script) that:
   a. Generates a mock snapshot (snapshot_generator.py --chain base --mock)
   b. Boots the binary in shadow mode
   c. Confirms the orchestrator scans, finds candidates, simulates them, makes pacing decisions, and shadow-logs the results
   d. Confirms metrics endpoint responds with incrementing counters
   e. Confirms breaker_state == 0
2. Verify JSONL persistence: outcomes.jsonl is written and recover_state.py can rebuild state from it.
3. Verify emergency flag: emergency_pause.py creates the flag, binary detects it within ≤3s, breaker trips.
4. Verify crash recovery: kill binary, restart, confirm state restored from JSONL.

Validation gate: Integration test passes. Shadow mode binary runs for 1 hour with zero errors.

## Phase 4: Testnet Deployment
Goal: Deploy contracts to testnet and smoke-test the flash-loan flow.

Tasks:
1. Deploy Executor + FundDistributor to Base Sepolia via contracts/script/Deploy.s.sol
2. Verify: owner() == multisig address, multisig.code.length > 0
3. From multisig: setPool(<aaveV3Pool>) and confirm slot reads back correctly
4. Verify contracts on block explorer
5. Smoke test: one end-to-end liquidation attempt on testnet (may not find real candidates — verify the calldata is constructed correctly and the tx would succeed)
6. Test auth: non-owner exec() reverts, non-pool executeOperation reverts

Validation gate: Contracts deployed, verified, auth tested on Base Sepolia.

## Phase 5: Shadow Soak (7 Days)
Goal: Prove the system behaves correctly over sustained operation.

Tasks:
1. Configure: real Base RPC URL, real EOA public addresses in eoa_pool.json, real snapshot (not mock)
2. Start shadow mode binary
3. Verify metrics healthy: breaker_state=0, candidates_seen incrementing, sims_run incrementing
4. Run 7 continuous days
5. Cross-validate simulated profit against fetch_historical_liquidations.py
6. Test crash-restart recovery during the soak
7. Perform emergency drill: emergency_pause.py → verify breaker trips within ≤5s
8. Archive soak evidence: metrics export + log summary

Validation gate: 7 days clean, zero unexpected breaker trips, simulated profit within 10% of historical.

## Phase 6: Go-Live Transition
Goal: Flip to live mode and execute the first real liquidation.

Pre-conditions:
- Phase 5 soak evidence archived
- Encrypted keystore created (CHIMERA_KEYSTORE_PATH + CHIMERA_KEYSTORE_PASSWORD)
- CHIMERA_OPERATOR_TOKEN set
- Multisig owns mainnet contracts
- docs/emergency-procedures.md reviewed by operator

Tasks:
1. Run `toggle_shadow.py --set-live` (enforces 7-day rule — will refuse if soak incomplete)
2. Deploy contracts to Base mainnet via multisig
3. Fund worker EOAs (~0.02 ETH each) via fund_eoa.py
4. Start binary with keystore configured
5. Watch first ~10 live ops at minimum sizing
6. Confirm inclusion > 85%, no breaker trips
7. Verify first profit sweep (manual path if sweep_profits.py not fully validated)

Validation gate: First live liquidation executed successfully. Profit swept to treasury.

# KEY FILES TO READ FIRST

- README.md — Architecture overview and revenue model
- AGENTS.md — Invariants and module boundaries
- docs/deployment-checklist.md — Complete pre-flight through go-live checklist
- docs/first-run-onboarding.md — Step-by-step first run guide (notes known gaps)
- .kilo/plans/comprehensive-refactor-validation-security-research.md — Known compile/integration blockers
- .kilo/plans/comprehensive-implementation-plan-2026.md — Previous implementation planning
- core/src/main.rs — Production entrypoint
- core/src/orchestrator.rs — Main loop
- config/pacing.yaml — All runtime configuration
- contracts/src/Executor.yul — Flash-loan execution contract

# WORKFLOW INSTRUCTIONS FOR TRAYCER

Use Phases Mode with the 6 phases above. For each phase:
1. Read the relevant files
2. Make changes
3. Run the validation gate before considering the phase complete
4. Do NOT skip ahead if a validation gate fails — fix the blocker first

After Phase 3, each subsequent phase requires operator (human) input for:
- RPC URLs (Phase 5)
- EOA addresses (Phase 5)
- Keystore creation (Phase 6)
- Multisig setup (Phase 6)
- The toggle_shadow.py --set-live command (Phase 6)

Focus your autonomous work on Phases 1-3. Phases 4-6 have human gate dependencies.

Start with Phase 1, Task 1: fix async trait object safety in the Rust core.
```

---

## Part 4: Summary for the Operator

| Metric | Value |
|--------|-------|
| Code completion (all layers written) | ~75% |
| Validated completion (tests passing, system runnable) | ~15% |
| Revenue-generating completion (live, autonomous) | 0% |
| **Overall weighted completion toward 100%** | **~58%** |

The Traycer prompt above is designed to be pasted directly into a Traycer Epic or Phases Mode task. It contains:
- Full project context and directory layout
- All invariants that must not be violated
- The exact known blockers
- 6 ordered phases with file-level task descriptions
- Validation gates per phase
- Clear delineation of what Traycer can do autonomously (Phases 1-3) vs what requires operator input (Phases 4-6)

Traycer will use AGENTS.md automatically (it detects it at the repo root). The existing `.kilo/plans/` documents provide additional context that Traycer can reference.
