# Changelog

All notable changes to Project Chimera are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---


## [0.1.1] - 2026-06-16 - Fix: f64 to Decimal for all monetary config fields (AGENTS.md Invariant #3)

### Summary
Resolves the Invariant #3 violation introduced in the initial commit: six monetary
fields in PacingConfig were typed as f64, creating floating-point rounding risk in
financial calculations. All affected fields are now rust_decimal::Decimal. The fix
touches core/src/config.rs and core/src/pacing_engine.rs. No YAML files required
changes - serde_yaml + rust_decimal's serde feature deserializes plain numeric YAML
values into Decimal natively. 70 tests pass (66 unit + 2 config-sync + 2 integration),
including both 3-way pacing invariant assertions.

### Changed

#### core/src/config.rs
- max_daily_net_usd: f64        -> Decimal
- max_weekly_net_usd: f64       -> Decimal
- max_single_transfer_usd: f64  -> Decimal
- max_daily_loss_eth: f64       -> Decimal
- min_profit_multiplier: f64    -> Decimal
- eth_price_usd_fallback: f64   -> Decimal
- Default impl: replaced f64 literals with Decimal::from(N) and
  Decimal::from_str("N").expect("valid literal") for fractional values
- default_eth_price_usd_fallback() serde default fn: now returns Decimal::from(1800)
- validate(): all cap comparisons use Decimal directly; no f64 cast anywhere
- apply_env_overrides(): override_decimal! macro now calls Decimal::from_str() instead
  of v.parse::<f64>()
- Unit tests in config::tests: assertions now compare Decimal == Decimal

#### core/src/pacing_engine.rs
- Removed 27 Decimal::from_f64_retain(self.config.X).unwrap_or(Decimal::MAX) call
  sites; replaced with direct field access (fields are now native Decimal)
- estimate_gas_cost_usd(): eth_price_usd_fallback used directly as Decimal
- Breaker threshold comparisons (max_daily_loss_eth, max_weekly_net_usd,
  max_single_transfer_usd): direct Decimal field comparisons throughout
- make_test_config() in tests: all monetary fields use Decimal::from(N) and
  Decimal::from_str("N").unwrap() instead of bare f64 literals
- proptest prop_never_exceeds_daily_cap: proptest still generates f64 strategy values
  (legitimate arbitrary test inputs, not config fields); converted at test boundary
  via Decimal::from_f64(v).unwrap_or(ZERO)
- Removed unused use std::str::FromStr import
- Fixed prop_assert! calls: escaped pattern-match messages to avoid rustc
  format-string parse errors on the { .. } syntax

### No-change files (verified still correct)
- config/pacing.yaml -- plain numeric values; Decimal serde handles it natively
- core/tests/fixtures/pacing_canonical.yaml -- same
- core/tests/config_sync_test.rs -- already used assert_eq! on struct fields;
  now compares Decimal == Decimal automatically; no source edit needed
- core/Cargo.toml -- rust_decimal already present in dependencies with serde feature

### Verification
- cargo check: clean (0 errors, 0 warnings)
- cargo test (WSL Ubuntu, rustc 1.96.0 stable): 70 passed, 0 failed
    - 66 unit tests
    - 2 config-sync tests (pacing_yaml_matches_rust_defaults, disk_yaml_matches_canonical)
    - 2 orchestrator integration tests
- Invariant #1 (3-way pacing sync) confirmed intact
- Invariant #3 (Decimal not f64) fully resolved
## [0.1.0] - 2026-06-16 ΓÇö Initial Repository Commit

### Summary
First tracked commit of the Project Chimera codebase. This commit captures the
complete working tree produced across Phases 1ΓÇô6 of the implementation plan,
including the Rust engine core, EVM execution contracts, Python operational
scripts, configuration, monitoring, Docker infrastructure, and all supporting
documentation. The index was reconstructed from a stale staging state: four
deleted-but-still-indexed files were removed from the index, three artifacts
were permanently excluded via .gitignore (.venv/, temp_executor.yul,
tests/fixtures/golden_replays.json), and all remaining untracked work was
staged.

83 files, 19 268 insertions.

---

### Added

#### Repository Scaffolding
- `.gitignore` ΓÇö comprehensive ignore rules covering Rust build artifacts,
  Python caches, secrets/keys, large data files, Foundry cache/out/broadcast,
  Docker data volumes, IDE files, and newly added: `.venv/`, `temp_executor.yul`,
  and the misplaced duplicate `tests/fixtures/golden_replays.json`
- `AGENTS.md` ΓÇö LLM agent instruction file; documents invariants, module
  boundaries, and the validation gate all agents must pass before committing
- `README.md` ΓÇö architecture overview: sovereign L2 MEV liquidation bot
  targeting Aave V3 on Base and Arbitrum; describes the flash-loan atomic
  execution loop, pacing/risk controls, and operator runbook pointers
- `Cargo.toml` ΓÇö workspace manifest binding `chimera-core` crate
- `Cargo.lock` ΓÇö fully pinned dependency tree (5 990 lines)
- `PHASE6_VALIDATION_GATE.md` ΓÇö gate report confirming Phase 6 validation
  criteria; records test results, Slither findings, and sign-off checklist

#### Rust Engine ΓÇö `core/` (`chimera-core` crate, ~4 600 LOC)
- `core/Cargo.toml` ΓÇö crate manifest; deps include tokio, ethers, rust_decimal,
  serde, prometheus, proptest, tracing
- `core/rust-toolchain.toml` ΓÇö pins stable Rust toolchain
- `core/src/lib.rs` ΓÇö public API surface re-exporting all modules
- `core/src/main.rs` ΓÇö binary entry point; constructs and drives `Orchestrator`
- `core/src/config.rs` ΓÇö `PacingConfig` and `RiskConfig` structs with serde
  deserialization from YAML; 415 LOC including validation logic
- `core/src/error.rs` ΓÇö `ChimeraError` enum covering RPC, simulation, pacing,
  persistence, and oracle failure variants
- `core/src/orchestrator.rs` ΓÇö top-level event loop: polls oracle, feeds
  detector, gates on pacing engine, dispatches executor
- `core/src/pacing_engine.rs` ΓÇö 859 LOC; daily/weekly net-USD caps, per-op
  transfer limits, circuit-breaker logic, `BreakerReason` enum,
  `PacingDecision` with full audit trail
- `core/src/metrics.rs` ΓÇö Prometheus metrics server; counters and gauges for
  opportunities seen/taken/skipped, breaker trips, PnL, gas spend
- `core/src/detector/mod.rs` + `core/src/detector/liquidation.rs` ΓÇö 349 LOC;
  `LiquidationDetector` consuming `MarketSnapshot` from oracle, scoring
  candidates by health factor and expected profit
- `core/src/oracle/mod.rs` + `oracle/aave.rs` + `oracle/chainlink.rs` ΓÇö
  `PriceOracle` trait; Aave V3 pool data fetching and Chainlink price feed
  aggregation; 508 LOC total
- `core/src/executor/mod.rs` + `executor/balance.rs` + `executor/builder.rs` +
  `executor/submitter.rs` ΓÇö `TransactionExecutor` trait; calldata builder for
  flash-loan liquidation payloads; EOA balance/gas sufficiency checks;
  RPC submitter with receipt polling; 657 LOC total
- `core/src/simulator/mod.rs` + `simulator/prewarm.rs` + `simulator/golden.rs`
  ΓÇö `LiquidationSimulator` trait; pre-warm reserve data cache (`ReserveData`
  struct documented in `docs/snapshot-schema.md`); golden-replay harness for
  regression testing; 1 084 LOC total
- `core/src/state/mod.rs` + `state/persistence.rs` + `state/recovery.rs` ΓÇö
  `StatePersistence` trait; JSON-backed state file with atomic write pattern;
  crash-recovery with last-known-good fallback; 531 LOC total

#### Tests ΓÇö `core/tests/`
- `core/tests/config_sync_test.rs` ΓÇö validates 3-way pacing invariant:
  `pacing.yaml` values == `PacingConfig` defaults == `pacing_canonical.yaml`
  fixture (Invariant #1 from AGENTS.md)
- `core/tests/integration_test.rs` ΓÇö end-to-end happy-path and breaker-trip
  integration tests using mock oracle and executor
- `core/tests/fixtures/pacing_canonical.yaml` ΓÇö canonical fixture for the
  config sync test
- `core/tests/fixtures/golden_replays.json` ΓÇö golden replay scenarios for
  simulator regression tests
- `core/proptest-regressions/pacing_engine.txt` ΓÇö proptest regression seeds

#### EVM Contracts ΓÇö `contracts/` (Foundry/Cancun)
- `contracts/foundry.toml` ΓÇö Foundry config; `evm_version = "cancun"`,
  optimizer enabled, remappings for interfaces
- `contracts/src/Executor.yul` ΓÇö 730 LOC; flash-loan atomic liquidation
  contract in Yul; receives flash loan, calls Aave liquidationCall, swaps
  collateral via DEX router, repays loan, forwards net profit to owner
- `contracts/src/FundDistributor.sol` ΓÇö profit distribution and EOA sweep
  helper; 87 LOC
- `contracts/src/interfaces/IAavePool.sol` ΓÇö Aave V3 pool interface subset
- `contracts/src/interfaces/IDexRouter.sol` ΓÇö DEX router interface (Uni V2/V3
  compatible)
- `contracts/test/Executor.t.sol` ΓÇö 214 LOC Foundry tests: happy-path
  liquidation, insufficient-profit revert, reentrancy guard, gas snapshot
- `contracts/test/FundDistributor.t.sol` ΓÇö distribution and access-control tests

#### Python Operational Scripts ΓÇö `scripts/`
- `scripts/snapshot_generator.py` ΓÇö 619 LOC; Aave V3 state extraction;
  graceful fallback when web3.py unavailable (Invariant #5)
- `scripts/rotate_eoa.py` ΓÇö 288 LOC; wallet rotation with nonce tracking
- `scripts/update_venues.py` ΓÇö 575 LOC; DEX liquidity discovery and
  `config/pools.toml` refresh
- `scripts/emergency_pause.py` ΓÇö 237 LOC; circuit-breaker webhook handler
- `scripts/fetch_historical_liquidations.py` ΓÇö 503 LOC; historical replay
  data collection
- `scripts/fund_eoa.py` ΓÇö 48 LOC; EOA funding helper
- `scripts/sweep_profits.py` ΓÇö 35 LOC; profit sweep to cold wallet
- `scripts/__init__.py` ΓÇö makes scripts/ a package (importable without web3.py)
- `requirements.txt` ΓÇö pinned Python deps: slither-analyzer, web3, pytest,
  crytic-compile

#### Configuration ΓÇö `config/`
- `config/pacing.yaml` ΓÇö runtime pacing parameters (daily/weekly caps,
  per-op limits, cooldown); must stay in sync with `PacingConfig` defaults
- `config/risk.yaml` ΓÇö risk thresholds (max slippage, min health factor delta,
  gas price ceiling)
- `config/routing.yaml` ΓÇö DEX routing preferences per chain and token pair
- `config/pools.toml` ΓÇö active Aave V3 pool addresses per network
- `config/eoa_pool.json` ΓÇö EOA wallet pool with nonce state
- `config/forensic_tags.json` ΓÇö address tagging for known MEV/liquidation bots

#### Infrastructure ΓÇö `docker/`, `monitoring/`
- `docker/Dockerfile.rust` ΓÇö multi-stage Rust build image
- `docker/docker-compose.yml` ΓÇö full stack: chimera engine, Prometheus,
  Grafana; volume mounts for config and state
- `monitoring/prometheus.yml` ΓÇö scrape config for chimera metrics endpoint
- `monitoring/grafana/dashboards/chimera-overview.json` ΓÇö pre-built dashboard:
  PnL, breaker state, opportunity funnel, gas metrics

#### Documentation ΓÇö `docs/`
- `docs/architecture.md` ΓÇö 388 LOC; component diagram, data flow, trait
  boundaries, deployment topology
- `docs/snapshot-schema.md` ΓÇö `ReserveData` struct schema; must stay in sync
  with `core/src/simulator/prewarm.rs` (Invariant #2)
- `docs/operator-manual.md` ΓÇö 446 LOC; runbook for deploy, config tuning,
  emergency pause, log triage, EOA rotation
- `docs/emergency-procedures.md` ΓÇö incident response playbook
- `docs/testing-strategy-liquidations.md` ΓÇö test pyramid: unit, integration,
  proptest, golden replay, Foundry fork tests
- `docs/security-research.md` ΓÇö threat model and known attack surfaces
- `docs/research/aave-v3-liquidation-compendium.md` ΓÇö protocol mechanics
  reference
- `docs/research/dependency-cve-triage.md` ΓÇö CVE triage for pinned deps
- `docs/gate-report-2026-06-16.md` + `docs/gate-report-2026-06-16-t2.md` ΓÇö
  validation gate run records
- `docs/review-2026-06-16-uncommitted.md` ΓÇö pre-commit review notes

#### Audit Infrastructure ΓÇö `ai-audit/`, `audits/`, `.cargo/`
- `ai-audit/prompts/audit_prompt_template.txt` ΓÇö LLM audit prompt template
- `ai-audit/scripts/run_audit.py` ΓÇö 848 LOC; automated audit orchestration
- `audits/osv-scanner.json` ΓÇö OSV vulnerability scan results
- `audits/pip-audit.json` ΓÇö pip-audit results for Python deps
- `.cargo/audit.toml` ΓÇö cargo-audit advisory database config

#### Tooling ΓÇö `.kilo/`
- `.kilo/kilo.jsonc` ΓÇö Kilo agent workspace config
- `.kilo/plans/comprehensive-implementation-plan-2026.md` ΓÇö 1 137 LOC phased
  implementation plan (Phases 1ΓÇô6)
- `.kilo/plans/comprehensive-refactor-validation-security-research.md` ΓÇö
  refactor and security research plan

---

### Removed from Index (stale AD entries, files no longer exist at these paths)
- `ai-audit/prompts/cfg_ssa_grounding.txt` ΓÇö deleted; content superseded by
  `ai-audit/prompts/audit_prompt_template.txt`
- `core/src/simulator.rs` ΓÇö monolith split into `core/src/simulator/` module
  directory (`mod.rs`, `prewarm.rs`, `golden.rs`)
- `tests/config_sync_test.rs` ΓÇö moved to `core/tests/config_sync_test.rs`
- `tests/integration_test.rs` ΓÇö moved to `core/tests/integration_test.rs`

### Excluded via .gitignore (not committed)
- `.venv/` ΓÇö Python virtual environment with symlinks incompatible with
  Windows Git; reproducible from `requirements.txt`
- `temp_executor.yul` ΓÇö scratch artifact at workspace root; not part of the
  contract build
- `tests/fixtures/golden_replays.json` ΓÇö duplicate; canonical copy lives at
  `core/tests/fixtures/golden_replays.json`

---

### Known Issues (carried forward, not fixed in this commit)
- `PacingConfig` monetary fields (`max_daily_net_usd`, `max_single_transfer_usd`,
  `max_weekly_net_usd`) use `f64` instead of `rust_decimal::Decimal` ΓÇö
  violates AGENTS.md Invariant #3; fix tracked separately
- `config/risk.yaml` and `config/routing.yaml` have no Rust loader yet;
  currently consumed only by Python scripts
- CRLF line-ending warnings on all files ΓÇö Windows Git autocrlf; cosmetic only,
  no functional impact; resolve with a `.gitattributes` if needed

