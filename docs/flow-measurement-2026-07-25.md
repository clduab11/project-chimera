# 30-Day Liquidation Flow Measurement — Base Aave V3

**Date:** 2026-07-25
**Status:** measured and independently verified. This is the go/no-go number the
project had never measured.

## Headline

Over the trailing 30 days (blocks 47,819,899–49,115,899, 2026-06-25 → 2026-07-25 UTC),
the Aave V3 Pool on Base (`0xA238Dd80C259a72e81d7e4664a9801593F98d1c5`) emitted
**116 LiquidationCall events** totalling:

| Metric | Value |
|---|---|
| Collateral seized | **$79,773** |
| Debt repaid | $75,324 |
| **Gross liquidation bonus (total revenue pool, pre-gas/pre-swap)** | **≈ $4,450 / month** |
| Last 7 days | 12 events, $8,500 seized, ≈ $405 bonus |
| Unique liquidators | 22 |
| Winner concentration | `0xd128…4b3f` took **85.4%** of seized value |

The pool of revenue that ALL Base Aave liquidators competed for last month is
roughly **$4.5k gross**. The residue left by the dominant bot was ≈ $650 gross.

### Distribution

- 72 of 116 events seized **< $1** (dust); p50 event size $0.04.
- All value sits in 14 events ≥ $1k; largest single event $20.7k seized.
- 9+ days of the month had zero meaningful flow; best day (Jul 1) ≈ $2.5k bonus.
- Collateral mix by USD: cbBTC $35.7k, USDC $34.9k, WETH $9.1k — everything else
  negligible (11 of 13 listed collaterals essentially never liquidated at size).

## Methodology

- Scanner: [`scripts/measure_liquidation_flow.py`](../scripts/measure_liquidation_flow.py)
  (stdlib-only, checkpointed eth_getLogs sweep; topic0
  `0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286` verified via
  `cast sig-event`).
- Pricing: **current** AaveOracle (`0x2cc0…e156`) prices applied to all events.
  Verified within 0.02% of Coinbase spot at measurement time.
- Artifacts: [`docs/data/liquidation-flow-2026-07-25/`](data/liquidation-flow-2026-07-25/)
  (raw logs JSONL, summary JSON, enriched per-event CSV).

### Known artifact of current-price valuation

ETH rallied ~20–25% and BTC ~9% during the window. Events before ~Jul 10 therefore
show distorted per-event bonuses (up to +29% for early WETH-collateral events;
*negative* bonuses for USDC-collateral/WETH-debt events from the Jul 2–3 ETH spike).
Re-estimating the 7 distorted large events at plausible 5–7.5% bonus rates moves the
total from $4,449 to ≈ $4,500 — over- and under-statements offset. The 7 clean large
events (Jul 15–24) land at 4.2–8.4% bonus, exactly Aave's expected band, validating
the pipeline. **Aggregates are trustworthy; individual pre-Jul-10 bonus figures are not.**

## Verification (4 independent checks, all passed)

1. **Independent recompute** from raw logs with fresh code: identical totals
   (116 events, $79,773.23 / $75,324.19 / $4,449.05), zero discrepancies.
2. **Cross-RPC spot check** against public `mainnet.base.org` (not Alchemy): test
   subrange returned exactly the expected 29 events; all 29 matched on
   (txHash, logIndex).
3. **Price sanity**: oracle vs Coinbase spot drift ≤ 0.016%; bonus-band analysis
   as above.
4. **Incumbent recon** (see below).

## The incumbent: `0xd12810b19b596347a3afac206d3ca65d08594b3f`

- **Verified ERC1967/UUPS proxy → unverified implementation** (`0x2b88…40a7`),
  actively upgraded (frequent `upgradeToAndCall`) by operator EOA `0xF16E…8647`.
- Atomic liquidate-and-swap bot: seize collateral → DEX swap → repay → net WETH,
  one tx. No Aave flashloan observed (DEX flash-swap or inventory).
- **Submission: routed through the "Atlas" MEV auction framework
  (`0x583d…9b77`, FastLane Labs-style) via a rotating pool of 6+ high-nonce signer
  EOAs** — managed auction orderflow, parallel nonce lanes. Not a naive public
  mempool racer.
- Priority fees are trivial in absolute terms: median tip ~0.10 gwei (~20× Base's
  0.005 gwei floor) ≈ **$0.20–0.50 per liquidation**. Lands mid-block. Nobody is
  in a gas war because there is nothing worth warring over.
- Sourcing: continuous health-factor monitoring; no evidence of same-block
  Chainlink oracle backrunning (in-block pushes did not match liquidated assets).

## Strategic implication

**The flow does not justify the roadmap.** The conditional next steps (private tx
submission, multi-venue swap engine for 11 collaterals — weeks of work) were
premised on a flow measurement that has now come in at ≈ $4.5k/month gross for the
entire market, of which an actively-maintained incumbent with professional
MEV-auction infrastructure captures 85%. Perfect execution against that incumbent
competes for a residual of roughly **$650/month gross**, before gas, swap slippage,
and infrastructure costs.

Coverage was never the constraint (confirmed earlier: standing liquidatable stock
is $9.15 of dust across 214,180 borrowers). Latency was assumed to be the
constraint; this measurement shows **market size** is the binding constraint on
Base Aave V3 specifically. Options from here, in rough order of expected value:

1. **Measure other venues before building anything** — same 1-day scan against
   Morpho Blue (Base), Compound III (Base), Moonwell, and/or Aave V3 on Arbitrum —
   the scanner generalizes with a pool address + event signature swap. Decide on
   numbers, not vibes.
2. Repurpose the engine's working components (discovery, simulation, execution)
   for a different strategy on Base where flow exists.
3. Mothball: keep the shadow-mode engine as a cheap option on future volatility
   regimes (the $4.5k month included only a mild ETH rally; a crash month would be
   larger — but the incumbent scales with it too).

What is *not* justified by this number: building the swap engine or private
submission for Base Aave V3 alone.
