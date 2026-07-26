# Gate-0 Survey — the exit-denominated re-measurement of everything

**Date:** 2026-07-26 · **Budget:** bounded ~1.5-day survey pass · **Mode:** recommend, do not build
**Decision rule:** fixed in [gate0-decision-rule.md](gate0-decision-rule.md) **before any survey number existed** — ≥$100k/30d of `bonus_exit` surviving Gate-0 across Tiers A–D reopens the strategy; less means the liquidation category is dead for this operator. Tier E scored separately.

Epistemic tags as in the prior briefs: `[MEASURED]` on-chain this pass · `[REPORTED]` from docs/web · `[UNMEASURED]` explicitly unknown.

---

## 0. The decision, up front

**The liquidation category is dead for this operator.** Total `bonus_exit` surviving Gate-0 across Tiers A–D: **≈$69–79k/30d** (the range is Tier D's wide-band Fluid estimate) — and $50.2k of that is Base Morpho price-push flow, the latency race already established as lost. Excluding it, the pollable remainder is **≈$19–29k/30d, fragmented across ~8 venues each carrying an 85–99.7% incumbent**. The accrual niche this survey existed to finish is **under $8k/30d**. The pre-declared threshold was $100k/30d. It is not met on the raw sum and not close on any honest composition.

The $283.6k/30d accrual niche was a mark-to-oracle artifact three different ways:
1. **AVLT ($165.9k attributed):** oracle premium now **+6.04%** `[MEASURED]` (was +4.18% yesterday — the pinned $1.09449 feed diverges further as market drifts). `bonus_exit` = **−$67k/30d**. And independently: the market's borrow collapsed **$4.50M → $43.9k in 30 days (−99%)** — a fixed borrower set being ground down, now nearly exhausted. `[MEASURED]`
2. **AZND ($13.7k attributed):** price hardcoded 1.0 passes the premium check trivially, exactly as the work order predicted — and fails on substance: the best aggregator route on Ethereum turns 1,000 AZND into **186 USDC (−81%)** and 50,000 into **470 USDC (−99%)**. Unsellable. One borrower (holding 58% of total AZND supply) at 100% utilization. `[MEASURED]`
3. **The venue-wide residue:** Gate-0 across **all 241 live-and-relevant Ethereum Morpho markets** leaves **$2,892/30d** of exit-real bonus in PASS markets, plus ~$4.8k in the PT bucket. **~97% of Ethereum Morpho's $278.7k structural bonus evaporates under exit denomination.** `[MEASURED]`

Cost of the answer: ~1 day of scripting and measurement, not a quarter of building.

---

## 1. Task 1 — the scanner fix, and its control proof

### The load-bearing verification (done before any code changed)

`Morpho.sol::liquidate` `[MEASURED at source]`:

```solidity
uint256 collateralPrice = IOracle(marketParams.oracle).price();
seizedAssets = repaidAssets.wMulDown(liquidationIncentiveFactor)
    .mulDivDown(ORACLE_PRICE_SCALE, collateralPrice);
```

Seizure is **denominated at the market oracle price by construction** — confirmed both in Morpho docs ("the oracle price dictates both the liquidation threshold and the exchange rate") and in source. So collateral seized is worth exactly `repaid × LIF` **at oracle prices**, and its market value is `repaid × LIF / (1 + premium)`. An oracle premium is not noise on the bonus — it is subtracted from it, dollar for dollar.

### What changed in `scripts/scan_liquidation_venues.py`

Three numbers per venue and per market, never one:

| number | formula | meaning |
|---|---|---|
| `bonus_oracle` | `seized × (LIF−1)/LIF`, `LIF = min(1.15, 1/(1−0.3×(1−lltv)))` (Aave: per-reserve `liquidationBonus` from the `getConfiguration` bitmap) | what the protocol credits |
| `bonus_exit` | `repaid_usd × (LIF/(1+premium) − 1)` | what an exit realises. Derived from the protocol invariant, so the 30-day *current-price window error* now touches only the loan token (almost always a stable) — this also kills the 63% Base-Morpho inflation |
| `oracle_premium_pct` | `oraclePrice/tradedPrice − 1`, current oracle read vs DefiLlama/aggregator traded price | the wedge between the two |

Checkpointing, provider-span latching, and key redaction untouched. Markets rows now also carry `market_id`, `oracle`, `lltv`, and per-market **median/p90 clip sizes** (feeds Gate-0 quote sizing).

### Validation gate — PASS `[MEASURED]`

| control | want | got | verdict |
|---|---|---|---|
| Base Aave events | 116 | **116** | exact |
| Base Aave seized | $79,790 | $79,845–79,863 | +0.08% (24h DefiLlama drift) |
| Base Aave naive bonus | $4,404 | $4,314–4,316 | −2.0% (same drift, within 3% tolerance) |
| Base Aave top-1 | 85.4% | **85.4%** | exact |
| AVLT collapse | bonus_exit → ~0 | $178,004 → **−$67,394** | collapsed through zero |

Base Morpho cross-check: new `bonus_oracle` = **$57.6k** vs the prior corrected oracle-anchored $50.5k (same order, method now derived from the invariant rather than per-event oracle replay); the legacy naive channel still shows the inflated $150.6k — kept visible as a warning column.

---

## 2. Task 2 — Gate-0 (`scripts/gate0_market_filter.py`)

Three checks, ~5–9 network calls per market, checkpointed to a `.partial.jsonl`:

- **(ii) Oracle premium** — current market-oracle read vs aggregator-quote traded price; fail at |premium| > 1%. *The discriminator, as predicted.*
- **(i) Routable exit** — **KyberSwap** (keyless, widest chains) with **ParaSwap** fallback (keyless), quotes at median clip / 3× / p90 sizes on the **same chain as the debt asset**; fail if 3× median clip not routable under 2% impact; all-aggregators-refuse = `NO_ROUTE` (a fail, not an error). *Odos answered 429; 1inch/0x/CowSwap need keys — noted, not needed.*
- **(iii) Exit type** — `DEX` / `PT_REDEEMABLE` (symbol-parsed maturity; judged on discount-vs-time, never on depth; post-maturity redemption is 1:1 and permissionless `[REPORTED, Pendle docs]`) / `VAULT_REDEEMABLE` (ERC-4626 `asset()` answers) / `BRIDGE_ONLY` / `NONE`.

**The junk-route guard** (added after the smoke test): an aggregator will happily "route" AVLT through 90–99.99%-fee V4 pools at a uniformly terrible price with low *relative* impact. If the quote-implied price diverges >30% from DefiLlama's traded price, the route is scored fake: unroutable, DefiLlama used as traded price. This is what makes AVLT read `BRIDGE_ONLY, premium +6.04%` instead of `DEX, premium +523%`.

Traps honored: no `roundId` filter (RedStone `roundId=1` is structural); transferability not used as a gate.

Supporting tools added: `scripts/enumerate_morpho_markets.py` (CreateMarket sweep + Multicall3 state reads → Gate-0 input rows) and `scripts/assemble_gate0_survey.py` (merges tiers → `config/gate0_survey.json`).

---

## 3. Task 3 — the survey

### Tier A — the accrual thesis, finished `[MEASURED]`

**Enumeration:** 1,677 markets ever created on Ethereum Morpho · 1,009 with live borrow · **241 Gate-0 candidates** (≥$10k borrow or any 30d liquidation) · 768 recorded-but-skipped below the floor (declared cap, not silent).

**Ethereum Morpho Gate-0 (241 markets):** 137 PASS · 52 FAIL · 32 REVIEW_PT · 20 REVIEW_VAULT.

| bucket | 30d bonus_exit | comment |
|---|---:|---|
| PASS markets with 30d liq history (19) | **$2,892** | largest: WBTC/EURCV $1,216; WBTC/USDC $944; rswETH/msETH $333 |
| REVIEW_PT (32 mkts) | ~$4,770 | PT-apyUSD-5NOV2026/USDC: $8.8k oracle → $3.2k exit at +2.52% discount, 101d to maturity ⇒ ~9.4%/yr carry, not an inefficiency. PT-stcUSD-29JAN2026 matured 179d ago, discount 0.003% ⇒ nothing left to converge |
| REVIEW_VAULT (20 mkts) | ~$79k *nominal* | entirely one event: ynETHx/wstETH ($79.8k, single liquidation, NAV premium just +0.015%). Real *if* ynETHx redemption at NAV is prompt & permissionless — queue/lockup `[UNMEASURED]`; one event in 30d is not a business |
| FAIL, the phantom bucket | — | AVLT $178k (BRIDGE_ONLY, +6.04%), ROY-ST-apyUSD (oracle +5,118% vs traded — worse than AVLT), apyUSD (+1.98%), wbravUSDC (−8.29%) |

**The four named accrual markets, final:** AVLT dead (premium + bridge + 99% wound down) · AZND dead (unsellable) · ROY-ST-apyUSD dead (premium +5,118%) · ynETHx = one real-looking event behind an unverified vault-redemption path.

**Base Morpho Gate-0:** 8/10 PASS, **$50,171/30d bonus_exit** (VVV/FXUSD $23.8k, cbBTC/USDC $19.2k, cb-asset tail). Honest oracles, real exits — but 98.8% of this venue is price-push flow: the latency race already lost. Open ≠ winnable.

### Tier B — Aave V3 on non-SVR chains `[MEASURED]`

Openness gate: **every reachable chain is OPEN** — Optimism (`AccessControlledOffchainAggregator 3.0.0`), Polygon, Avalanche (OCR2 1.0.0), Linea, Gnosis, Scroll, Metis, Sonic, Celo. zkSync Era unreachable `[UNMEASURED]`. Shelf-life risk stands: Aave's ARFC lists pending SVR expansions `[REPORTED]`, so anything open here can close on governance schedule.

30-day flow on the open chains (the number that matters):

| chain | events | seized | bonus_oracle | bonus_exit | top-1 | verdict |
|---|---:|---:|---:|---:|---:|---|
| Polygon | 262 | $82,508 | $6,285 | **$5,528** | 84.9% (89.4% ≥$1k) | Base-Aave shape: one incumbent, dust tail |
| Avalanche | 27 | $47,078 | $2,908 | **$2,643** | 87.1% | same shape |
| Gnosis | 12 | — | — | **~$2.5k** | 99.7% | one liquidator owns the chain; gate0 spot-check: WETH→GNO `NO_ROUTE` via keyless aggregators |
| Optimism | 38 | $4,547 | $217 | **$192** | 54.2% | dead |
| Linea, Scroll, Metis, Sonic, Celo | — | — | — | — | — | open at the gate; flow `[UNMEASURED]` inside budget — all are sub-Gnosis TVL Aave deployments |

**Tier B total: ~$10.9k/30d of open bonus_exit, each chain carrying an entrenched 85–99.7% incumbent.** The "largest unexplored surface" is a collection of Base-Aave-sized ponds that are open only because they are not worth auctioning.

### Tier C — Morpho Blue on other chains `[MEASURED]`

Correction to the work order: **the canonical `0xBBBB…FFCb` address has code only on Ethereum/Base.** Arbitrum/Optimism/Polygon/Unichain/Katana run per-chain deployments (verified via `eth_getCode` + Morpho's API).

| chain | events 30d | bonus_oracle | bonus_exit | note |
|---|---:|---:|---:|---|
| Arbitrum (38.1% window, partial) | 127 | $2,035 | **−$46k** | 118 of 127 events are thBILL/USDC at **+1.86% RWA oracle premium** — the AVLT pattern again |
| Optimism | **0** | $0 | $0 | dead venue |
| Polygon | 70 | $511 | $286 | dust |
| Unichain | 4 | ~$0 | ~$0 | wstETH/WETH only |
| Katana | 25 | $16 | $15 | dust |

### Tier D — other lending protocols (one pass) `[MEASURED unless tagged]`

| protocol | verdict | evidence |
|---|---|---|
| Venus (BNB, $1.03B TVL) | **CLOSED** | BTCB/WBNB/ETH all walk to `DualAggregator 1.0.0` — SVR |
| Euler v2 | CLOSED (carried) | dutch auction = latency race; 22 faster bots; 80% RWA |
| Fluid | OPEN-TINY | 31 ev/30d; bonus O($1–10k); WBTC→USDT passes Gate-0; penalty 0.1–3% by design |
| Moonwell (Base) | OPEN-TINY | 242 ev, ~$4.6k/30d bonus; gate OPEN; Gate-0 PASS — Base-Aave-sized pool |
| Spark (Eth) | OPEN-TINY (carried) | $71.9k/30d but top-1 100%, one event |
| Zerolend | DEAD | 0 liquidations/30d, $3.2M TVL — open door, empty room |
| Benqi (Avax) | DEAD | ~$430/30d bonus (9.2% sample); mostly $3 dust |
| Silo V2 / Radiant / Term / Notional / Seamless / Compound III | DEAD | TVL-kill or measured-zero |
| Tectonic (Cronos) | `[UNMEASURED]` | no Cronos RPC; not actionable |

**Tier D roll-up: ~$5–15k/30d open + passing, fragmented across Moonwell and Fluid.**

---

## 4. Tier E — adjacent classes (reported separately, not scored)

One-liners from the desk-research pass (sources in `tier_e_verdicts.md`):

1. **Pendle PT convergence: DEAD** as an arb — it is market-rate carry (~5–15% implied APY), not an inefficiency — repo transfers: no.
2. **Liquity V2 BOLD redemptions: LATENCY-RACE**, pool ~$150–230k/yr protocol-wide — too small to fight for.
3. **crvUSD/LLAMMA: LATENCY-RACE** (confirms prior); PegKeeper caller-share is real but dust.
4. **Ajna: DEAD** — mechanism *is* polling-and-capital (bonded kicks, 72h auctions) but <$1M TVL, ~$8.8k/yr fees.
5. **LlamaLend: LATENCY-RACE** for the flow that matters; hard-liq tail is polling-friendly but rare-by-design.
6. **Perp-DEX liquidators: GATED or unpaid** everywhere except Drift — which is a sub-65ms Solana race.
7. **Gearbox: POLLING-FRIENDLY and permissionless — the one mechanical fit** — but $18.8M TVL puts the premium pool plausibly <$50k/yr `[TVL REPORTED; flow UNMEASURED]`. **Watchlist trigger, not a work order:** re-rate if Gearbox TVL recovers toward 2024 levels.

**No Tier E work orders.**

---

## 5. Task 4 — decision against the pre-declared threshold

**Threshold:** ≥$100k/30d of Gate-0-surviving `bonus_exit` (fixed in advance in [gate0-decision-rule.md](gate0-decision-rule.md)).

**Measured:** ≈**$69–79k/30d** total across Tiers A–D — composed of ~$50.2k Base Morpho (a latency race, not winnable by polling), ~$2.9k Ethereum Morpho PASS, ~$10.9k Tier B (Polygon $5.5k / Avalanche $2.6k / Gnosis $2.5k / Optimism $0.2k, incumbents at 85–99.7%), ~$300 Tier C, ~$5–15k Tier D fragments (Moonwell, Fluid). Excluding the latency-race flow — which this operator cannot win and the entire prior investigation established as lost — the pollable remainder is **≈$19–29k/30d, fragmented across venues that each cost more to enter than they pay, every one of them already owned**.

**Verdict: the interest-accrual niche and the liquidation category as a whole are dead for this operator.** The niche's headline number was an artifact of oracle-denominated seizure accounting; its two flagship markets were, respectively, negative after the premium and unsellable at any size; and the flagship market has physically wound down (−99% of borrow) inside the measurement window. This answer cost ~1.5 days. Stopping here is the outcome the plan priced in as its base case.

**What survives on merit:** the measurement toolchain itself — the three-number scanner, Gate-0, the enumerator, and the survey assembler — which turned a quarter-scale question into a day-scale one, twice.

---

## 6. Operator actions (reported, not run)

- **`/commit`** — nothing from this or the prior investigation is committed. New this pass: `scripts/gate0_market_filter.py`, `scripts/enumerate_morpho_markets.py`, `scripts/assemble_gate0_survey.py`, the scanner fix, `config/gate0_survey.json`, `docs/gate0-decision-rule.md`, this report.
- **`/schedule`** — a monthly re-run has genuine option value at near-zero cost: SVR coverage is expanding (each expansion closes a venue), Morpho markets open continuously, and the Gearbox watchlist trigger needs a periodic TVL read. The whole re-run is: scanner → enumerator → Gate-0 → assembler.
- **`/revise-claude-md`** — worth persisting: the exit-denomination invariant (never value an oracle-denominated seizure at oracle prices), the junk-route guard rationale, and the per-chain Morpho address correction.
- **BigQuery** — still not authorized in this session (`plugin:finance:bigquery` requires auth); it remains the single tooling change that would make Tier B/C sweeps trivially exhaustive. zkSync Era and Cronos stay `[UNMEASURED]` until then or until keys exist.
- `/code-review ultra`, `/security-review` — moot; no build is recommended.

## Appendix — artifacts

- `config/gate0_survey.json` — every surveyed market/venue row (Tiers A–D)
- Scratchpad (session): `venues30/venue_report.json` (three-number venue report), `tier_a/gate0_ethereum_morpho.json` (241 rows), `tier_a/gate0_base_morpho.json`, `tier_a/morpho_markets_ethereum.json` (full enumeration), `tier_b/openness.json`, `tier_c/tier_c_rows.json` + per-chain analyses, `tier_d/tier_d_rows.json` + findings, `tier_e/tier_e_verdicts.md`
