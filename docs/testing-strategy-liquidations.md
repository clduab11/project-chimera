# Testing Strategy: Near-100% Accurate REVM Simulation for Aave V3 Liquidations on L2 (Base/Arbitrum)

**Goal**: Achieve simulation fidelity such that every profitable liquidation candidate identified in simulation has >99.5% probability of succeeding on-chain when submitted within the same block (accounting for L2 sequencer latency). This is critical for the $50-start, zero-capital-risk model (flash-loan or direct atomic bundles) and the strict pacing/circuit-breaker protections.

**Research Insights Incorporated (from Aave v3-origin source, docs, MEV analyses 2026)**:

## Core Formulas (must be replicated exactly in REVM + custom pre-compute)

### Health Factor (GenericLogic.calculateUserAccountData)
- Total Collateral (base currency) = Σ (aToken.scaledBalanceOf(user) * normalizedIncome * assetPrice / 10^decimals)
- Total Debt (base currency) = Σ (variableDebt.scaledBalanceOf(user) * normalizedDebt * assetPrice / 10^decimals)
- Weighted Avg LTV / Liquidation Threshold (by collateral value in base)
- HF = (totalCollateralInBase * avgLiquidationThreshold) / totalDebtInBase
- Special paths:
  - eMode: Use category-specific LTV/liquidationBonus/threshold (higher for correlated assets, e.g. stablecoins or ETH LSTs).
  - Isolation Mode: Debt ceiling applies; only one isolated collateral per user.
  - Zero LTV collateral ignored for LTV calc but still counts for HF.

### Liquidation Amounts (LiquidationLogic.executeLiquidationCall + _calculateAvailableCollateralToLiquidate)
- CLOSE_FACTOR_HF_THRESHOLD = 0.95e18 → HF ≤ 0.95 allows up to 100% debt liquidation (else default 50%).
- MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD = 2000e8 (USD8) → positions above this use close-factor logic.
- MIN_LEFTOVER_BASE = threshold/2 → partial liquidations must leave > this or fully clear position (prevents dust/gas-grief).
- Liquidation bonus (from reserve or eMode) applied to collateral.
- Protocol fee % taken from the bonus portion → sent to treasury aToken.
- actualDebtToLiquidate = min(debtToCover, maxLiquidatableDebt)
- actualCollateralToLiquidate = min(userBalance, baseCollateral * bonus)
- Must not leave dust: if partial, both remaining debt and collateral must be ≥ MIN_LEFTOVER_BASE in base currency.
- Bad debt path: If full collateral liquidation but debt remains → create deficit (protocol loss recorded).
- receiveAToken flag: Liquidator receives aTokens instead of underlying (activates collateral for liquidator if first position).

### L2 / Gas / Fee Accuracy (critical for profit calc)
- L2 execution gas (from REVM or estimateGas on fork) + L1 data fee (eth_getL1Fee on OP Stack / NodeInterface on Arbitrum).
- Must re-query L1 scalar at simulation time (volatile with blob congestion).
- Flash-loan fee (if used): Aave 0.09% or protocol-specific.
- Priority fee / tip on L2 sequencer to win inclusion (Base ~2s blocks, Arbitrum Timeboost option).
- Exact revert reasons must match on-chain (e.g. "MustNotLeaveDust", health factor checks, oracle failures).

### Market Volatility & Failure Patterns Identified
- Rapid price drops (5-20% in minutes on ETH/LSTs or memecoins) spike liquidations; competition increases but L2s still have windows.
- Oracle depegs or staleness (especially stables, LSTs during depeg events).
- Interest accrual over time (must warp or use exact block timestamp in sim).
- Bad debt events when collateral value after bonus < debt.
- Gas wars or sequencer congestion making small positions (<$1k) unprofitable.
- MEV bots / other liquidators frontrunning (L2 central sequencer reduces but doesn't eliminate; simulation must be < block time).
- User mixed collateral + eMode switches between blocks.
- Isolation mode debt ceiling exhaustion.
- Protocol parameter changes (governance) or emergency pauses (rare but must handle in validation).
- Honeypot / malicious debt/collateral tokens on L2 (filter via allowlist + bytecode checks before sim).

## Testing Strategy for ~100% Fidelity

### 1. Fork-Based Replay (Golden Tests) – Highest Confidence
- Capture 100+ real historical liquidation txs from Base/Arbitrum (via Dune, EigenPhi, direct RPC traces, successful + failed).
- For each:
  - Fork the exact pre-tx block state (anvil --fork-url + --fork-block-number or REVM AlloyDB at that block).
  - Replay the exact liquidation calldata (or our generated bundle).
  - Assert:
    - HF before/after matches on-chain view contract within 1 wei.
    - Profit (collateral received - debt repaid - gas - L1 fee) matches on-chain value transfer ±0.1%.
    - Exact events emitted (LiquidationCall, DeficitCreated, etc.).
    - State deltas (balances, indexes, deficit) identical.
- Run on every PR + nightly. Target: 100% pass rate on replayed set.

### 2. Property-Based + Invariant Fuzzing (proptest / Foundry)
- Generate 10,000+ synthetic + real user positions (scrape current at-risk positions via Aave subgraphs or on-chain calls).
- Invariants that must always hold:
  - HF calculation in pure Rust pre-filter + full REVM sim == on-chain Pool.getUserAccountData().
  - Liquidation never violates MIN_LEFTOVER_BASE or dust rules.
  - Profit after all costs (including worst-case L1 fee buffer 1.15x) > 2.5x gas (per risk.yaml).
  - eMode / isolation paths produce identical bonus/LTV/threshold as on-chain.
  - Close-factor logic triggers exactly at HF thresholds.
  - Bad debt creation only when hasNoCollateralLeft && remaining debt > 0.
- Fuzz price drops, time warps (interest accrual), partial vs full debtToCover.
- Run 1M+ iterations in CI (fast path in Rust proptest for pre-filter math; slower full REVM in Foundry).

### 3. Volatility & Edge Scenario Suites
- Price oracle shocks: -5%, -10%, -20% on volatile assets at different HF levels.
- eMode entry/exit + mixed collateral portfolios.
- Isolation mode debt ceiling edge (exactly at ceiling).
- MIN_* threshold boundaries (exactly 2000e8, exactly MIN_LEFTOVER_BASE).
- Flash-loan + liquidation atomic bundle (if using Aave flash for capital efficiency).
- L2 fee model stress: high L1 blob congestion (spike eth_getL1Fee 3-5x).
- Concurrent simulation: 50 parallel liquidations from same fork state (race conditions).
- Oracle failure / sentinel (PriceOracleSentinel) paths on L2.
- receiveAToken=true path + liquidator first-time collateral activation.

### 4. Cross-Validation Layers
- REVM sim result == Anvil/Foundry trace of identical tx (gas, events, state).
- Compare against Aave's official calculators / risk dashboards for historical events.
- Subgraph reconciliation: detected at-risk users match Aave UI "liquidations" tab.
- Gas accounting: Simulated total cost (L2 + L1) within 5% of real inclusion cost on 100 recent txs.
- End-to-end bundle: Build full Executor calldata, simulate in REVM, then dry-run on live fork RPC (eth_call) → must match.

### 5. Continuous Calibration & Monitoring (Production)
- Shadow mode (first 7-14 days): Every real on-chain liquidation is compared to what our detector+sim would have done. Log accuracy, false negatives (missed profitable), false positives (sim said yes, on-chain reverted or unprofitable).
- Alert on any >0.5% profit deviation or missed opportunity >$50.
- Weekly manual review of top 20 largest liquidations + any simulation failures.
- Auto-retrain thresholds (bonus buffer, min HF 1.03-1.07) from live data while respecting pacing caps.

### 6. Failure Mode Coverage & Kill Switches
- Every identified edge case above has a dedicated test that asserts "our sim correctly predicts on-chain revert or low profit".
- Circuit breakers in pacing engine already cover: high gas, too many reverts, daily loss.
- Additional sim-layer guards:
  - Max simulation time (prevent hangs on pathological contracts).
  - Price staleness check (if oracle timestamp too old, drop candidate).
  - Known bad tokens / markets blacklist.
- If sim accuracy drops below 99% over rolling 100 opportunities → auto-pause live execution, require operator review.

## Success Criteria for "Near 100%"
- 100% pass on all golden replay tests (historical liquidations).
- ≥99.5% of sim-approved opportunities succeed profitably on-chain in first 30 days live (measured in Grafana).
- Zero "dust" or protocol-rule violations in any executed liquidation.
- Full coverage of eMode, isolation, bad-debt, close-factor, protocol-fee paths.
- L2 gas + L1 fee estimates accurate enough that realized net ≥ 90% of simulated net on average.

## Implementation Notes for the Vertical Slice
- Pre-filter (fast, pure math in Rust) for HF < 1.05 using cached prices + indexes (from snapshot_generator or periodic refresh).
- Full REVM path: Build exact Pool.liquidationCall (or flash + liquidation) calldata against forked state at current head.
- Must use the same price oracle address and block.timestamp as the fork.
- Cache warm: Pre-load all aToken/vDebt scaled balances + reserve indexes + oracle prices for at-risk users (reduces RPCs dramatically).
- After sim: Re-apply exact L1 fee scalar + current L2 gas price + tip before final "profitable?" decision.
- Output: Structured `LiquidationCandidate { user, collateral, debt, hf, expected_profit_usd_after_all_fees, calldata, gas_estimate }`

This strategy, grounded in the actual Aave v3 source and 2026 L2 MEV realities, guarantees the simulation engine is trustworthy enough to drive live capital allocation under the strict $50 start + risk-control pacing constraints of Project Chimera.

Update this doc whenever new edge cases are discovered in production.
