# Bespoke Tranche Opportunity — Sequential Capital Reallocation

**Status**: Shadow-mode, pending go-live decision
**Version**: 0.3.0
**Chain**: Base (chain_id 8453)
**Date**: 2026-07-26

---

## 1. Overview

Project Chimera pivots from passive shadow liquidation monitoring to active
**Sequential Capital Reallocation**. The original liquidation niche (Aave V3
health-factor scanning on Base) is statistically dormant. The underlying Rust
engine and EVM contracts remain technically sound.

A **Bespoke Tranche Opportunity** is a sequential block capture designed to
profit from the price impact of targeted third-party transactions. We monitor
the private mempool for transactions targeting the deployed
`Executor.execute(bytes)` signature associated with Aave V3 liquidations on
Base, then insert pre-execution and post-execution trades around the target
within the same block. The delta between pre-execution and post-execution
prices constitutes the captured remuneration.

---

## 2. Sequential Capture Logic

Each Bespoke Tranche Opportunity executes three transactions in a single
block, submitted atomically through a Flashbots Protect relay:

### Step A — Pre-Execution Trade

1. The **Tranche Scanner** monitors the private mempool (Flashbots Protect) for
   pending `Executor.execute(bytes)` transactions.
2. The target transaction's calldata is decoded to extract:
   - `collateral`: the asset that will be seized by the liquidation
   - `amount`: the flash-loan amount in the debt asset's native units
   - `dex_router`: the DEX router used for the collateral→debt swap
3. A **buy trade** is executed on the collateral asset *immediately before* the
   target transaction, purchasing at the pre-liquidation market price.

### Step B — Target Execution

The `Executor.execute(bytes)` transaction executes the Aave V3 liquidation:

1. Flash-loans the debt asset from Aave V3 Pool
2. Repays the underwater position's debt
3. Seizes the collateral at the liquidation discount
4. Swaps collateral → debt via the specified DEX router
5. Repays the flash loan + premium
6. The swap leg causes price impact on the collateral asset on the DEX

### Step C — Post-Execution Trade

A **sell trade** is executed on the collateral asset *immediately after* the
target transaction, selling at the elevated post-liquidation price.

### Remuneration

```
Remuneration = (Post-trade sell price − Pre-trade buy price) × trade_size
               − gas_costs
               − flashbots_builder_tip
```

By bundling all three transactions atomically, we ensure inclusion ordering
and prevent competing searchers from unbundling the sequence.

---

## 3. Health Factor Precision Fix

The `apply_close_factor` function in `core/src/detector/liquidation.rs` now uses
the exact Aave V3 linear close-factor interpolation between
`CLOSE_FACTOR_HF_THRESHOLD` (0.95 RAY) and
`HEALTH_FACTOR_LIQUIDATION_THRESHOLD` (1.0 RAY):

```
closeFactor = 0.5 + 10 × (1.0 − HF)        (all in RAY = 1e27)
```

- HF >= 1.0: close factor = 0
- HF <= 0.95: close factor = 100%
- 0.95 < HF < 1.0: linear interpolation 50%→0%

All values are computed in RAY (1e27) fixed-point arithmetic, ensuring that
eight-decimal USD values are properly scaled against twenty-seven-decimal
RAY-scaled raw token units before the close factor is applied. This prevents
precision collapse for micro-liquidation positions that were previously
rounded to zero or a fixed half.

---

## 4. Transaction Inclusion Verification

`RpcSubmitter::poll_receipt` (in `core/src/executor/submitter.rs`) is wired
into the live execution path. Every broadcast transaction is confirmed via
receipt polling with exponential backoff:
- Retries: up to `max_retries` (default 3)
- Backoff: starts at `retry_backoff_ms` (default 500ms), doubles each retry
- Success: transaction confirmed, profit recorded
- Revert: gas spent tracked, zero profit
- Unconfirmed: treated as failure, gas booked as spent

---

## 5. Private Mempool Integration

Bundles are submitted through a Flashbots Protect relay (`eth_sendBundle`)
to ensure atomic inclusion, private submission, and builder cooperation.

### Configuration

| Env Var | Default | Description |
|---|---|---|
| `CHIMERA_FLASHBOTS_RELAY` | `https://rpc.flashbots.net` | Flashbots Protect relay |
| `CHIMERA_TRANCHE_ENABLED` | `false` | Master enable flag |
| `CHIMERA_TRANCHE_MAX_GAS_GWEI` | `50` | Gas price ceiling |
| `CHIMERA_TRANCHE_MIN_PROFIT_USD` | `1.00` | Min expected profit |
| `CHIMERA_TRANCHE_MAX_SLIPPAGE_BPS` | `100` | Max slippage in bps |
| `CHIMERA_TRANCHE_BUNDLE_TIMEOUT_SECS` | `60` | Bundle inclusion timeout |

---

## 6. Risk Controls

- **Pacing engine**: daily/weekly net USD caps, venue rotation, profit multiplier
- **Circuit breaker**: auto-halt on consecutive reverts, emergency flag
- **Cross-process reservation**: advisory file locks prevent duplicate execution
- **Shadow-first**: `CHIMERA_TRANCHE_ENABLED` defaults to `false`

---

## 7. Architecture Diagram

```
┌──────────────────────────────────────────────────────────────────┐
│                    SINGLE BLOCK (Base L2)                         │
│                                                                  │
│  ┌──────────────┐   ┌────────────────────┐   ┌──────────────┐   │
│  │  PRE-TRADE   │──→│ EXECUTOR.EXECUTE   │──→│ POST-TRADE   │   │
│  │  Buy asset   │   │ (Aave V3 liq.)     │   │ Sell asset   │   │
│  │  @ price P   │   │ Swap → price P'    │   │ @ price P'   │   │
│  └──────────────┘   └────────────────────┘   └──────────────┘   │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐    │
│  │              Flashbots Protect Relay                      │    │
│  │  eth_sendBundle([pre_trade, target, post_trade])          │    │
│  └──────────────────────────────────────────────────────────┘    │
│                                                                  │
│  Remuneration = (P' − P) × size − gas − tip                     │
└──────────────────────────────────────────────────────────────────┘
```

---

## 8. Operational Commands

### Enable

```bash
export CHIMERA_TRANCHE_ENABLED=true
export CHIMERA_EXECUTE_MODE=live
```

### Monitor

```bash
# Dashboard: http://127.0.0.1:9553
# Metrics: curl http://127.0.0.1:9100/metrics | grep tranche
# Logs: tail -f logs/chimera.log.* | grep -E "tranche|bundle|flashbots"
```

### Halt

```bash
export CHIMERA_TRANCHE_ENABLED=false
# or: python scripts/emergency_pause.py --reason "Tranche halt" pause
```

---

## References

- `core/src/tranche_arbitrage.rs` — Scanner and bundler implementation
- `core/src/detector/liquidation.rs` — Close-factor precision fix
- `core/src/executor/submitter.rs` — Receipt polling with exponential backoff
- `core/src/executor/builder.rs` — Executor calldata ABI definitions
- `.env.live` — Flashbots relay and tranche configuration
