# Contributing to Project Chimera

This is a private repository. These guidelines are for maintainers and invited contributors.

## Shadow-Mode-First Principle

The engine runs in shadow mode by default. **Never auto-flip to live.** The `execute_mode` field in config and fixtures must always commit as `shadow`. The shadow-to-live transition is gated in code (`toggle_shadow.py`) and requires a mandatory 7-day shadow soak, encrypted treasury/worker keystores, a multisig-owned standalone Executor, canonical Aave Pool configuration, and authorization of every active worker.

The `shadow-guard` CI job blocks any PR that flips `execute_mode: live` in committed files.

## Development Environment Setup

### Required Toolchain

1. **Rust** — stable channel as specified in `core/rust-toolchain.toml`. Install via [rustup](https://rustup.rs):
   ```
   rustup show
   ```
   This will pick up the toolchain version, `rustfmt`, and `clippy` from `core/rust-toolchain.toml`.

2. **Foundry** — `forge`, `cast`, `anvil`. Install via [foundryup](https://book.getfoundry.sh/getting-started/installation):
   ```
   curl -L https://foundry.paradigm.xyz | bash
   foundryup
   forge install foundry-rs/forge-std  # inside contracts/
   ```

3. **Python 3.11+** — create a virtual environment:
   ```
   python -m venv venv
   venv\Scripts\activate   # Windows
   source venv/bin/activate # Linux/macOS
   ```

4. **Slither** (optional, for static analysis):
   ```
   pip install slither-analyzer
   ```

### Build

```bash
cargo build -p chimera-core --release
```

## Running Tests

All commands run from the repository root.

```bash
# Rust unit + integration tests
cargo test -p chimera-core

# Solidity contract tests
forge test --root contracts/

# Python script syntax check
python -m compileall scripts ai-audit/scripts

# Static analysis (optional)
slither contracts --config-file slither.config.json
```

## Validation Gate

Before committing changes, run all four validation steps on a machine with the full toolchain:

1. `cargo test -p chimera-core`
2. `forge test --root contracts/`
3. `python -m compileall scripts ai-audit/scripts`
4. `slither contracts --config-file slither.config.json`

A PR must not be merged until all gates pass.

## Invariants

These constraints must be preserved in every change:

| # | Invariant | Rationale |
|---|-----------|-----------|
| 1 | `config/pacing.yaml` values must match `core/src/config.rs` defaults and the `valid_yaml()` test fixture | Configuration drift breaks pacing enforcement |
| 2 | `docs/snapshot-schema.md` must stay synchronized with `core/src/simulator/prewarm.rs` `ReserveData` struct | Schema mismatch produces corrupted simulation state |
| 3 | Monetary values use Rust `Decimal`, not `f64` | Floating-point rounding is unacceptable in financial calculations |
| 4 | EVM contracts target Cancun only (`foundry.toml` `evm_version = "cancun"`) | The standalone Executor and its tests must execute under the same EVM semantics |
| 5 | Python scripts must remain importable without `web3.py` installed (graceful fallback) | Operator tooling must work on minimal environments |
| 6 | All executor contract changes require a corresponding Foundry test in `contracts/test/` | Untested contract changes risk MEV loss |

## Branch Naming Conventions

| Prefix | Purpose | Example |
|--------|---------|---------|
| `feature/` | New capabilities | `feature/revenue-split` |
| `fix/` | Bug fixes | `fix/isolation-mode-debt-ceiling` |
| `chore/` | Maintenance, tooling, docs | `chore/update-foundry` |

## Commit Guidelines

- Write commits in imperative mood: `Add eMode liquidation bonus override`
- Reference invariants by number when relevant: `Fix Decimal precision (invariant #3)`
- Keep commits focused; avoid mixing unrelated changes

## PR Review Process

1. Create a PR with a descriptive title and body
2. Ensure all four validation gates pass in CI
3. At least one maintainer must review and approve
4. Verify that no committed file flips `execute_mode` to `live`
5. Squash-merge to keep history linear

## Module Boundaries

- **`core/`** — Rust engine library. `lib.rs` re-exports the public API. Binary entry at `main.rs` (orchestrator). Trait boundaries: `PriceOracle`, `TransactionExecutor`, `StatePersistence`.
- **`contracts/`** — EVM execution layer. `Executor.yul` is the standalone, construction-owned flash-loan liquidation contract. Authorized worker EOAs call `execute(bytes)` with ordinary EIP-1559 transactions; there is no EIP-7702/delegation model. `interfaces/` contains Solidity interfaces for cross-contract calls.
- **`config/`** — YAML/TOML/JSON configuration consumed by core (Rust) and scripts (Python). Schema documented in `docs/snapshot-schema.md` or inline.
- **`scripts/`** — Python operational tooling: snapshot generator, wallet rotation, DEX venue discovery, emergency pause.
- **`ai-audit/`** — Auxiliary, optional contract scanner. Independent of the liquidation engine.

## Questions

For questions about the codebase, architecture, or invariants, open an issue or contact the maintainers directly. See `README.md` and `AGENTS.md` for additional context.
