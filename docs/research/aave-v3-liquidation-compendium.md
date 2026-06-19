# Aave V3 Liquidation Compendium

This document summarizes the protocol facts Chimera must mirror before live liquidation execution. It is intentionally conservative and source-linked.

## Canonical Source Order

1. Aave V3 Origin: https://github.com/aave-dao/aave-v3-origin
2. Aave deployed-address data: https://github.com/bgd-labs/aave-address-book
3. Aave official docs: https://aave.com/docs/aave-v3/smart-contracts
4. DeepWiki explanatory index: https://deepwiki.com/aave/aave-v3-core/4.2-liquidation-logic

## Liquidation Preconditions

- A position is liquidatable only when the health factor is below 1.
- Health factor depends on collateral value, debt value, reserve liquidation thresholds, eMode category overrides, oracle prices, and accrued debt indexes.
- Any stale oracle value or missing reserve configuration must fail closed and deny the candidate.

## Close Factor Rules To Verify Against Origin

DeepWiki summarizes Aave V3 liquidation logic as having a default close factor and max close factor when health factor is sufficiently low. The current Rust detector must be verified against the exact Origin constants and tests before live mode.

Implementation checklist:

- Confirm `CLOSE_FACTOR_HF_THRESHOLD` value and scale.
- Confirm default close factor value and max close factor value.
- Confirm whether `MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD` and `MIN_LEFTOVER_BASE` apply on the target deployment.
- Confirm behavior under eMode and isolation mode.
- Add golden fixtures for both default-close-factor and max-close-factor positions.

## Reserve Configuration Bits

Aave V3 stores reserve parameters in a packed configuration bitmap. Chimera currently extracts liquidation bonus from bits 32-47. Before live mode, verify all bit ranges against the exact Origin commit used by the deployment.

Fields required by Chimera:

- LTV.
- Liquidation threshold.
- Liquidation bonus.
- Decimals.
- Active/frozen/paused flags.
- Borrowing enabled.
- Isolation mode and debt ceiling.
- Liquidation protocol fee.

## Snapshot Requirements

Snapshots must include:

- Pool address.
- Reserve underlying address.
- aToken address.
- VariableDebtToken address.
- Decimals.
- Liquidation threshold and bonus.
- Liquidity and variable borrow indexes.
- Current rates and last update timestamp.
- User collateral and variable debt balances.
- eMode category and isolation-mode flags.

If any field is unavailable, the simulator should run in shadow-only mode and mark the candidate incomplete.

## Regression Scenarios

- ETH price shock where multiple positions cross health factor at once.
- wstETH/weETH eMode position with oracle divergence.
- USDC debt against volatile collateral with non-18-decimal debt.
- Isolation-mode collateral with debt ceiling near exhaustion.
- Bad-debt path where collateral cannot fully cover debt.
- L2 fee spike that turns gross-positive liquidation into net-negative execution.
