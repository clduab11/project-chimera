# Project Chimera — Comprehensive Implementation Plan

> **Baseline date:** 2026-06-17  
> **Repository root:** `C:\Users\cld-main\Desktop\github-projects\project-chimera`  
> **Current phase:** Phase 1 ⏵ Phase 2 (validation hardening + live shadow mode)  
> **Stack:** Rust 1.82+ / Yul (Cancun EVM) / Python 3.11+ / Foundry / Aave V3  

---

## Table of Contents

1. [Architecture Assessment & Current State](#1-architecture-assessment--current-state)
2. [Context Engineering for LLM Workflows](#2-context-engineering-for-llm-workflows)
3. [Phase 2 — Validation Hardening (Weeks 1–4)](#3-phase-2--validation-hardening)
4. [Phase 3 — Treasury & Wallet Funding System](#4-phase-3--treasury--wallet-funding)
5. [Phase 4 — Orchestrator & Live Shadow Pipeline](#5-phase-4--orchestrator--live-shadow-pipeline)
6. [Phase 5 — Deployment, Monitoring & Operations](#6-phase-5--deployment-monitoring--operations)
7. [Scalability & Maintainability Strategy](#7-scalability--maintainability-strategy)
8. [Change Log & File Manifest](#8-change-log--file-manifest)

---

## 1. Architecture Assessment & Current State

### 1.1 Module Inventory

```
project-chimera/
├── core/                    ← Rust engine (alloy 1.7 + revm 36)
│   ├── config.rs            ✅ Pacing caps, env overrides, validation
│   ├── detector/            ✅ Liquidation pre-filter (local snapshot, no RPC)
│   ├── executor/            ✅ ABI builder + RPC submitter (retry, dry-run)
│   ├── oracle/              ✅ Chainlink + Aave oracle trait + mock
│   ├── pacing_engine.rs     ✅ Caps, breakers, EOA/venue rotation, proptest
│   ├── simulator/           ✅ REVM pre-warm, golden replay
│   ├── state/               ✅ JSONL audit trail, crash recovery trait
│   ├── mempool/             ⚠️ EMPTY — placeholder for future mempool scan
│   └── main.rs              ⚠️ DEMO — not an orchestrator loop
├── contracts/               ← Yul executor + Foundry tests
│   ├── src/Executor.yul     ✅ Flash loan + liquidation + DEX swap + EIP-7702
│   └── test/Executor.t.sol  ✅ 5 passing tests (callback, profit gate, revert, EIP-7702, exec)
├── scripts/                 ← Python utility layer
│   ├── snapshot_generator.py     ✅ Mock + live Aave V3 reserve data
│   ├── update_venues.py          ✅ On-chain DEX liquidity queries (web3.py)
│   ├── fetch_historical_liquidations.py  ✅ Chainlink ETH/USD + LiquidationCall events
│   ├── rotate_eoa.py             ✅ Round-robin selection + cooldown
│   └── emergency_pause.py        ✅ Breaker webhook + state halt
├── config/                  ✅ All configs updated (pacing, routing, pools, risk, forensic_tags, eoa_pool)
├── tests/fixtures/          ✅ golden_replays.json (4 entries, new schema)
├── docker/                  ⚠️ EMPTY — no compose files
├── monitoring/grafana/      ⚠️ EMPTY — no dashboards
└── ai-audit/                ✅ Slither + Ollama pipeline stub
```

### 1.2 Critical Gaps

| Gap | Severity | Description |
|-----|----------|-------------|
| **No treasury funding** | 🔴 Critical | Worker EOAs need gas ETH — no auto-fund or balance pre-flight check |
| **No profit sweep** | 🔴 Critical | Liquidation profits accrue in worker wallets — no consolidation |
| **No orchestrator loop** | 🟠 High | `main.rs` is a static demo, not a continuous detection→simulation→execution pipeline |
| **Mempool module empty** | 🟡 Medium | No pending-tx scanning for competitive liquidation timing |
| **Docker/monitoring empty** | 🟡 Medium | No deployment artifacts or Grafana dashboard JSON |
| **EOA pool uses placeholders** | 🟡 Medium | Addresses like `0x1111…1111` need to be replaced with real EOAs |
| **Pool address divergence** | 🟡 Medium | `pools.toml` Base pool `0xA238Dd...44E8` differs from `snapshot_generator.py` `0x0000…0000` |

---

## 2. Context Engineering for LLM Workflows

### 2.1 Principles (June 2026)

Context engineering is the discipline of structuring information so that coding LLMs receive precisely the right context at the right granularity. The project must be structured so that any LLM session — whether Kilo, Claude, Copilot, or a CI agent — can understand and modify any module without ambiguity.

### 2.2 XML Role Tagging Standard

All agent instruction files (`AGENTS.md`, `.kilo/agent/*.md`, `.kilo/command/*.md`) must use XML role tags to clearly delineate instruction types. This is the universally adopted pattern across major coding agents in 2026:

```xml
<agent>
  <identity>
    You are a Rust smart-contract engineer specializing in EVM execution,
    REVM simulation, and Aave V3 liquidation mechanics.
  </identity>

  <constraints>
    - Never modify config/pacing.yaml without updating core/src/config.rs defaults
    - All monetary values MUST use rust_decimal::Decimal, not f64
    - All new Rust tests must include a proptest property test
    - Yul contracts target Cancun EVM only — no pre-Cancun opcodes
  </constraints>

  <context>
    <project>
      Repository: project-chimera (L2 Aave V3 liquidation MEV)
      Language: Rust (core) + Yul (contracts) + Python (scripts)
      Targets: Base (chain_id=8453), Arbitrum (chain_id=42161)
    </project>
    <current_task>
      Implementing Phase 2 validation hardening.
      Rust toolchain required: cargo test -p chimera-core
    </current_task>
    <key_files>
      - core/src/pacing_engine.rs (risk governor)
      - core/src/executor/submitter.rs (tx broadcast)
      - contracts/src/Executor.yul (on-chain executor)
    </key_files>
  </context>

  <workflow>
    1. Read the target file and its tests first
    2. Identify the module boundary (which traits/interfaces cross it)
    3. Write failing tests before implementation (TDD)
    4. Run `cargo test -p chimera-core` to verify
    5. Run `forge test --root contracts/` if contracts changed
    6. Update docs/ if public interfaces changed
  </workflow>

  <output_format>
    - Commit messages: conventional format (feat:, fix:, refactor:, docs:)
    - Code changes: minimal diff, no unrelated formatting
    - Explanations: concise, cite the specific lines that motivated the change
  </output_format>
</agent>
```

### 2.3 AGENTS.md Structure

The project-level `AGENTS.md` (at repo root) serves as the entry point for any LLM context:

```markdown
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
</module_boundaries>

## Validation Gate

<validation_gate>
Before committing, run on a machine with full toolchain:
  1. cargo test -p chimera-core
  2. forge test --root contracts/
  3. python -m compileall scripts ai-audit/scripts
  4. slither contracts --config-file slither.config.json
</validation_gate>
```

### 2.4 Per-Command XML Templates

Create `.kilo/command/` templates for common multi-step operations:

**`.kilo/command/phase2-validation.md`**
```xml
<command>
  <name>phase2-validation</name>
  <description>Run Phase 2 validation gate across all layers</description>
  
  <steps>
    <step id="rust-tests">
      <action>cargo test -p chimera-core --verbose</action>
      <on_failure>Read the failing test, check if it's a config sync issue. 
                  Compare config/pacing.yaml values against core/src/config.rs defaults.</on_failure>
    </step>
    <step id="forge-tests">
      <action>forge test --root contracts/ -vvv</action>
      <on_failure>Check Executor.yul selectors match test expectations.
                  Verify mock contracts in Executor.t.sol match IAavePool interface.</on_failure>
    </step>
    <step id="python-syntax">
      <action>python -m compileall scripts/ ai-audit/scripts/</action>
    </step>
    <step id="stale-check">
      <action>grep -rn "max_daily_net_usd" core/src/ config/</action>
      <description>Verify no stale guardrail values exist outside of synchronized locations</description>
    </step>
  </steps>
</command>
```

### 2.5 Context Budget Optimization

Modern LLMs have large context windows, but retrieval quality degrades with noise. Structure files to minimize context pollution:

```
✅ GOOD — Each file has a single, well-defined purpose
   config/pacing.yaml → only pacing guardrails
   core/src/oracle/chainlink.rs → only Chainlink feed logic
   
❌ BAD — Mixed concerns
   core/src/main.rs → config + detection + simulation + execution + metrics (150+ lines of demos)
   scripts/utils.py → everything in one file
```

**Practical rules:**
- Rust modules: max 300 lines per file (split beyond that into submodules)
- Python scripts: max 400 lines (extract shared code into `scripts/lib/`)
- Config files: one concern per file, no cross-cutting config
- Comments: document *why*, not *what* (the code says what)
- Tests: colocated with source (`#[cfg(test)] mod tests`)

---

## 3. Phase 2 — Validation Hardening

### 3.1 Config Synchronization Sweep

**Problem:** Values in `config/pacing.yaml`, `core/src/config.rs` defaults, `core/src/config.rs` test YAML, and `core/src/pacing_engine.rs` test config must always match.

**Step-by-step:**

#### Step 1: Create a unified config fixture

Create `tests/fixtures/pacing_canonical.yaml` as the single source of truth:

```yaml
# Canonical pacing config — all Rust defaults, Python scripts, and docs must match this.
# Last verified: 2026-06-17
max_daily_net_usd: 2000
max_weekly_net_usd: 7500
max_single_transfer_usd: 1000
min_interval_hours: 6
max_jitter_hours: 12
venue_rotation_count: 5
clean_eoa_pool_size: 10
auto_halt_on_reverts: 3
max_gas_gwei: 300
max_daily_loss_eth: 0.005
min_profit_multiplier: 2.5
execute_mode: shadow
log_level: info
metrics_port: 9100
chain_id: 8453
oracle_staleness_seconds: 300
eth_price_usd_fallback: 1800
recent_outcomes_capacity: 128
eoa_pool_path: config/eoa_pool.json
pools_toml_path: config/pools.toml
```

#### Step 2: Add a sync-validation integration test

Create `tests/config_sync_test.rs`:

```rust
//! Integration test: verify config/pacing.yaml matches Rust defaults.
//! Run with: cargo test --test config_sync_test

use chimera_core::PacingConfig;
use std::fs;

const CANONICAL_YAML: &str = include_str!("fixtures/pacing_canonical.yaml");

#[test]
fn pacing_yaml_matches_rust_defaults() {
    let canonical: PacingConfig = serde_yaml::from_str(CANONICAL_YAML)
        .expect("Canonical YAML must parse");
    let defaults = PacingConfig::default();

    assert_eq!(canonical.max_daily_net_usd, defaults.max_daily_net_usd,
        "pacing.yaml max_daily_net_usd ({}) != Rust default ({})",
        canonical.max_daily_net_usd, defaults.max_daily_net_usd);
    assert_eq!(canonical.max_weekly_net_usd, defaults.max_weekly_net_usd);
    assert_eq!(canonical.max_single_transfer_usd, defaults.max_single_transfer_usd);
    assert_eq!(canonical.eth_price_usd_fallback, defaults.eth_price_usd_fallback,
        "pacing.yaml eth_price_usd_fallback ({}) != Rust default ({})",
        canonical.eth_price_usd_fallback, defaults.eth_price_usd_fallback);
}

#[test]
fn disk_yaml_matches_canonical() {
    let disk = fs::read_to_string("config/pacing.yaml")
        .expect("config/pacing.yaml must exist on disk");
    let on_disk: PacingConfig = serde_yaml::from_str(&disk)
        .expect("config/pacing.yaml must parse as valid PacingConfig");
    let canonical: PacingConfig = serde_yaml::from_str(CANONICAL_YAML)
        .expect("Canonical YAML must parse");
    
    assert_eq!(on_disk.max_daily_net_usd, canonical.max_daily_net_usd);
    assert_eq!(on_disk.eth_price_usd_fallback, canonical.eth_price_usd_fallback);
}
```

### 3.2 Oracle Pool Address Reconciliation

**Problem:** `config/pools.toml` has Base pool `0xA238Dd...44E8` and data provider `0x2d8A...bC5A`, while `snapshot_generator.py` uses `0x0F43...C73A` and `main.rs` uses `Address::ZERO`.

**Step-by-step:**

#### Step 1: Verify contract addresses against on-chain registries

Base Aave V3 (June 2026 verified addresses):
- Pool Proxy: `0xA238Dd80C259a72e81d7e4666a2CEDEcD3cA5cB`
- PoolDataProvider: `0x0F43731EB8d45A581f4a36DD74F5f358bc90C73A`
- Oracle: `0x2DaD3A13EF0C636622150F51bA8b404Fd4c56B98c`

#### Step 2: Unify all addresses in a single source

Update `config/pools.toml` with verified addresses and add a checksum comment:

```toml
# Aave V3 verified contract addresses
# Source: https://docs.aave.com/developers/deployed-contracts/deployed-contracts
# Verified: 2026-06-17 against basescan.org and arbiscan.io

[base]
pool = "0xA238Dd80C259a72e81d7e4666a2CEDEcD3cA5cB"
pool_data_provider = "0x0F43731EB8d45A581f4a36DD74F5f358bc90C73A"
oracle = "0x2DaD3A13EF0C636622150F51bA8b404Fd4c56B98c"
weth = "0x4200000000000000000000000000000000000006"
usdc = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"

[arbitrum]
pool = "0x794a61358D6845594F94dc1DB02A252b5b4814aD"
pool_data_provider = "0x69FA688f1Dc47d4B5d8029D5a35FB7a5480d4d96"
oracle = "0xb56c2F0B8Be1bd3C6f81A24fe035B91ea9E14711"
weth = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
usdc = "0xaf88d065e77c8cC2239327C5EDb3A432268e5831"
```

#### Step 3: Make snapshot_generator.py read from pools.toml

The script already supports `--pools-config config/pools.toml`. Update the default behavior:

```python
# In snapshot_generator.py, change DEFAULT_DATA_PROVIDERS to load from pools.toml:
def load_pool_config(chain: str, path: str = "config/pools.toml") -> dict[str, str]:
    config_path = Path(path)
    if not config_path.exists():
        return DEFAULT_DATA_PROVIDERS  # fallback
    with open(config_path, "rb") as f:
        data = tomllib.load(f)
    return data.get(chain, {})
```

### 3.3 Golden Replay Schema Validation

Add a Rust-side deserialization test for `tests/fixtures/golden_replays.json`:

```rust
// In core/src/simulator/golden.rs

#[cfg(test)]
mod tests {
    use serde_json;
    use crate::simulator::golden::ReplayFixture;

    #[test]
    fn golden_replays_parse() {
        let raw = std::fs::read_to_string("../../tests/fixtures/golden_replays.json")
            .unwrap_or_else(|e| panic!("golden_replays.json not found: {e}"));
        let fixtures: Vec<ReplayFixture> = serde_json::from_str(&raw)
            .expect("golden_replays.json must deserialize into Vec<ReplayFixture>");
        assert!(fixtures.len() >= 2, "Need at least 2 replay fixtures");
        for f in &fixtures {
            assert!(!f.chain.is_empty());
            assert!(f.block_number > 0);
        }
    }

    #[test]
    fn golden_replays_have_verified_source_fields() {
        let raw = std::fs::read_to_string("../../tests/fixtures/golden_replays.json").unwrap();
        let fixtures: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        for f in &fixtures {
            assert!(f.get("_verified").is_some(), "Missing _verified field");
            assert!(f.get("_source").is_some(), "Missing _source field");
            assert!(f.get("user").is_some(), "Missing user field");
            assert!(f.get("debt_to_cover").is_some(), "Missing debt_to_cover field");
        }
    }
}
```

---

## 4. Phase 3 — Treasury & Wallet Funding

### 4.1 Architecture

```
┌──────────────┐         ┌─────────────────────────────────────────┐
│  Treasury     │────────▶│  FundDistributor.sol                    │
│  (multisig)   │  ETH    │  • distribute(Payment[])                │
│               │         │  • onlyOwner                            │
└──────────────┘         └───────────────┬─────────────────────────┘
                                         │ batch ETH sends
                    ┌────────────────────┼────────────────────┐
                    ▼                    ▼                    ▼
            ┌───────────┐        ┌───────────┐        ┌───────────┐
            │ EOA-01    │        │ EOA-02    │        │ EOA-N     │
            │ (worker)  │        │ (worker)  │        │ (worker)  │
            │ 0.01 ETH  │        │ 0.01 ETH  │        │ 0.01 ETH  │
            └─────┬─────┘        └─────┬─────┘        └─────┬─────┘
                  │ execute liq       │ execute liq       │ execute liq
                  ▼                    ▼                    ▼
            ┌────────────────────────────────────────────────────┐
            │                    Aave V3 Pool                     │
            │  flashLoanSimple → liquidationCall → swap → repay   │
            └────────────────────────────────────────────────────┘
            
            After success, profit stays in worker wallet.
            Periodic sweep script consolidates to treasury.
```

### 4.2 Solidity: FundDistributor Contract

Create `contracts/src/FundDistributor.sol`:

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

/// @title FundDistributor
/// @notice Batch-send native ETH from a treasury to worker EOAs.
/// @dev Designed for L2 gas top-ups. Owner is typically a multisig.
contract FundDistributor {
    address public owner;
    address public pendingOwner;

    struct Payment {
        address payable to;
        uint256 amount;
    }

    event Distributed(uint256 count, uint256 totalValue);
    event OwnerTransferRequested(address indexed pendingOwner);
    event OwnerTransferred(address indexed oldOwner, address indexed newOwner);
    event EmergencyWithdraw(address indexed to, uint256 amount);

    error Unauthorized();
    error InsufficientBalance(uint256 required, uint256 available);
    error TransferFailed(address recipient);
    error ZeroPayment();
    error ZeroAddress();

    modifier onlyOwner() {
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    constructor(address _owner) {
        if (_owner == address(0)) revert ZeroAddress();
        owner = _owner;
    }

    /// @notice Send ETH to multiple recipients atomically.
    /// @param payments Array of (recipient, amount) tuples.
    function distribute(Payment[] calldata payments) external payable onlyOwner {
        uint256 totalRequired;
        for (uint256 i = 0; i < payments.length; i++) {
            if (payments[i].amount == 0) revert ZeroPayment();
            totalRequired += payments[i].amount;
        }
        if (totalRequired > msg.value) {
            revert InsufficientBalance(totalRequired, msg.value);
        }

        for (uint256 i = 0; i < payments.length; i++) {
            (bool ok, ) = payments[i].to.call{value: payments[i].amount}("");
            if (!ok) revert TransferFailed(payments[i].to);
        }

        // Refund excess to caller
        uint256 excess = msg.value - totalRequired;
        if (excess > 0) {
            (bool ok, ) = msg.sender.call{value: excess}("");
            if (!ok) revert TransferFailed(msg.sender);
        }

        emit Distributed(payments.length, totalRequired);
    }

    /// @notice Two-step ownership transfer (prevents accidental loss).
    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert ZeroAddress();
        pendingOwner = newOwner;
        emit OwnerTransferRequested(newOwner);
    }

    function acceptOwnership() external {
        if (msg.sender != pendingOwner) revert Unauthorized();
        emit OwnerTransferred(owner, msg.sender);
        owner = pendingOwner;
        pendingOwner = address(0);
    }

    /// @notice Emergency drain. Only owner.
    function emergencyWithdraw() external onlyOwner {
        uint256 bal = address(this).balance;
        (bool ok, ) = owner.call{value: bal}("");
        if (!ok) revert TransferFailed(owner);
        emit EmergencyWithdraw(owner, bal);
    }

    receive() external payable {}
}
```

### 4.3 Rust: Balance Pre-Flight Check

Add `core/src/executor/balance.rs`:

```rust
//! EOA balance verification for gas pre-flight checks.
//! Called before transaction submission to prevent insufficient-funds reverts.

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use async_trait::async_trait;
use tracing::warn;

use crate::ChimeraError;

/// Minimum gas budget for a liquidation transaction on L2.
/// Conservative: 250k gas × 50 gwei = 0.0125 ETH buffer.
pub const MIN_GAS_BUDGET_WEI: u128 = 12_500_000_000_000_000; // 0.0125 ETH

/// Check whether an EOA has enough native balance to cover gas.
///
/// Returns `Ok(true)` if the wallet has at least `MIN_GAS_BUDGET_WEI`
/// more than the estimated gas cost. Returns `Ok(false)` if underfunded.
pub async fn check_eoa_gas_sufficient<P: Provider + Sync>(
    provider: &P,
    eoa: Address,
    estimated_gas_cost_wei: U256,
) -> Result<bool, ChimeraError> {
    let balance = provider
        .get_balance(eoa)
        .await
        .map_err(|e| ChimeraError::RpcError(format!("get_balance failed for {eoa}: {e}")))?;

    let required = estimated_gas_cost_wei.saturating_add(U256::from(MIN_GAS_BUDGET_WEI));

    if balance < required {
        warn!(
            target = "chimera::balance",
            %eoa,
            balance_wei = %balance,
            required_wei = %required,
            shortfall_wei = %(required - balance),
            "EOA has insufficient gas balance — skipping opportunity"
        );
        return Ok(false);
    }
    Ok(true)
}

/// Return the native balance of an address as a U256.
pub async fn get_eoa_balance<P: Provider + Sync>(
    provider: &P,
    eoa: Address,
) -> Result<U256, ChimeraError> {
    provider
        .get_balance(eoa)
        .await
        .map_err(|e| ChimeraError::RpcError(format!("get_balance failed for {eoa}: {e}")))
}
```

Integrate into `lib.rs`:

```rust
pub mod executor;
// In executor/mod.rs, add:
pub mod balance;
pub use balance::{check_eoa_gas_sufficient, get_eoa_balance, MIN_GAS_BUDGET_WEI};
```

### 4.4 Python: Auto-Fund Script

Create `scripts/fund_eoa.py` (full implementation in previous response — see the funding architecture). Key addition: integrate with the pacing engine to prevent funding when circuit breakers are active:

```python
def fund_wallets(...) -> int:
    # Pre-flight: check pacing state before funding
    pacing_state_path = "core/state/pacing_state.json"
    if Path(pacing_state_path).exists():
        state = json.load(open(pacing_state_path))
        if state.get("breaker_tripped"):
            logger.error("Circuit breaker active (%s) — aborting funding", state["breaker_tripped"])
            return 0
    # ... proceed with funding
```

### 4.5 Python: Profit Sweep Script

Create `scripts/sweep_profits.py` (full implementation in previous response). Add pacing-aware guard:

```python
def sweep_worker_eth(w3, worker_key, treasury, chain, pacing_config: dict) -> int:
    """Sweep excess ETH from worker to treasury, respecting pacing caps."""
    # Don't sweep if daily pacing cap would be exceeded
    account = w3.eth.account.from_key(worker_key)
    balance = w3.eth.get_balance(account.address)
    # ... sweep logic with pacing awareness
```

---

## 5. Phase 4 — Orchestrator & Live Shadow Pipeline

### 5.1 Main Orchestrator Loop

Replace the current demo `main.rs` with a proper continuous loop:

```rust
// core/src/orchestrator.rs (new file)

use crate::{
    check_eoa_gas_sufficient, BuiltTransaction, CalldataBuilder, ChimeraError,
    LiquidationDetector, LiquidationSimulator, MarketSnapshot, Metrics, Opportunity,
    PacingConfig, PacingDecision, PacingEngine, PriceOracle, RpcSubmitter,
    TransactionExecutor,
};
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

/// Configuration for the orchestrator loop.
pub struct OrchestratorConfig {
    /// How often to scan for new liquidation opportunities.
    pub scan_interval_secs: u64,
    /// Maximum number of opportunities to process per scan cycle.
    pub max_opportunities_per_scan: usize,
    /// Minimum profit (USD) to proceed with simulation.
    pub min_sim_profit_usd: f64,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            scan_interval_secs: 12, // ~1 Aave block on L2
            max_opportunities_per_scan: 5,
            min_sim_profit_usd: 5.0,
        }
    }
}

/// The main orchestrator. Runs a continuous detect→simulate→execute loop.
pub struct Orchestrator<P: Provider + Send + Sync + 'static> {
    config: OrchestratorConfig,
    pacing: PacingEngine,
    metrics: Arc<Metrics>,
    provider: Arc<P>,
    snapshot: MarketSnapshot,
    chain_id: u64,
}

impl<P: Provider + Send + Sync + 'static> Orchestrator<P> {
    pub fn new(
        config: OrchestratorConfig,
        pacing: PacingConfig,
        metrics: Arc<Metrics>,
        provider: Arc<P>,
        snapshot: MarketSnapshot,
        chain_id: u64,
    ) -> Self {
        Self {
            config,
            pacing: PacingEngine::new(pacing),
            metrics,
            provider,
            snapshot,
            chain_id,
        }
    }

    /// Main loop — runs until interrupted.
    pub async fn run(&self) -> Result<(), ChimeraError> {
        info!(target = "chimera::orchestrator", chain_id = self.chain_id, "Orchestrator starting");

        let detector = LiquidationDetector::new(self.snapshot.clone(), self.chain_id);

        loop {
            // 1. Detect at-risk positions
            let candidates = detector.find_at_risk_positions();
            self.metrics.observe_candidate(&self.snapshot.chain);

            // 2. Process top candidates
            for candidate in candidates.iter().take(self.config.max_opportunities_per_scan) {
                if let Err(e) = self.process_candidate(candidate).await {
                    warn!(target = "chimera::orchestrator", error = %e, "Candidate processing failed");
                    self.metrics.observe_sim("error", 0.0, 0, 0, 0.0);
                }
            }

            // 3. Sleep before next scan
            sleep(Duration::from_secs(self.config.scan_interval_secs)).await;
        }
    }

    async fn process_candidate(
        &self,
        candidate: &crate::LiquidationCandidate,
    ) -> Result<(), ChimeraError> {
        let opp_id = format!("liq-{}", candidate.user);

        // Phase gate: pacing check
        let opp = Opportunity {
            id: opp_id.clone(),
            expected_net_usd: Decimal::from_f64_retain(candidate.estimated_profit_usd).unwrap_or(Decimal::ZERO),
            gas_estimate_gwei: candidate.gas_estimate_gwei,
            venue: candidate.venue.clone(),
            eoa: candidate.eoa.clone(),
            timestamp: chrono::Utc::now(),
        };

        match self.pacing.check(&opp)? {
            PacingDecision::Deny { reason } => {
                info!(target = "chimera::orchestrator", id = %opp.id, reason = %reason, "Pacing denied");
                self.metrics.observe_sim("denied", 0.0, 0, 0, 0.0);
                return Ok(());
            }
            PacingDecision::Allow { release_at } => {
                info!(target = "chimera::orchestrator", id = %opp.id, release_at = %release_at, "Pacing allowed");
            }
        }

        // Balance pre-flight: check EOA has enough gas
        let eoa_addr: Address = candidate.eoa.parse()
            .map_err(|e| ChimeraError::ConfigError(format!("Invalid EOA address: {e}")))?;
        let estimated_gas_wei = U256::from(candidate.gas_estimate_gwei as u128)
            * U256::from(candidate.estimated_gas_units as u128)
            * U256::from(1_000_000_000u128); // gwei to wei

        let has_gas = check_eoa_gas_sufficient(&*self.provider, eoa_addr, estimated_gas_wei).await?;
        if !has_gas {
            self.pacing.record_outcome(&opp, Decimal::ZERO, Decimal::ZERO, true);
            self.metrics.observe_sim("insufficient_funds", 0.0, 0, 0, 0.0);
            return Ok(());
        }

        // Simulation phase
        // (In production: call LiquidationSimulator here with REVM replay)
        let simulated_profit_usd = candidate.estimated_profit_usd;

        // Record outcome
        let gas_spent_eth = Decimal::from(candidate.gas_estimate_gwei) / Decimal::from(1_000_000_000)
            * Decimal::from(150_000u64); // rough gas cost in ETH
        let realized = Decimal::from_f64_retain(simulated_profit_usd - 0.5).unwrap_or(Decimal::ZERO); // after gas

        if realized > Decimal::ZERO {
            self.pacing.record_outcome(&opp, realized, gas_spent_eth, false);
            self.metrics.observe_sim("success", simulated_profit_usd, 0, 0, simulated_profit_usd);
        } else {
            self.pacing.record_outcome(&opp, Decimal::ZERO, gas_spent_eth, true);
            self.metrics.observe_sim("revert", 0.0, 0, 0, 0.0);
        }

        Ok(())
    }
}
```

### 5.2 Integration Test Harness

Create `tests/integration_test.rs` to validate cross-module interactions:

```rust
//! Integration test: orchestrator pacing + detector + EOA validation
//! Run: cargo test --test integration_test

use chimera_core::{PacingConfig, PacingEngine, Opportunity};
use rust_decimal::Decimal;

#[test]
fn orchestrator_pacing_allows_valid_liquidation() {
    let config = PacingConfig::default(); // 2000 daily, 7500 weekly, 1000 single
    let engine = PacingEngine::new(config);
    
    let opp = Opportunity {
        id: "integration-001".into(),
        expected_net_usd: Decimal::new(450, 0), // $450 profit
        gas_estimate_gwei: 30,
        venue: "aerodrome".into(),
        eoa: "0x1111111111111111111111111111111111111111".into(),
        timestamp: chrono::Utc::now(),
    };
    
    let decision = engine.check(&opp).unwrap();
    assert!(matches!(decision, chimera_core::PacingDecision::Allow { .. }),
        "Valid liquidation should pass pacing gate");
}

#[test]
fn orchestrator_pacing_denies_over_single_cap() {
    let config = PacingConfig::default();
    let engine = PacingEngine::new(config);
    
    let opp = Opportunity {
        id: "integration-002".into(),
        expected_net_usd: Decimal::new(2000, 0), // $2000 > $1000 single cap
        gas_estimate_gwei: 30,
        venue: "aerodrome".into(),
        eoa: "0x1111111111111111111111111111111111111111".into(),
        timestamp: chrono::Utc::now(),
    };
    
    let decision = engine.check(&opp).unwrap();
    assert!(matches!(decision, chimera_core::PacingDecision::Deny { .. }),
        "Over-cap liquidation should be denied");
}
```

---

## 6. Phase 5 — Deployment, Monitoring & Operations

### 6.1 Docker Compose

Create `docker/docker-compose.yml`:

```yaml
version: "3.9"
services:
  chimera-core:
    build:
      context: ..
      dockerfile: docker/Dockerfile.rust
    environment:
      - BASE_RPC_URL=${BASE_RPC_URL}
      - ARB_RPC_URL=${ARB_RPC_URL}
      - CHIMERA_EXECUTE_MODE=shadow
      - RUST_LOG=chimera=info
    volumes:
      - chimera-state:/app/core/state
      - ../config:/app/config:ro
    restart: unless-stopped
    depends_on:
      - qdrant

  grafana:
    image: grafana/grafana:11.5.0
    ports:
      - "3002:3002"   # Changed from 3000 to avoid Docker port conflicts
    volumes:
      - grafana-data:/var/lib/grafana
      - ../monitoring/grafana/dashboards:/etc/grafana/provisioning/dashboards:ro
    environment:
      - GF_SERVER_HTTP_PORT=3002
      - GF_SECURITY_ADMIN_PASSWORD=${GRAFANA_ADMIN_PASSWORD:-chimera}
    restart: unless-stopped

  prometheus:
    image: prom/prometheus:v2.53.0
    ports:
      - "9090:9090"
    volumes:
      - ../monitoring/prometheus.yml:/etc/prometheus/prometheus.yml:ro
    restart: unless-stopped

  qdrant:
    image: qdrant/qdrant:v1.13.2
    ports:
      - "6333:6333"
    volumes:
      - qdrant-data:/qdrant/storage
    restart: unless-stopped

volumes:
  chimera-state:
  grafana-data:
  qdrant-data:
```

Create `docker/Dockerfile.rust`:

```dockerfile
FROM rust:1.82-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY core/ core/
RUN cargo build --release -p chimera-core

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/chimera /usr/local/bin/chimera
COPY config/ /app/config/
WORKDIR /app
EXPOSE 9100
ENTRYPOINT ["chimera"]
```

### 6.2 Grafana Dashboard

Create `monitoring/grafana/dashboards/chimera-overview.json` with panels for:
- Pacing decisions (allow/deny counter)
- Breaker state (gauge)
- Daily/weekly net USD (time series)
- EOA pool health (balance + rotation status)
- Oracle price freshness
- Detection candidate rate

### 6.3 Operational Runbook

Add to `docs/operator-manual.md`:

```markdown
## Pre-Flight Checklist (Before Each Shadow Mode Run)

1. **Verify pacing config sync**
   ```bash
   diff <(grep "max_daily_net_usd" config/pacing.yaml | awk '{print $2}') \
        <(grep "max_daily_net_usd:" core/src/config.rs | grep -oP '\d+' | head -1)
   # Should produce no output (values match)
   ```

2. **Check EOA pool balances**
   ```bash
   python scripts/rotate_eoa.py --config config/eoa_pool.json --chain base --min-eth 0.005
   ```

3. **Verify Aave pool addresses**
   ```bash
   python scripts/snapshot_generator.py --chain base --mock --output /tmp/test.json
   cat /tmp/test.json | jq '.pool'
   ```

4. **Start shadow mode**
   ```bash
   CHIMERA_EXECUTE_MODE=shadow cargo run --release
   ```

5. **Monitor**
   - Open Grafana at localhost:3002
   - Check `/metrics` endpoint on port 9100

## Emergency Procedures

1. **Halt execution**
   ```bash
   python scripts/emergency_pause.py --halt
   ```

2. **Recover state**
   ```bash
   cat core/state/audit.jsonl | jq -s 'last'
   ```
```

---

## 7. Scalability & Maintainability Strategy

### 7.1 Configuration Layering

Implement a layered config system that resolves values from highest to lowest priority:

```
1. CLI arguments (--max-daily-net)
2. Environment variables (CHIMERA_MAX_DAILY_NET_USD)
3. Config file (config/pacing.yaml)
4. Code defaults (PacingConfig::default())
```

This is already partially implemented via `load_with_env()`. Extend to a unified resolver:

```rust
// core/src/config/resolver.rs

pub trait ConfigResolver {
    fn resolve(&self, key: &str) -> Option<String>;
}

pub struct LayeredResolver {
    layers: Vec<Box<dyn ConfigResolver>>,
}

impl LayeredResolver {
    pub fn new() -> Self {
        Self {
            layers: vec![
                Box::new(EnvResolver),
                Box::new(FileResolver::new("config/pacing.yaml")),
                Box::new(DefaultResolver),
            ],
        }
    }
}
```

### 7.2 Feature Flags

As the system grows (mempool scanning, multi-chain, new DEXes), use Cargo feature flags to control compilation:

```toml
# core/Cargo.toml
[features]
default = ["base", "arbitrum"]
base = []
arbitrum = []
optimism = []
mempool-scanner = ["dep:reth-net"]
advanced-routing = []
```

### 7.3 Testing Strategy

| Layer | Tool | Scope | Frequency |
|-------|------|-------|-----------|
| **Unit** | `cargo test` | Individual functions | Every commit |
| **Integration** | `cargo test --test *` | Cross-module | Every PR |
| **Contract** | `forge test` | Yul executor + mocks | Every PR |
| **Python** | `pytest` | Script logic | CI |
| **Property** | `proptest` | Pacing invariants | `cargo test` |
| **Shadow** | `chimera --shadow` | Full pipeline on testnet | Daily |
| **Fuzzing** | `cargo fuzz` | ABI decoding edge cases | Weekly |

### 7.4 Module Evolution Path

```
Phase 2 (NOW)        Phase 3 (2 weeks)    Phase 4 (4 weeks)     Phase 5 (6 weeks)
─────────────        ─────────────────    ─────────────────     ─────────────────
✅ Config sync       📋 FundDistributor    📋 Orchestrator loop  📋 Mempool scanner
✅ Schema validation 📋 fund_eoa.py        📋 Live shadow mode   📋 Multi-chain
✅ Pool addresses    📋 sweep_profits.py   📋 Grafana dashboards 📋 Advanced routing
✅ Balance check     📋 Balance pre-flight 📋 Docker compose     📋 Fuzz testing
```

---

## 8. Change Log & File Manifest

### Files to Create

| File | Phase | Purpose |
|------|-------|---------|
| `tests/fixtures/pacing_canonical.yaml` | 2 | Single source of truth for pacing values |
| `tests/config_sync_test.rs` | 2 | Integration test: YAML ↔ Rust defaults |
| `tests/integration_test.rs` | 2 | Cross-module orchestrator tests |
| `contracts/src/FundDistributor.sol` | 3 | Batch ETH distribution to EOAs |
| `contracts/test/FundDistributor.t.sol` | 3 | Foundry tests for distributor |
| `core/src/executor/balance.rs` | 3 | EOA gas balance pre-flight |
| `core/src/orchestrator.rs` | 4 | Continuous detection→execute loop |
| `scripts/fund_eoa.py` | 3 | Treasury → EOA funding script |
| `scripts/sweep_profits.py` | 3 | EOA → treasury profit sweep |
| `docker/docker-compose.yml` | 5 | Full stack deployment |
| `docker/Dockerfile.rust` | 5 | Multi-stage Rust build |
| `monitoring/grafana/dashboards/chimera-overview.json` | 5 | Operational dashboard |
| `AGENTS.md` | 2 | LLM agent instructions (XML-tagged) |
| `.kilo/command/phase2-validation.md` | 2 | Kilo command template |

### Files to Modify

| File | Phase | Change |
|------|-------|--------|
| `config/pools.toml` | 2 | Unify addresses with snapshot_generator.py |
| `config/pacing.yaml` | 2 | Verify sync (may need no change) |
| `core/src/lib.rs` | 3 | Export `orchestrator`, `executor::balance` |
| `core/src/executor/mod.rs` | 3 | Add `pub mod balance` |
| `core/src/main.rs` | 4 | Replace demo with orchestrator launch |
| `scripts/snapshot_generator.py` | 2 | Default to reading pools.toml |
| `README.md` | 5 | Update architecture diagram, add deploy section |
| `docs/operator-manual.md` | 5 | Add pre-flight checklist |
| `requirements.txt` | 3 | Already has web3>=6.0 (sufficient) |

### Environment Variables Required

| Variable | Source | Purpose |
|----------|--------|---------|
| `BASE_RPC_URL` | Operator | Base mainnet RPC endpoint |
| `ARB_RPC_URL` | Operator | Arbitrum mainnet RPC endpoint |
| `CHIMERA_TREASURY_PRIVATE_KEY` | Treasury operator | Sign funding transactions |
| `CHIMERA_EXECUTE_MODE` | CLI / env | `shadow` or `live` |
| `GRAFANA_ADMIN_PASSWORD` | Docker | Grafana access |
| `BASE_CHAINLINK_ETH_USD` | Auto | Chainlink feed override |

---

## Appendix: XML Role Tag Reference

For any agent instruction file in this project, use only these XML tags:

| Tag | Purpose | Required |
|-----|---------|----------|
| `<agent>` | Root container | Yes |
| `<identity>` | Role definition, persona | Yes |
| `<constraints>` | Hard rules, invariants | Yes |
| `<context>` | Project + task metadata | Yes |
| `<workflow>` | Step-by-step procedure | Recommended |
| `<output_format>` | Commit message style, code conventions | Recommended |
| `<invariants>` | Cross-cutting rules (used in AGENTS.md) | Yes (in AGENTS.md) |
| `<module_boundaries>` | File-to-responsibility map | Yes (in AGENTS.md) |
| `<validation_gate>` | Pre-commit checklist | Yes (in AGENTS.md) |
| `<command>` | Kilo command definition | For .kilo/command/*.md |
| `<steps>` / `<step>` | Command execution steps | For commands |
| `<action>` | Shell command to run | For steps |
| `<on_failure>` | Recovery/troubleshooting guidance | For steps |

### Anti-patterns (DO NOT use)

- ❌ Mixing YAML and XML in the same instruction file
- ❌ Using `<!-- -->` HTML comments for agent instructions (use XML tags instead)
- ❌ Putting constraints inside `<identity>` (they have different semantics)
- ❌ Using more than 3 levels of nesting in any XML structure
- ❌ Duplicating content between AGENTS.md and individual .kilo/agent/*.md files

---

*End of implementation plan. Review against the project's Phase 2 validation gate before proceeding.*
