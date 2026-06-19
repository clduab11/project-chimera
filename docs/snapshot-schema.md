# Market Snapshot Schema

`scripts/snapshot_generator.py` emits the JSON consumed by `core/src/simulator/prewarm.rs` and future detector hydration code.

## Top-Level Fields

```json
{
  "chain": "base",
  "block_number": 0,
  "pool": "0x0000000000000000000000000000000000000000",
  "timestamp": 0,
  "reserves": [],
  "users": {}
}
```

| Field | Type | Description |
| --- | --- | --- |
| `chain` | string | `base` or `arbitrum`. |
| `block_number` | number | Source block for all reserve/user reads. |
| `pool` | address string | Aave V3 Pool address used for reserve storage warming. |
| `timestamp` | unix seconds | Snapshot generation time. |
| `reserves` | array | Reserve metadata and token addresses. |
| `users` | object | User address to collateral/debt maps. |

## Reserve Fields

```json
{
  "address": "0x...",
  "symbol": "WETH",
  "decimals": 18,
  "ltv": 8000,
  "liquidation_threshold": 8250,
  "liquidation_bonus": 10500,
  "liquidity_rate": "3000000000000000000000000000",
  "variable_borrow_rate": "5000000000000000000000000000",
  "total_variable_debt": "700000000000000000000000000000",
  "price_usd": 2500.0,
  "a_token": "0x...",
  "variable_debt_token": "0x..."
}
```

| Field | Type | Description |
| --- | --- | --- |
| `address` | address string | Underlying reserve asset contract. |
| `symbol` | string | Human-readable ticker, e.g. `WETH`. |
| `decimals` | integer | ERC20 decimals. |
| `ltv` | integer | Loan-to-value in basis points (e.g. `8000` = 80%). |
| `liquidation_threshold` | integer | Liquidation threshold in basis points. |
| `liquidation_bonus` | integer | Liquidation bonus in basis points (e.g. `10500` = 5% bonus). |
| `liquidity_rate` | string | Current liquidity rate as a RAY (`1e27`). |
| `variable_borrow_rate` | string | Current variable borrow rate as a RAY (`1e27`). |
| `total_variable_debt` | string | Total scaled variable debt as a RAY-scaled amount. |
| `price_usd` | number | USD price used for profit/guardrail estimates. |
| `a_token` | address string | Aave aToken contract address. |
| `variable_debt_token` | address string | Aave variable-debt token contract address. |

The `a_token` and `variable_debt_token` addresses are required for real prewarming of ERC20 `balanceOf` storage slots. If they are zero addresses, prewarming falls back to price-only compatibility mode and must not be used for live simulation confidence.

### Reserve configuration bitmap

`core/src/simulator/prewarm.rs` packs the above fields into Aave V3's `ReserveConfigurationMap` bitmap before writing it to the Pool's storage slot. The layout follows the canonical Aave V3 definition:

| Bits | Field |
| --- | --- |
| 0-15 | LTV |
| 16-31 | Liquidation threshold |
| 32-47 | Liquidation bonus |
| 48-55 | Decimals |
| 56 | Active |
| 57 | Frozen |
| 58 | Borrowing enabled |
| 59 | Stable-rate borrowing enabled (deprecated) |
| 60 | Paused |
| 61 | Borrowable in isolation |
| 62 | Siloed borrowing |
| 63 | Flash-loan enabled |
| 64-79 | Reserve factor |
| 80-115 | Borrow cap |
| 116-151 | Supply cap |
| 152-167 | Liquidation protocol fee |
| 168-175 | eMode category (deprecated) |
| 176-211 | Unbacked mint cap (deprecated) |
| 212-251 | Debt ceiling |
| 252 | Virtual accounting (deprecated) |
| 253-255 | Unused |

For mock snapshots the pre-warmer sets only the lower 64 bits: LTV, threshold, bonus, decimals, active, borrowing-enabled, and flash-loan-enabled. All higher-order fields are left at zero.

## User Position Fields

```json
{
  "collateral": {"0xReserve": "1000000000000000000"},
  "debt": {"0xReserve": "2000000000"},
  "emode_category": 0
}
```

All token amounts are serialized as strings to avoid JSON integer precision loss.

## Compatibility Rules

- Missing `pool`, `a_token`, or `variable_debt_token` fields default to zero addresses in Rust.
- Zero-address token fields are accepted only for mock snapshots.
- Live snapshots should fail validation if any active reserve lacks token addresses or reserve configuration fields.
