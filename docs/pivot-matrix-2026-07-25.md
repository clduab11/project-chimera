# Pivot Matrix — Where Project Chimera Can Still Earn

**Date:** 2026-07-25
**Builds on** [venue-landscape-2026-07-25.md](venue-landscape-2026-07-25.md)
**Tooling:** [`scripts/check_venue_open.py`](../scripts/check_venue_open.py) ·
[`scripts/scan_liquidation_venues.py`](../scripts/scan_liquidation_venues.py)
**Status:** 14 agents across 3 workflows; every headline number measured on-chain,
several of them corrections to this document's own earlier drafts.

> **On the ~$300 of sunk API spend:** it is sunk and should carry zero weight. It
> bought the measurements that stopped weeks of building the wrong thing. The
> only question is whether a forward path clears its *future* cost.

---

## 1. The answer, up front

**Every venue measured in this project resolves to one of three failure modes:**

1. **GATED** — Aave V3 (Base + Ethereum), Atlas/SVR, Compound's absorb trigger.
   The trigger price arrives inside a winning solver's bundle. Not a latency
   problem; cannot be fixed with a faster node.
2. **A LATENCY RACE** — Base Morpho (98.8% of its bonus), all six dutch-auction
   venues, crvUSD LLAMMA, Compound's 65.9% same-transaction capture.
3. **DENOMINATED IN COLLATERAL NOBODY HAS PROVEN IS SELLABLE** — AVLT, AZND,
   ROY-ST-apyUSD, ynRWAx.

The residual "open and not a latency race" niche this investigation was hunting
**does exist, is worth roughly $2–3.45M/yr gross — and lands entirely in
category 3.**

---

## 2. The interest-accrual niche: real, and concentrated to a point

Liquidations triggered by debt-side interest accrual generate **no oracle update,
therefore no SVR auction to enter, and no event to race**. Measured by bucketing
every open liquidation as PRICE-PUSH / ACCRUAL / NO-PUSH-FEED:

| Venue | n | Price-push | Accrual | No-push-feed |
|---|---:|---:|---:|---:|
| Ethereum Aave V3 | 174 | 63 / **$154,119** (98.2%) | 110 / $2,861 (1.8%) | 1 / $0 |
| Base Morpho Blue | 120* | 97 / $55,392 (98.8%) | 15 / $380 | 8 / $290 |
| **Ethereum Morpho Blue** | 130* | 2 / $958 (0.3%) | 15 / $11,847 | **113 / $268,245 (95.4%)** |

\* largest-first samples covering 99.4–99.9% of value, rescaled to established totals.

**Not-price-push total: $283,623/30d ≈ $3.45M/yr of structural bonus.**

The evidence is unusually hard. The AVLT/USDC market's oracle
(`0x28b82a…7520`, a RedStone AVLT_FUNDAMENTAL/USD feed) returned a
**byte-identical `latestAnswer` at four blocks spanning the whole window and
emitted zero logs in 30 days.** The collateral price never moved — all 138
liquidations there were caused by interest accrual alone. AZND/USDC has its price
hardcoded to exactly 1.0. ROY-ST-apyUSD and ynETHx rose monotonically, and a
rising collateral cannot trigger a liquidation.

**But:** 96% of it is one venue, **59% is one market**, and that market already
has **32 liquidators with top-1 at 47%**. Median bonus per event is **$71**
(p90 $1,724, max $37,816). It is an open market, not an unattended one.

**Sanity check passed:** the pipeline reproduced Base Aave's known open/gated
split exactly (84 events / $420.05 open vs $420 established; 32 / $79,462 gated
vs $79,451).

---

## 3. Cross-comparison matrix

| Option | Open? | Size /30d | New-entrant /yr | Capital | Repo reuse | Effort | Killer risk | Verdict |
|---|---|---:|---:|---|---:|---|---|---|
| **Interest-accrual — ETH Morpho** | **OPEN, not a race** | **$283.6k** | **$50–250k** *(inferred)* | Low | see §4 | 3-day test, then ~24 d | **Can seized AVLT be sold at all? UNMEASURED** | **Only live candidate — gate it** |
| Audit contests (PoC-mandatory) | n/a | — | $5–30k ordinary active | **None** | 30% code / ~90% artifacts | ~3 hrs/wk | Unmeasured; PoC edge ≠ bug-finding edge | **Run in parallel** |
| Morpho Blue — Ethereum (latency framing) | OPEN | $281k | ~$503k tail | Low | 62% | 20–22 d | 98.8%→ subsumed by the accrual row | Kill the framing, keep the venue |
| Morpho Blue — Base | OPEN | $56k | **$0–2k** | Low | 62% | 8–10 d | 98.8% is the race already lost | Cheapest test only |
| **Compound III `buyCollateral`** | open post-absorb | **$0 — 2 buys, $1.16 total** | ~$0 | — | 45% | — | **Measured empty; 82% gas burn in the reachable band** | **Closed — the $39k figure was wrong** |
| Dutch auctions (Euler v2, Ajna, LlamaLend, Liquity, Sky) | open | Euler pool ~$415k/yr | ~$0 | med–high | 50% | 8–12 wks | **Premise falsified: with flash loans a descending auction IS a latency race**; 22 faster bots; 80% RWA collateral, same unmeasured exit | **Closed** |
| Aave V3 — Base *(current)* / Ethereum | **CLOSED** | $4.4k / $295k | ~$36 / ~$21k | — | 100% / 70% | n/a | SVR sealed auction | **Exit** |
| Atlas SVR solver | gated entry | — | ~0% residual | 0.1–1.5 ETH | 35% | 2–4 wks | Reserved high-rep slot; parallel auctions ⇒ ~50% win | **Closed** |
| Keeper networks | n/a | **$4,147/yr + $675/yr, entire network** | ~$415 / ~$75 **per operator** | — | — | — | Fixed permissioned DON — nothing to join | **Dead — measured** |
| Protocol fixed-fee roles | n/a | Sky 250 DAI flat + 0.1% | ~$3.6k/yr TAM | — | — | — | Sky fired **once**/30d; Liquity V1 **zero** | **Dead — measured** |
| Sell risk data to DAOs | n/a | — | — | — | — | months | Chaos Labs exited Aave April 2026 after 3 yrs of losses | **Dead** |
| Borrower-side liquidation protection | n/a | UNMEASURED | UNMEASURED | None | code **is** the product | weeks–months | It is a startup, and unmeasured as one | Parked |
| **STOP** | — | — | — | none | — | 0 | Opportunity cost only | **The base case** |

> **The skeptic panel's verdict, stated in full because it is the honest read:**
> *"Not one row clears its bar on a measured number. Every row with a large number
> has an unmeasured gate in front of it, and every row with a clean measurement
> has a small or zero number. That is not a portfolio to choose from; it is a
> completed negative result."*

---

## 4. The reuse score was measured against the wrong core

The 92% reuse figure for the accrual niche is **misleading**. The niche is a
**polling problem, not a latency problem** — the AVLT feed emitted zero logs in 30
days, so there is no trigger to subscribe to. Winning means continuously
evaluating health factors over a small known borrower set (AVLT has **22
borrowers**; AZND has **one**).

That inverts which code matters. The swap engine, `Executor.yul`, the mempool
watcher and the oracle-backrun path are latency machinery for a race that does
not exist here. What actually transfers is the **recent** work: the Multicall3
batching and concurrent checkpointed borrower backfill (commits `30e6996`,
`7fef2d2`), the signer registry, the pacing engine, and the ops surface.

`Executor.yul` **cannot be wrapped** — no arbitrary-call primitive, and its entry
point hard-asserts a 420-byte Aave payload. The replacement is **~150 lines of
Solidity**, because Morpho liquidation is callback-financed and the flash-loan
half is unnecessary.

---

## 5. Delete regardless of direction — and it is a code change, not YAML

**Correction to an earlier draft:** these are *not* deletable in YAML.
`Config::validate()` (`core/src/config.rs:188-197`) **hard-rejects**
`max_single_transfer_usd > 1000` and `max_daily_net_usd > 2000`. The "1-day
config deletion" is a **1-day code change**.

1. **`max_single_transfer_usd: 1000`** — and this is worse than a throttle.
   `pacing_engine.rs:135` compares it against `expected_net_usd`, i.e. **net
   profit**. It therefore rejects precisely the tail that carries the value on
   every venue in this matrix while admitting the dust. **It is an anti-selection
   filter, inversely correlated with income.** AVLT's p90 bonus is $1,724 and its
   max $37,816.
2. **`min_interval_hours: 6`** — ≤4 releases/day. Its jitter is **dead code**:
   `release_at` is computed at `pacing_engine.rs:187-189` and its only non-test
   consumer, `orchestrator.rs:639`, matches the variant and discards the payload.
3. **`venue_rotation_count: 5`** — with exactly one eligible Base venue the deque
   can never evict it: **at most one non-same-asset liquidation per process
   restart, forever.**
4. **`max_daily_net_usd: 2000`** *(fourth killer, previously unnamed)* — also a
   hard invariant. Combined ceiling: **$2,000/day**.
5. **`core/src/simulator/prewarm.rs`** — hardcodes Aave's `_reserves` at slot 53
   and overwrites live fork state with fabricated values, producing confidently
   wrong profit numbers. Its own sibling `seeding.rs` condemns the practice.
6. **`core/src/simulator/golden.rs`** — asserts nothing and zeroes prices, so it
   never exercises the pricing code it exists to validate.

Also: `execute_live` books SUCCESS the instant `send_raw_transaction` returns and
never polls the receipt — the revert breaker is dead code and PnL is fiction.

---

## 6. What to do — the two panels converged

**Day 1–3: a zero-code, zero-capital kill test.** Three binary questions,
~40 RPC calls:

- **(a) Can seized AVLT/AZND be sold or redeemed by an arbitrary address?**
  Check the token for a transfer allowlist, pause flag or hook; enumerate
  Uniswap/Curve/Balancer pools; compute routable depth at 1/2/5% slippage against
  the median seized size (~$1,690) and p90 (~$41,000).
- **(b) Trace what the incumbents do with the collateral.** Follow
  `0x6566…0245` (46.7% of AVLT bonus, n=35) and two others. Do they sell, redeem
  through a vault, or simply **hold**? *If they hold, this is a directional long
  on illiquid RWA dressed as arbitrage — kill it.*
- **(c) Is the RedStone feed push or pull?** If the price arrives in the
  liquidator's calldata under an authorised-signer set, **the market is gated,
  not open, and the whole branch closes.**

**If (a) is NO or (c) is PULL, that is terminal — and it should be treated as the
base case, not the sad case.**

**Days 4–6 (regardless of branch): the deletion above**, plus extend
`check_venue_open.py` with the push-feed test — 30-day log count per aggregator
and a two-block drift check — so the tool that *found* this niche becomes the
standing venue-selection gate rather than a one-off.

**Days 7–13: a read-only poller** over the accrual markets. Gate 2 at Day 13:
proceed only if detection recall ≥80%, median lead ≥1 block, ≥3 crossings.
Otherwise stop, having spent 13 days and zero dollars.

**Days 14–30:** ~150-line `MorphoAccrualLiquidator.sol`, fork tests replayed at
the exact blocks of five historical AVLT liquidations (tx hashes known — the
golden-replay this repo never actually had), then live with **$3,000** capital,
no capital committed before Day 18.

**In parallel, ~3 hrs/week:** one PoC-mandatory audit contest. Zero capital, no
shared engineer-hours, and the only line item that can produce a first dollar
independent of the on-chain thesis.

### Honest outcome distribution

| | |
|---|---|
| ~40% | Gate 1 kills it on **Day 3** (no exit, or a pull feed) |
| ~25% | Gate 2 kills it on **Day 13** (poller not competitive) |
| ~20% | It runs and earns **a few thousand dollars a year** |
| ~15% | It clears **$25k/yr** |

**The expected value of this plan is a few thousand dollars a year, not a
business. Its actual value is that it costs three days to find that out.**
