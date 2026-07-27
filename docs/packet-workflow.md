# Atomic Packet Workflow — Three-Leg Bundle Submission

**Version**: 1.0
**Last Updated**: 2026-07-26

---

## Overview

This document describes the complete lifecycle of an atomic three-leg bundle, from mempool detection through on-chain inclusion. The workflow coordinates `MempoolPredator`, `TrancheOrchestrator`, and `TrancheBundler` to capture price-impact deltas from targeted `Executor.execute(bytes)` transactions.

---

## 1. Bundle Structure

An `AtomicPacket` contains three transactions and an execution window:

```rust
AtomicPacket {
    pre_trade_tx: Bytes,        // Buy collateral asset before victim executes
    target_tx: Bytes,           // The intercepted Executor.execute transaction
    post_trade_tx: Bytes,       // Sell collateral asset after victim executes
    execution_window: ExecutionWindow {
        min_block: u64,         // Earliest block for inclusion
        max_block: u64,         // Latest block for inclusion
        max_timestamp: Option<u64>,  // Optional time deadline
    }
}
```

---

## 2. Workflow States

The `TrancheOrchestrator` manages a state machine through the following phases:

```
Idle → Analyzing → Constructing → Submitting → Confirmed/Failed
```

| State | Duration | Description |
|-------|----------|-------------|
| `Idle` | Continuous | Scanning mempool for target transactions |
| `Analyzing` | <1ms | Pattern matching and slippage calculation |
| `Constructing` | <5ms | Building pre/post-trade transactions |
| `Submitting` | 1-30s | Sending bundle to Flashbots relay |
| `Confirmed` | — | Bundle included in target block |
| `Failed` | — | Bundle reverted or missed inclusion window |

---

## 3. Phase Details

### 3.1 Detection (MempoolPredator)

**Input**: Pending transaction from mempool watcher
**Output**: `Option<ScoredTarget>`

1. **Pattern Match**: Check if `to` address matches deployed Executor and calldata starts with `0x09c5eabe` (execute selector)
2. **Decode Target**: Extract transaction details from calldata payload
3. **Calculate Slippage**: Estimate price impact using reserve delta analysis
4. **Score Target**: Composite score = (slippage_bps × 100) + (net_profit_wei / 1e15) + priority_bonus
5. **Filter**: Reject if slippage < threshold (default: 100 bps) or profit < minimum (default: $10)

### 3.2 Construction (TrancheOrchestrator)

**Input**: `ScoredTarget` from detection phase
**Output**: `AtomicPacket`

1. **Build Pre-Trade**: Construct transaction to buy collateral asset before victim execution
2. **Clone Target**: Copy the intercepted `Executor.execute(bytes)` transaction
3. **Build Post-Trade**: Construct transaction to sell collateral asset after victim execution
4. **Set Window**: Calculate execution window based on current block + max_wait_blocks (default: 6)

### 3.3 Submission (TrancheBundler)

**Input**: `AtomicPacket`
**Output**: `Result<B256, ChimeraError>` (bundle hash or error)

1. **Build Bundle**: Combine three transactions into `FlashbotsBundle`
2. **Sign Bundle**: Sign with authorized worker key
3. **Submit to Relay**: Send via `eth_sendBundle` to Flashbots Protect endpoint
4. **Return Hash**: Relay returns bundle hash for tracking

### 3.4 Verification

**Input**: Bundle hash and target block
**Output**: `bool` (inclusion confirmed)

1. **Query Relay**: Check if bundle was included in target block
2. **Confirm or Retry**: If included, mark as `Confirmed`; if missed, mark as `Failed`

---

## 4. Shadow Mode

When `shadow_mode: true` (default), the orchestrator simulates submission instead of sending to relay:

- Generates deterministic bundle hash for tracking
- Returns simulated profit and gas cost estimates
- No on-chain transactions are submitted
- Used for testing and validation before live deployment

---

## 5. Configuration

Key configuration fields in `config/pacing.yaml`:

```yaml
# Atomic bundle parameters
atomic_priority_multiplier: 3      # 3x base fee for inclusion
atomic_max_gas_gwei: 50000         # Ceiling for leg transactions
atomic_min_slippage_bps: 100       # 1% minimum target impact
atomic_max_wait_blocks: 6          # Max blocks to wait for inclusion
atomic_swap_gas_estimate: 150000   # Estimated gas per swap leg

# Execution mode
execute_mode: shadow               # shadow | live
```

Environment overrides:
- `CHIMERA_ATOMIC_PRIORITY_MULTIPLIER`
- `CHIMERA_ATOMIC_GAS_GWEI`
- `CHIMERA_ATOMIC_MIN_SLIPPAGE_BPS`
- `CHIMERA_ATOMIC_MAX_WAIT_BLOCKS`
- `CHIMERA_ATOMIC_SWAP_GAS_ESTIMATE`

---

## 6. Error Handling

| Error | Cause | Recovery |
|-------|-------|----------|
| `Failed` | Bundle reverted on-chain | Log for analysis, continue scanning |
| `Failed` | Bundle missed inclusion window | Adjust max_wait_blocks, retry |
| `Failed` | Relay submission error | Check relay connectivity, retry |
| `Failed` | Insufficient profit | Increase min_profit_usd threshold |

---

## 7. Operational Commands

```bash
# Run in shadow mode (default)
cargo run --release

# Run in live mode (requires config change)
CHIMERA_EXECUTE_MODE=live cargo run --release

# Monitor metrics
curl http://localhost:9100/metrics

# Check bundle status
curl http://localhost:9100/api/state
```

---

## 8. Safety Notes

- **Shadow-first**: Always test in shadow mode before switching to live
- **Circuit breakers**: Auto-halt on 3+ consecutive reverts, gas > 300 gwei, or daily loss > 0.005 ETH
- **Pacing caps**: Daily net ≤ $2,000, weekly net ≤ $7,500, single transfer ≤ $1,000
- **Profit gate**: Each transaction must clear costs by 2.5x minimum
