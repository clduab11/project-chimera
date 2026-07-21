# Project Chimera — Agent Instructions

## Overview
See README.md for architecture overview. This file provides constraints for LLM agents.

## Invariants

<invariants>
1. `config/pacing.yaml` values MUST match `core/src/config.rs` defaults and `valid_yaml()` test fixture
2. `docs/snapshot-schema.md` MUST stay synchronized with `core/src/simulator/prewarm.rs` ReserveData struct
3. Monetary values: Rust Decimal, not f64 (risk of floating-point rounding in financial calcs)
4. EVM contracts target Cancun only (foundry.toml evm_version = "cancun")
5. Python scripts must remain importable without web3.py installed (graceful fallback)
6. All executor contract changes require a corresponding Foundry test in contracts/test/
</invariants>

## Module Boundaries

<module_boundaries>
### config/ — YAML/TOML/JSON configuration
  - Consumed by: core (Rust), scripts (Python)
  - Schema for each file documented in docs/snapshot-schema.md or inline

### core/src/ — Rust engine library
  - lib.rs re-exports public API
  - Binary entry at main.rs (orchestrator)
  - Trait boundaries: PriceOracle, TransactionExecutor, StatePersistence

### contracts/src/ — EVM execution layer
  - Executor.yul: flash-loan atomic liquidation
  - interfaces/: Solidity interfaces for cross-contract calls
  
### scripts/ — Python operational tooling
  - snapshot_generator.py: Aave V3 state extraction
  - rotate_eoa.py: wallet rotation
  - update_venues.py: DEX liquidity discovery
  - emergency_pause.py: circuit-breaker webhook
  - dashboard.py: stdlib-only ops console on 127.0.0.1:9553 (proxies engine metrics on 9554, tails logs/, public ETH/USD ticker)
</module_boundaries>

## Validation Gate

<validation_gate>
Before committing, run on a machine with full toolchain:
  1. cargo test -p chimera-core
  2. forge test --root contracts/
  3. python -m compileall scripts ai-audit/scripts
  4. slither contracts --config-file slither.config.json
</validation_gate>
