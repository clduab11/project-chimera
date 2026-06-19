# Comprehensive Refactor Validation + Security Research Plan

## Objective
Bring the repository from the current post-refactor state to a clean, production-ready, documented shadow-mode baseline by recursively auditing all four refactor waves, fixing integration regressions, removing obsolete artifacts, and adding a curated security research compendium relevant to Aave V3 liquidations, Rust Ethereum tooling, Yul/Foundry, Slither, and dependency CVEs.

## Read-Only Findings From Planning

### Critical compile/integration blockers
- `core/src/oracle/mod.rs`, `core/src/state/mod.rs`, and `core/src/executor/mod.rs` define async/RPITIT trait methods but the code stores `Arc<dyn PriceOracle>` and `Arc<dyn StatePersistence + Send + Sync>`. These traits are not object-safe in this form and will fail compilation. Fix with `async-trait` or boxed futures.
- Alloy `sol!` interfaces used for RPC calls appear to lack `#[sol(rpc)]`; Context7 Alloy docs show this is required to generate contract structs and call builders like `IAavePool::new(...).getReserveData(...).call().await`.
- `config/pacing.yaml` lacks newly required `PacingConfig` fields (`chain_id`, `oracle_staleness_seconds`, `eoa_pool_path`, `pools_toml_path`). Because only `recent_outcomes_capacity` has `serde(default)`, current config loading will fail.
- `PacingEngine::load_eoa_pool()` parses `Vec<String>`, but `config/eoa_pool.json` is an object with a `wallets` array. `main.rs` calls this with `?`, so the demo binary currently fails on startup.
- The README and main commands use `cargo build -p chimera-core` from repo root, but the repo has no root workspace `Cargo.toml`. Either add a workspace or change docs/scripts to use `--manifest-path core/Cargo.toml`.
- `main.rs` writes `data/audit.jsonl`, while `.gitignore` only ignores `core/state/`; state path and ignore rules are inconsistent.

### Protocol correctness blockers
- `detector/liquidation.rs` close-factor tiers should be revalidated against Aave V3 Origin constants. The current 50% / 25% / 10% tier logic conflicts with DeepWiki/Aave V3 documentation that describes default close factor and max close factor behavior for low health factors.
- `simulator/prewarm.rs` still references `seed_state.py` in docs/comments and inserts zero placeholders for pool reserve fields instead of actual on-chain `ReserveConfigurationMap`, indexes, rates, and timestamps.
- `scripts/snapshot_generator.py` does not serialize `pool`, `a_token`, or `variable_debt_token`, so the new prewarm logic cannot actually warm real token balance slots. Its default data provider addresses include placeholders/inconsistent values and should be sourced from `config/pools.toml`.
- `pacing_engine.rs` estimates gas cost with a fixed `$2000` ETH price and a TODO; this should use the oracle or a configurable conservative fallback before production claims.
- `AaveOracle` returns Aave market-reference prices, but the simulator treats `eth_oracle_asset` as ETH/USD. Normalize units explicitly and distinguish asset/USD from asset/reference-currency prices.

### Contract/test blockers
- `contracts/src/Executor.yul` contains placeholder event topic comments and hard-coded selectors that must be verified with `cast sig`/generated ABI. Production comments claim readiness before verification.
- `contracts/test/Executor.t.sol` depends on `forge-std/Test.sol`, but the repository does not include `forge-std` or remappings. Add dependency setup or document/install it.
- Foundry `vm.getCode("Executor.yul")` may not resolve the pure Yul artifact as written. Validate artifact names and update tests accordingly.
- Yul executor authorization and safety need hardening: verify allowed pool/caller, validate `initiator`, support safe ERC20 return handling, reset approvals if needed, verify exact flash-loan repayment semantics, and avoid open direct execution unless intentionally controlled.

### Repo cleanliness blockers
- Deprecated/temporary files still exist: `scripts/seed_state.py`, `ai-audit/prompts/cfg_ssa_grounding.txt`, `tests/fixtures/cwe_liquidations.json`, `SPECIFICATION_ANALYSIS.md`, plus `analyze_json.py` and `decode_attachments.py`.
- Raw conversation artifacts `chimera-1.json` through `chimera-5.json` remain in the repo root; decide whether to move to a gitignored research archive or delete them from production source.
- Generated cleanup scripts (`cleanup_deprecated.ps1`, `DELETE_DEPRECATED.sh`) should be removed after actual cleanup unless kept intentionally under `scripts/maintenance/`.
- `.kilo/` contains project config plus package/node artifacts; keep only intentional Kilo config and ensure dependency caches are ignored.

### Documentation inconsistencies
- `docs/architecture.md` still says the executor is a v1 stub and references `scripts/seed_state.py`; update to `snapshot_generator.py` and accurate executor status.
- README states Phase 1 complete/shadow ready, but compile/config/contract validation has not passed yet. Downgrade status until validation succeeds or complete fixes first.
- Some documentation uses “forensic/evasion” language. Reframe as compliance, safety, auditability, and operational risk controls.

## Security Research Sources To Incorporate

### Context7 / current documentation
- Alloy `/alloy-rs/alloy`: `sol!` RPC calls require `#[sol(rpc)]`; provider construction examples use `ProviderBuilder`; receipt fields are accessed from provider receipts.
- Foundry `/foundry-rs/book`: tests should inherit `forge-std/Test.sol`; use `vm.mockCall`, `vm.expectCall`, `vm.expectRevert`; `forge test` supports `--via-ir`, optimizer, and EVM version flags.
- Slither `/crytic/slither`: `slither . --json results.json`, `--json -`, SARIF output, `--filter-paths`, detector include/exclude options, and config-file support.

### GitHub / DeepWiki / public security materials
- Aave docs now point to `aave-dao/aave-v3-origin` as latest V3 source; older `aave/aave-v3-core` is deprecated.
- DeepWiki page `aave/aave-v3-core/4.2-liquidation-logic` is relevant for liquidation logic, close factors, and links to `LiquidationLogic.sol`, `ReserveLogic.sol`, `GenericLogic.sol`, and related source files.
- Aave `aave-address-book` should be the canonical source for Base/Arbitrum pool/oracle/data-provider addresses.
- Solidity compiler known bugs: `docs.soliditylang.org/en/latest/bugs.html`, `bugs.json`, and `bugs_by_version.json` should be referenced for solc/via-IR/Yul compiler risk checks.
- Trail of Bits `trailofbits/slither-mcp` is relevant as an MCP-searchable static-analysis compendium/API layer for Solidity projects.
- Aave oracle/CAPO liquidation incident reporting should be captured as an operational scenario, but any 2026 news-source claims should be marked as unverified until corroborated by Aave governance/Chaos Labs primary sources.

## Implementation Plan

### Phase 1 — Clean repository state and remove obsolete artifacts
1. Delete replaced files: `scripts/seed_state.py`, `ai-audit/prompts/cfg_ssa_grounding.txt`, `tests/fixtures/cwe_liquidations.json`, `SPECIFICATION_ANALYSIS.md`.
2. Delete temporary analysis scripts: `analyze_json.py`, `decode_attachments.py`.
3. Decide final handling for `chimera-1.json` through `chimera-5.json`: remove from production source, or move to a gitignored `research/raw/` archive if they must remain locally available.
4. Remove `cleanup_deprecated.ps1` and `DELETE_DEPRECATED.sh` after cleanup, or move them under `scripts/maintenance/` with documentation.
5. Audit `.kilo/`; keep intentional Kilo config, remove/ignore package caches and generated dependency folders.
6. Update `.gitignore` for actual runtime paths: `core/state/`, `core/snapshots/`, `ai-audit/queue/`, `ai-audit/.daily_counter.json`, `research/raw/`, and any final local-only state paths.

### Phase 2 — Restore Rust compile compatibility
1. Add a root `Cargo.toml` workspace with `members = ["core"]`, or update all README/docs/scripts to use `--manifest-path core/Cargo.toml`. Prefer a root workspace to preserve `cargo build -p chimera-core`.
2. Make async traits object-safe:
   - Either add `async-trait` and annotate `PriceOracle`, `StatePersistence`, and `TransactionExecutor`, or replace `impl Future`/`async fn` in trait definitions with boxed futures.
   - Update impls and dyn trait aliases consistently.
3. Add `#[sol(rpc)]` to Alloy contract interfaces used for RPC calls (`IAavePool`, `AggregatorV3Interface`, `IAavePriceOracle`) and adjust imports/types to match Alloy 1.7.
4. Reconcile Cargo features after compile testing. Start with `alloy = { version = "1.7", features = ["full"] }` if the current feature set fails.
5. Add `serde(default = "...")` to all new `PacingConfig` fields or update `config/pacing.yaml` to include them. Prefer both explicit config values and serde defaults for backward compatibility.
6. Fix `PacingEngine::load_eoa_pool()` to parse the actual `config/eoa_pool.json` object shape (`wallets[].address`) and validate addresses.
7. Align `main.rs` state persistence path with `.gitignore` (`core/state/audit.jsonl`) and create parent directories in `JsonlPersistence::append_outcome()`.
8. Replace fixed ETH price gas-cost estimates in pacing with a configured conservative fallback plus an optional oracle-injected price path.

### Phase 3 — Normalize data contracts between Python, configs, detector, simulator, and prewarm
1. Define a single JSON snapshot schema in `docs/API.md` or `docs/snapshot-schema.md` with fields consumed by both `detector` and `simulator/prewarm`.
2. Update `scripts/snapshot_generator.py` to load pool/oracle/data-provider addresses from `config/pools.toml`, not hardcoded defaults.
3. Add PoolDataProvider calls for token addresses (`aToken`, stable debt token, variable debt token) and serialize `pool`, `a_token`, `variable_debt_token`, indexes, rates, timestamps, liquidation threshold, and liquidation bonus.
4. Update `prewarm.rs` to consume actual serialized reserve data instead of zero placeholders.
5. Rename stale comments from `seed_state.py` to `snapshot_generator.py` throughout Rust/docs.
6. Reconcile `MarketSnapshot` type duplication between `detector/liquidation.rs` and `simulator/prewarm.rs`; either share a common schema module or document the distinction clearly.
7. Confirm `golden_replays.json` fields match `simulator/golden.rs`, or update the harness to parse the new schema.

### Phase 4 — Correct Aave V3 protocol logic against primary sources
1. Use Aave V3 Origin and DeepWiki liquidation-logic references to confirm:
   - health factor scale and comparisons,
   - close factor constants,
   - max close factor threshold,
   - `MIN_LEFTOVER_BASE`,
   - eMode overrides,
   - isolation mode/debt ceiling behavior,
   - liquidation protocol fee handling.
2. Update `detector/liquidation.rs` close-factor logic and tests to match the verified Aave V3 code exactly.
3. Update `simulator/mod.rs` reserve configuration parsing to extract the correct bit ranges and handle protocol liquidation fees.
4. Normalize oracle price units and staleness checks across Chainlink/Aave oracles and detector/simulator math.
5. Add golden replay fixtures that exercise eMode, isolation mode, bad debt, oracle staleness, L2 fee spikes, and non-18-decimal assets.

### Phase 5 — Harden contract execution and Foundry tests
1. Verify all selectors and event topics with `cast sig`/`cast keccak` or generated ABI constants. Replace placeholder event topic.
2. Add `forge-std` dependency/remappings or vendor instructions so `contracts/test/Executor.t.sol` compiles.
3. Confirm the correct artifact path for pure Yul deployment (`vm.getCode(...)`) and update tests accordingly.
4. Add a minimal Solidity harness if pure-Yul test deployment remains fragile.
5. Harden executor authorization:
   - verify `caller()` is the expected Aave Pool,
   - verify `initiator`/receiver semantics,
   - optionally store immutable owner/pool in constructor/codegen or pass through a trusted factory,
   - restrict or remove open direct `exec(bytes)` if not required.
6. Harden token handling:
   - support ERC20s that return no boolean,
   - check transfer/approve return data length correctly,
   - zero approvals if required for nonstandard tokens,
   - fail closed on unexpected returndata.
7. Run Slither on Solidity harness/interfaces and manual Yul review; add notes for limitations because Slither has limited pure-Yul coverage.

### Phase 6 — Stabilize Python utilities and AI audit pipeline
1. Add a Python dependency manifest (`requirements.txt` or `pyproject.toml`) for `web3`, `requests`, `PyYAML`, and test tooling.
2. Run `python -m compileall scripts ai-audit/scripts` and fix syntax/import issues.
3. Update Web3.py middleware usage for the pinned version.
4. Add `--dry-run` and `--mock` smoke tests for each script.
5. Update `run_audit.py` to support Slither JSON/SARIF output per Context7 docs, and add a clear fallback for unverified bytecode where Slither cannot analyze source.
6. Add `ai-audit/queue/.gitkeep` only if the directory should exist in repo; otherwise rely on runtime creation and `.gitignore`.
7. Ensure no scripts create files outside ignored runtime locations by default.

### Phase 7 — Add security research compendium and CVE workflow
1. Create `docs/security-research.md` with curated sections:
   - Aave V3 Origin and DeepWiki liquidation logic references,
   - Aave address-book usage,
   - Solidity compiler known-bugs workflow,
   - Slither and Slither MCP usage,
   - Foundry/Yul security testing notes,
   - dependency CVE scanning workflow.
2. Add `docs/research/aave-v3-liquidation-compendium.md` summarizing verified Aave V3 formulas, source-file links, and test vectors. Cite Aave V3 Origin as canonical and mark old `aave-v3-core` as deprecated/reference-only.
3. Add `docs/research/dependency-cve-triage.md` describing how to run `cargo audit`, `cargo deny` or `osv-scanner`, `pip-audit`, and GitHub Dependabot/advisory checks.
4. Add a `slither.config.json` aligned with Context7 Slither docs (`json`, `sarif`, `filter_paths`, detector exclusions only where justified).
5. Optionally add a `security/` directory for downloaded advisory snapshots if the user wants offline copies. Otherwise keep URLs and commands only to avoid stale vendored data.

### Phase 8 — Documentation consistency pass
1. Update README status to match actual validation outcome. Only claim “Phase 1 complete” after all checks pass.
2. Fix `docs/architecture.md` stale executor and `seed_state.py` references.
3. Add `docs/DEPLOYMENT.md`, `docs/API.md` or `docs/snapshot-schema.md`, and update `docs/operator-manual.md` to match final paths and commands.
4. Reword “forensic/evasion” language to compliance/auditability/risk-control language.
5. Ensure all docs reference the same commands, directories, state paths, and config files.

### Phase 9 — Validation gate
Run these after implementation changes:
1. `git status --short`
2. `cargo fmt --all --check`
3. `cargo test -p chimera-core` if root workspace is added, otherwise `cargo test --manifest-path core/Cargo.toml`
4. `cargo clippy -p chimera-core --all-targets -- -D warnings` if clippy is installed
5. `forge test --root contracts/`
6. `slither contracts --config-file slither.config.json --json ai-audit/queue/slither-smoke.json` or an equivalent non-generated output path
7. `python -m compileall scripts ai-audit/scripts`
8. `python scripts/snapshot_generator.py --chain base --mock --output core/snapshots/base_mock.json`
9. `python scripts/rotate_eoa.py --config config/eoa_pool.json --chain base --dry-run --output-format json`
10. `python ai-audit/scripts/run_audit.py --help`
11. Optional security checks: `cargo audit`, `cargo deny check`, `pip-audit`, `osv-scanner -r .`
12. Final `git diff --stat` and `git diff --check`.

## Execution Notes
- Do not enable live submission or use production RPC credentials during cleanup/validation.
- Treat all generated state, snapshots, audit reports, and raw conversation JSONs as local-only artifacts unless explicitly approved for version control.
- Prefer small, reviewable commits or checkpoints by phase once implementation begins.
