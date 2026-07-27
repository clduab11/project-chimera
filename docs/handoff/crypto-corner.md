# Crypto Corner

> A durable personal reference for one solo technical operator.

This document exists because Project Chimera — an MEV-focused crypto project — investigated five separate strategies and returned zero revenue on all five. Every strategy failed for the same structural reason, not for technical ones: the code worked, the market position didn't. The purpose of this reference is to compress what that cost into something that makes the next crypto decision faster, cheaper, and better-informed. It is written for one reader, assumes technical fluency, and optimizes for decision quality over completeness.

---

## How to use this

This is a **living document**, meant to be extended with Notion AI as new territory gets explored. Each section is self-contained — jump straight to what's relevant and read it cold, without needing the rest.

**Section 1 (Market structure first) is the one to re-read before ANY new crypto project.** Not skim — read. It is the section that would have prevented Chimera. The other seven sections are reference material you pull when you need them; Section 1 is a gate you pass through before starting.

---

> [!WARNING]
> **Freshness: current as of July 2026.**
> Crypto market structure, chain parameters, protocol versions, and fee economics move fast — quarterly at minimum, sometimes weekly. Treat every specific number, address, version, and named venue here as *stale until re-verified*. Re-verify anything load-bearing before you act on it or commit capital to it.

---

> [!IMPORTANT]
> ### The one-paragraph summary
>
> In crypto market-microstructure niches, the surplus goes to whoever holds **exclusive order flow**, **the block-building seat**, or **a colocation budget** — none of which is code. Writing better software does not move you up that stack; it only lets you compete for whatever residual the holders of those three things have already declined to take. Before starting anything, identify which of the three the incumbents hold, and whether you can hold one too. If the answer is "none of them, but our execution will be sharper," that is not an edge — that is the Chimera failure mode, and it is worth zero.

---

## Contents

1. [Market structure first — how to evaluate ANY crypto opportunity](#1-market-structure-first--how-to-evaluate-any-crypto-opportunity)
2. [Project Chimera — what happened, and the rules that came out of it](#2-project-chimera--what-happened-and-the-rules-that-came-out-of-it)
3. [Chains — structural cheat sheet](#3-chains--structural-cheat-sheet)
4. [DEX architectures — what you must know before integrating one](#4-dex-architectures--what-you-must-know-before-integrating-one)
5. [Smart contracts — patterns, hazards, and when to write your own](#5-smart-contracts--patterns-hazards-and-when-to-write-your-own)
6. [Lending protocols and liquidations](#6-lending-protocols-and-liquidations)
7. [MEV — the honest map](#7-mev--the-honest-map)
8. [Exchanges, stablecoins, custody, and operational security](#8-exchanges-stablecoins-custody-and-operational-security)

---

## Market structure first — how to evaluate ANY crypto opportunity

Every venue survey in this project reached the same answer by a different route: **the surplus in a crypto microstructure niche does not go to whoever writes the best code.** It goes to whoever holds a position in the flow. Code quality was the only asset on hand, and no venue was found where code quality was the binding constraint. This section is the pre-build checklist that would have produced that answer in a day instead of a quarter.

### The three things that capture surplus

Before estimating a number, establish which of these three the venue's surplus is routed through. It is always at least one.

| Capture point | What it is | How to check it, cheaply | If it applies to someone else |
|---|---|---|---|
| **Exclusive / private order flow** | The trigger or the trade is visible to a closed set before it is public | Decode one winning transaction. Does the price update or user intent arrive *inside the same tx* as the profit-taking? Is the entry point a registered/reputation-gated auction? | You are structurally blind. No RPC upgrade fixes it |
| **The building / sequencing seat** | Whoever orders transactions can insert their own | Who builds blocks? Centralized sequencer, PBS builder market, or leader schedule? Can an outsider bid for placement, or only for inclusion? | You buy placement at auction; the seat holder gets it at cost |
| **Colocation budget** | Physical proximity + tuned stack, measured in single-digit ms | Is the ordering rule first-come-first-served? What is the p50 gap between the winner and 2nd place? | It is a spending contest, and yours is the smallest budget |

The measured example: Base Aave V3 liquidations route through [FastLane Atlas](https://github.com/FastLane-Labs/atlas) v1.6.4 serving [Chainlink SVR](https://docs.chain.link/data-feeds/svr-feeds). In the flagship winning transaction, `log[0]` is the oracle round update and `log[14]` is the liquidation — same transaction. The price that creates the opportunity is not on a public RPC until 30 seconds later via fallback. That is capture point 1, and it is not a latency problem.

**A two-call gate worth running before anything else** on any Aave/Compound/Venus-shaped venue: call `getSourceOfAsset` on the protocol oracle, then `typeAndVersion` on the returned aggregator. If it answers `DualAggregator`, the trigger is auctioned before it is visible — drop the venue in under a minute. (See `docs/venue-landscape-2026-07-25.md`.)

### The single most important question: latency race or valuation auction?

Ask it first, because it determines whether *any* amount of engineering can matter.

- **Latency race** — the winner is whoever's packet arrives first. Determined by the chain's ordering rule.
- **Valuation auction** — the winner is whoever bids most. Determined by a sealed-bid or block-auction layer sitting above the chain.

Read it off the ordering rule (as of mid-2026 — this layer changes fast and every row here should be re-verified before use):

| Venue / chain | Ordering rule | Therefore |
|---|---|---|
| Base, OP Stack chains | Single centralized sequencer, FCFS, no public mempool | Latency race to one endpoint. A "private RPC" buys nothing — there is nothing to be front-run from |
| Arbitrum One | Sequencer FCFS, sub-second blocks | Latency race |
| Ethereum L1 | PBS / builder market | Valuation auction. Speed is cheap; bid value and flow exclusivity are the edge |
| Solana | Leader schedule + bundle auctions | Both — colocation *and* bid |
| Any protocol-level auction (Chainlink SVR/Atlas, CoW, RFQ, Euler v2 Dutch) | Explicit auction, often registration-gated | Valuation auction with an entry gate |

**Why the answer decides everything.** Lose the latency-race branch and you are outspent. But the auction branch is worse than it looks: a **first-price auction against a zero-marginal-cost incumbent clears at ~100% of the prize**. Measured directly — the winning solver's ETH balance rose about **$0.11** on a liquidation, while Atlas paid the bundler ~$0.47. The reason only one serious bidder shows up is not that the opportunity is undiscovered. It is that there is nothing left to win.

### Total addressable value vs residual value

Chain-wide gross figures are the single most misleading number in this domain. They overstate the reachable prize four separate ways:

1. **Already captured.** Base Aave V3 ([`0xA238Dd80C259a72e81d7e4664a9801593F98d1c5`](https://basescan.org/address/0xA238Dd80C259a72e81d7e4664a9801593F98d1c5)) produced **$4,404 of gross liquidation bonus over 30 days** across 22 liquidators. Top-1 took 85.4%; real top-3 took 99.2%. Everything beyond the top three was **$604 per 30 days** — roughly $36/yr after you subtract the part that is dust.
2. **Denominated wrong.** Seizure is credited at *oracle* prices; you realize at *exit* prices. Re-denominating Ethereum Morpho's $277.3k of 30-day structural bonus against real aggregator routes left **$2,892** — about 99% evaporated. One market's headline $178k became **−$67k** once a +6.04% oracle premium was subtracted.
3. **Dust.** 72 of 116 Base Aave events seized under $1; median event was $0.04. Gross figures count them; they cost more to take than they pay.
4. **One-offs.** A $79.8k "bucket" that is a single vault unwind is not a business. Strip non-recurring unwinds before believing any tail estimate.

Work with this instead:

> **Residual ≈ Gross × (1 − top-3 share) × (1 − dust share) × (exit price / oracle price) − gas − slippage − infra**

Then compare the residual to your **infrastructure floor**, not to zero. Base Aave scored **−$6.0k to −$1.1k per year** net of infra. A venue can be genuinely open and still be worth less than its RPC bill.

### Measuring a niche in under a day with eth_getLogs

This costs a free-tier RPC key and a few hours of stdlib scripting. It is the highest-return work available and it must come **before** any contract, simulator, or runbook.

1. **Get the event signature.** `cast sig-event "LiquidationCall(address,address,address,uint256,uint256,address,bool)"` → topic0. For Base Aave that is `0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286`.
2. **Sweep 30 days.** Chunked `eth_getLogs` starting at 50,000 blocks, halving to a 500-block floor whenever the provider rejects the range, growing back 1.5× after successes, checkpointed to disk so a crash costs nothing. 30 days on Base ≈ 1.3M blocks. See `scripts/measure_liquidation_flow.py`.
3. **Compute five numbers, never one:** event count · gross value · **exit-denominated** value (quote the actual collateral→debt route at the median clip size on a real aggregator) · **top-1 / top-3 concentration** · dust fraction.
4. **Attribute to `tx.origin`, not `msg.sender`.** A public router is not a competitor. Ethereum Morpho's apparent rank-2 "liquidator" at 25.3% was Morpho Bundler3 — an open-source multicall with 12+ distinct callers. Getting this wrong makes contestable venues look closed and vice versa.
5. **Run the persistence test** — the check that separates the two worlds:
   - Take the largest events. Re-evaluate the eligibility condition via `eth_call` pinned to **block N−1**. Eligible already? The opportunity **persisted** and is pollable — code and diligence can win it.
   - Not eligible at N−1? It was **created inside block N**. Confirm by reading the winning tx's own receipt: a price update in the same transaction means an auction, not a race.
   - Persistent flow is the only flow a solo operator with no seat can address.

### Pre-committed decision rules

Write the threshold in a dated file, commit it, **then** look at the data. Measurement is motivated: a $283.6k headline is very easy to rationalize into a "go" after the fact, and that exact number was an accounting artifact three different ways. Pre-commitment is what made a negative answer cheap and fast (`docs/gate0-decision-rule.md`, written before any survey number existed).

```
DECISION RULE — <venue/class>            written <date>, BEFORE any measurement
Metric:      <exit-denominated, post-cost, 30-day>
Threshold:   >= $<X> / 30d survives <named filters>
If met:      <report and re-open with a fresh brief — NOT "start building">
If not met:  <declare dead, in writing, and stop>
Excluded:    <flow types that don't count, e.g. auction-gated, one-off unwinds>
Budget:      <hard cap in days; if exceeded, answer is NO by default>
```

Two properties make this work: the pass branch says *report*, not *build* (one more gate before spending), and the budget cap makes silence a failure.

### Retail forex/CFD vs crypto MEV — why one paid and the other did not

| | Retail forex / CFD | Crypto MEV |
|---|---|---|
| **Counterparty** | A dealer quoting you a price, obliged to fill within a best-execution regime | Whoever wins the block — no obligation to anyone |
| **Who you compete with** | Nobody for *your* fill. Another trader taking the same setup does not take it from you | Exactly one winner per event, decided by seat, bid, or milliseconds |
| **Rivalry** | **Non-rival.** A thousand traders take EURUSD at ~the same price | **Perfectly rival, winner-take-all.** Second place earns zero and may still pay gas |
| **Binding constraint** | Model edge, risk discipline, capital | Flow access, sequencing seat, colocation |
| **Where code helps** | Everywhere — execution, sizing, backtests. Code *is* the edge | Only after you already hold a seat. Code is table stakes, not an edge |
| **Scaling** | With capital, at roughly constant edge until size moves price | Not with capital at all — with flow access |

**Rivalry is the load-bearing difference.** In a non-rival market, code quality can be the binding constraint, because your edge is not confiscated by someone else's faster packet. In a winner-take-all market it never is. Before entering any new niche, ask: *if a better-resourced competitor also finds this, do I still get paid?* If the honest answer is no, the seat is the product and code is overhead.

### Opportunity smell test

Run in order; any single failure is grounds to stop.

- [ ] **Persistence:** does the opportunity exist in block N−1's state, or is it created inside block N? Created in-block → drop unless you hold a seat.
- [ ] **Gate check:** is the trigger routed through a registered auction (`DualAggregator`, Atlas, RFQ, reputation-slot)? → drop.
- [ ] **Ordering rule:** FCFS sequencer without colocation → drop.
- [ ] **Residual, not gross:** is value beyond top-3, exit-denominated, net of dust, greater than 5–10× your infra floor?
- [ ] **Recurrence:** does the headline survive removing the largest single event? If one event is >50% of it, it is not a business.
- [ ] **Concentration read honestly:** are the top addresses real entities, or public routers?
- [ ] **Rivalry:** if a better-funded competitor finds the same thing, do I still get paid?
- [ ] **Pre-commitment:** is the threshold written down and dated *before* the data exists?
- [ ] **Budget:** can I answer this in ≤2 days of measurement? If the answer needs a build to find out, the answer is no.

### Red flags for a solo operator

- **"The market is open / permissionless."** Open ≠ winnable. Base Morpho's $50.2k/30d is fully open and is a latency race already lost. Gnosis Aave is open with one liquidator holding 99.7%. Venues are frequently open *because* they are not worth auctioning.
- **A large TVL or chain-wide gross figure quoted as the opportunity.** It is neither residual nor exit-denominated. Assume 90–99% evaporates until you have re-derived it against a protocol invariant.
- **"We just need to be faster."** If the trigger is not public before the winner acts, speed is irrelevant. Confirm which world you are in before optimizing anything.
- **Documentation and infrastructure accumulating ahead of measurement.** Twenty-two documents, five remediation waves, a threat model, and seven runbooks preceded the one day of Python that invalidated the roadmap. Measurement first is not a process preference — it is the whole edge a solo operator has, because it is the one activity where being small costs nothing.
- **Building because the code is nearly done.** Sunk cost. The only asset that ever changed a decision here was the measurement tooling.


---

## Project Chimera — what happened, and the rules that came out of it

### What happened

The goal was to reproduce, in crypto, the outcome of a previous profitable retail-forex bot (`oanda-autobot`) — this time using atomic flash-loan execution, where a trade that doesn't profit simply reverts and costs only gas. On paper that is a strictly better risk profile than forex. The thesis was: if execution risk is eliminated, and the code is good, revenue follows.

What got built was real. The engine is ~18,465 lines of Rust across `core/src` — opportunity detection, REVM fork simulation for pre-trade validation, pacing and risk controls, a multi-EOA signer registry, Prometheus metrics, JSONL persistence, 318 passing tests. On-chain, a 387-line Yul `Executor` deployed to Base mainnet, doing flash-loan → Aave `liquidationCall` → DEX swap → repay in a single atomic transaction. It ran live for six days.

It performed **zero liquidations**.

Then came the part that actually mattered: five separate strategy and venue investigations, each with a written brief, each returning negative.

| # | Investigation | Finding | Doc |
|---|---|---|---|
| 1 | Base Aave V3 liquidations | ~$4,450/mo gross bonus for the **entire market**; incumbent takes ~85%; ~$650/mo residual. Settled by a Chainlink OEV / Atlas sealed-bid auction, not a latency race — the trigger is auctioned before it is visible on-chain. | `docs/flow-measurement-2026-07-25.md` |
| 2 | Accrual / AVLT niche | An apparent $283.6k/30d opportunity was a mark-to-oracle artifact. AVLT oracle premium +6.04%, real `bonus_exit` **−$67k/30d**; borrow collapsed $4.50M → $43.9k (−99%). AZND unsellable: 50,000 tokens route to 470 USDC (−99%). | `docs/gate0-survey-2026-07-26.md` |
| 3 | "Bespoke Tranche Opportunity" (self-sandwich) | No profit source. You cannot sandwich yourself: gross is exactly zero, net is negative after fees. Proven analytically, then confirmed by a 6,125-parameter sweep. | `docs/decision-2026-07-26-tranche-venue.md` |
| 4 | Cross-chain sandwich venue survey | No chain works. Base and Arbitrum have no public mempool; BSC filters the pattern; Ethereum is picked over by builders. | `README.md` §2.5 |
| 5 | Per-chain atomic arbitrage | Every chain CLOSED or MARGINAL for a new entrant. Best case Polygon, ~$17k/mo residual, against a 3–4 month build. | `README.md` §2.5 |

Total spend: ~$350 in compute plus several months of labor. Total revenue: **$0**.

### What actually went wrong

Separate these two, because conflating them produces the wrong lesson.

**The strategic error (fatal):** the operator picked a market where code quality was not the binding constraint. In crypto market-microstructure niches the surplus reliably goes to whoever holds one of three things: exclusive or private order flow, the block-building/sequencing seat, or a colocation budget. None of those is code. The operator's only asset was code. Investigation #1 is the cleanest demonstration — the Base Aave liquidation trigger is resolved in a sealed-bid OEV auction, so there is no latency race to win no matter how fast the Rust is. A perfect engine and a mediocre engine score identically: zero.

The subtler version of the same error: **the atomic-revert property was mistaken for an edge.** Atomicity removes execution risk, which is exactly why it is table stakes — everyone competing has it. It removes your downside and therefore removes your ability to be paid for bearing risk. Six days of zero fills is not a bug report; it is the market correctly reporting that there was nothing to fill.

**The execution quality (sound):** the engineering was not the problem. 318 passing tests, REVM fork simulation, a Yul executor small enough to audit by hand, per-venue measurement scripts, adversarial self-review that overturned this project's own first-pass numbers. If the venue had been real, this stack would have traded it. That is worth saying plainly, because the temptation after a $0 outcome is to conclude the work was bad. It wasn't. It was aimed wrong.

### What was done right

- **The kill threshold was written before the data existed.** `docs/gate0-decision-rule.md` is dated and pre-committed: ≥$100,000/30d of exit-denominated bonus surviving Gate-0 reopens the strategy; below that, the category is declared dead. The actual result was ≈$69–79k/30d — and stripping out the Base Morpho flow already known to be a lost latency race, ≈$19–29k/30d. The threshold could not be fitted to the answer because it was fixed first.
- **Measurement before building — eventually.** The Gate-0 survey cost ~1.5 days and killed a strategy that would otherwise have consumed a quarter. That is the correct ratio. The error was doing it after the engine, not before.
- **Strategies were killed with numbers, not vibes.** The self-sandwich died to an analytic proof plus a 6,125-parameter sweep, not to a hunch.
- **Negative results were published, not buried.** Every brief lives in the repo, including the corrections. `docs/gate0-survey-2026-07-26.md` contains an inline correction overturning its own "$278.7k / ~97%" figures. That habit is rarer than working code.

> **Red flags — recognize these earlier next time**
> - The engine is green, dashboards are healthy, tests pass, and the fill count is zero. **Telemetry measuring your own machine, not your P&L, will improve right up to insolvency.**
> - A headline opportunity size that shrinks by 90%+ the moment you denominate it in what you could actually *exit* at (oracle price vs. aggregator-routable price).
> - Any structure where the counterparty on the other side of your profit is… you. If you can't name who loses the money you're winning, there is no money.
> - "Residual after the incumbent" figures. A $650/mo residual is not a business; it is a rounding error that still requires you to beat the incumbent on the axis you have no budget for.

### THE RULES

1. **Ask first: is code the binding constraint in this market?** Write the answer down. If the constraint is licensing, order flow, capital, relationships, colocation, or a sequencing seat — walk. Your comparative advantage must be the thing that's actually scarce.
2. **Measure the residual before writing a line.** Not TAM, not gross flow. Gross × (1 − incumbent share), denominated in what you could actually exit at, net of infra. Base Aave: ~$4,450/mo gross → ~$650/mo residual. That number was available on day one.
3. **Write the kill threshold before you look at any data**, with a date on it, in a file you commit. `gate0-decision-rule.md` worked. Do it at project start, not month four.
4. **Time-box the survey to ~1–2 days and honor it.** The survey that killed the accrual niche cost a day. The engine that preceded it cost months.
5. **Name the loser.** For any strategy, write one sentence identifying who pays you and why they can't avoid it. If the sentence can't be written, the strategy has no profit source — that alone would have killed the self-sandwich on day zero.
6. **Distrust structures whose telemetry improves while the account drains.** Instrument revenue as the primary metric from the first commit. Fills, dollars, realized P&L. Latency percentiles and test counts are hygiene, not evidence.
7. **Prefer being a counterparty to being a racer.** Racing means the fastest participant takes everything and everyone else takes zero — a winner-take-all payoff a solo operator cannot win. Being a counterparty (providing something scarce, bearing a risk others won't) pays a spread that survives not being first.
8. **Treat "risk-free" as a warning.** Atomic revert, zero-downside, guaranteed-or-refunded structures are commodity table stakes. Returns compensate for risk borne or scarcity supplied; if you bear no risk and supply nothing scarce, expect zero.
9. **Never let sunk engineering justify the next survey.** The correct response to "we already built it" is to re-run the venue test as if the repo didn't exist.
10. **Pick markets where the incumbent set is unknown or absent, not merely beatable.** If measurement shows 85–99.7% concentration in one actor, that actor holds a structural asset you don't, and speed won't dislodge it.
11. **Denominate everything in exit price.** Oracle price, mark price, and quoted price are opinions. The aggregator route for your actual clip size is the fact. This single reframe erased ~99% of a $277k figure.
12. **Keep the adversarial self-review.** It was the most valuable practice in the project and it transfers to every future one. Assume your own first-pass numbers are wrong and try to break them before someone else's money does.

### If you ever revisit crypto

Not "never." But the bar is now explicit. All four of these would have to be true **before** any build starts:

| Condition | Test |
|---|---|
| You hold one of the three surplus-capturing assets | Documented private order flow, a builder/sequencer relationship, or a colocation budget you can actually fund. Not "we'll get one later." |
| Measured residual clears a real hourly rate | Residual (post-incumbent, exit-denominated, net of infra) ≥ 10× your build cost in the first year. Polygon at ~$17k/mo against a 3–4 month build is borderline at best, and that's the *best* case found across five surveys. |
| The venue isn't auction-settled against you | If the trigger is resolved in a sealed-bid OEV/Atlas-style auction before it's publicly visible, you are not in the game regardless of latency. |
| You can name the loser | One sentence, written down, identifying who pays and why they can't route around you. |

Absent all four, the honest answer is that this is a market where a strong solo engineer's best asset is systematically not the one being paid. That is not a defeat — it is a specification. It tells you exactly what the next project should look for: a market where the scarce thing is the thing you can build.


---

## Chains — structural cheat sheet

The only question that matters before writing a line of on-chain trading code: **who decides the order of transactions in the next block, and what do they sell that seat for?** Everything else — TPS, TVL, dev experience — is downstream. All figures below are as of **July 2026**; the ordering layer on several of these chains changed materially in the last 18 months, so re-verify anything load-bearing before committing engineering time.

### Table 1 — Who orders transactions

| Chain | Chain ID | Block time | Who orders | Ordering rule | Public mempool? |
|---|---|---|---|---|---|
| Ethereum mainnet | 1 | 12s slots | Competitive builder market → relay → proposer | Sealed-bid block auction (MEV-Boost); within a block, builder's discretion | **Yes** (plus large private flow) |
| Base | 8453 | 2s canonical, 200ms Flashblocks | Single sequencer (Coinbase; TEE `base-builder`) | Priority fee **at time of Flashblock selection**, not globally | **No** |
| OP Mainnet | 10 | 2s canonical, 250ms Flashblocks | Single sequencer (OP Labs) | Same as Base | **No** |
| Arbitrum One | 42161 | ~250ms | Single sequencer (Offchain Labs) | **FCFS by arrival timestamp** + Timeboost express lane | **No** (sequencer feed is post-ordering) |
| Polygon PoS | 137 | ~2.0–2.3s, ~5s finality | **One validator-elected block producer per span** (VEBloP, Rio) | Producer's discretion; gas-price priority by default | **Yes** |
| Avalanche C-Chain | 43114 | Dynamic, sub-second capable (ACP-226) | Whichever validator wins the slot | Priority fee, per-validator mempool | **Yes** (gossip-based) |
| BNB Smart Chain | 56 | **0.45s** (Fermi, Jan 2026) | Builder market → validator (BEP-322 PBS) | Sealed-bid builder auction, **sandwich-filtered** by GWA builders | **Yes** |
| Solana | n/a (`solana:5eykt4Us…`) | 400ms slots | Leader validator; Jito Block Engine / BAM in practice | Off-chain bundle auction by tip; on-chain priority fee + FIFO-ish | **No** |

### Table 2 — What you can actually execute

| Chain | Atomic bundles? Provided by | Typical cost, simple swap | One line for a solo builder |
|---|---|---|---|
| Ethereum | **Yes** — Flashbots, Titan, beaverbuild, BuilderNet | ~$1–5 (2–10 gwei) | Bundle submission is permissionless; *winning* is not — top 5 builders take ~97% of MEV blocks |
| Base | **No public bundle market** — atomicity only inside one tx | <$0.01–0.05 | You cannot buy position; you can only be early. Two entities already dominate Base MEV |
| OP Mainnet | Same as Base | <$0.01–0.05 | Same structure, ~10x less flow. Not a better version of Base — a smaller one |
| Arbitrum One | **No** — one tx, or win the express lane | ~$0.02–0.05 | Gas price buys you nothing. Latency or a 60-second auction seat, or you lose |
| Polygon PoS | Partial — FastLane/Atlas backrun auctions | ~$0.002–0.02 | Rio collapsed ordering to a single elected producer. Verify current relay reality before building |
| Avalanche C-Chain | **No** builder market | ~$0.02–0.05 | Closest thing to a naive priority-gas-auction chain left. Also the thinnest flow |
| BNB Smart Chain | **Yes** — BEP-322 builders (48 Club, BlockRazor, TxBoost, …) | ~$0.10–0.20 | Genuinely open bundle market, but sandwiching is filtered by policy. Backruns and liquidations only |
| Solana | **Yes** — Jito bundles (≤5 tx, all-or-nothing) | ~$0.001 + priority fee + **tip** | Permissionless entry, but the tip auction converts your edge into someone else's revenue |

### Per-chain notes worth knowing

**Base and OP Mainnet (OP Stack).** There is no public mempool at all — the sequencer accepts transactions over RPC and you learn about other people's transactions when a Flashblock is published. The surprise is what Flashblocks do to gas bidding: Base runs ten 200ms priority-fee auctions per 2-second block, and **once a Flashblock is sealed its ordering is frozen** ([Base docs](https://docs.base.org/base-chain/network-information/block-building), [deep dive](https://blog.base.dev/flashblocks-deep-dive)). A later transaction paying 100x the priority fee lands *behind* an already-committed cheap one. In practice, by Flashblock index 4–8, priority fee barely moves your position; only arrival time does. OP Mainnet runs the identical design at 250ms ([Optimism](https://www.optimism.io/blog/flashblocks-deep-dive-250ms-preconfirmations-on-op-mainnet)).

**Arbitrum One.** ~250ms blocks, and the ordering rule is genuinely **first-come-first-served by sequencer arrival timestamp — gas price does not affect ordering at all** ([Arbitrum docs](https://docs.arbitrum.io/build-decentralized-apps/arbitrum-vs-ethereum/block-numbers-and-time)). Since 17 Apr 2025, Timeboost overlays a **sealed-bid second-price auction each 60-second round**; the winner's transactions skip a 200ms artificial delay applied to everyone else ([gentle intro](https://docs.arbitrum.io/how-arbitrum-works/timeboost/gentle-introduction)). Empirical work found the express lane concentrated among few controllers and increased spam ([arXiv 2509.22143](https://arxiv.org/html/2509.22143v1)). So: latency race, then an auction on top of the latency race.

**Polygon PoS.** The Rio upgrade (mainnet 8 Oct 2025) replaced multi-producer block production with **VEBloP — validators elect a single block producer per span**, with standby backups, eliminating reorgs ([Polygon](https://polygon.technology/blog/polygon-launches-major-payments-upgrade-with-rio-faster-lighter-and-easier-to-build), [The Block](https://www.theblock.co/post/373811/polygon-activates-rio-upgrade-to-revamp-block-production-speed-up-network)). There is no builder market to bid into; there is one entity per span whose relationships determine inclusion. [FastLane/Atlas](https://github.com/FastLane-Labs/atlas) backrun auctions still exist, but confirm they are wired to the current producer set before assuming a bundle path.

**BNB Smart Chain.** Fastest major EVM chain: **0.45s blocks** since the Fermi hard fork in Jan 2026 ([BNB Chain](https://www.bnbchain.org/en/blog/fermi-hard-fork-accelerates-bsc-to-0-45-second-block-times)). PBS is standardized in [BEP-322](https://github.com/bnb-chain/BEPs/pull/322), so anyone can submit bundles to builders. The structural catch: the **BNB Good Will Alliance** requires participating builders to *filter sandwich bundles*, with validators prioritizing alliance builders — sandwiches fell ~95% ([GWA](https://www.bnbchain.org/en/blog/bnb-good-will-alliance), [results](https://www.bnbchain.org/en/blog/how-the-goodwill-alliance-slashed-sandwich-attacks-by-95)). Detection uses the open-source `bscexorcist` cross-block detector from [48 Club](https://github.com/48Club/). Whole strategy classes are policy-banned here, not merely competed away.

**Solana.** 400ms slots, no protocol mempool. The [Jito Block Engine](https://chainstack.com/jito-explained-bundles-tips-mev-solana/) is the de facto ordering venue: submit an atomic bundle of up to 5 transactions plus a tip, it simulates and forwards the highest-value combination to the leader. Entry is permissionless — no whitelist, no relationship needed — which is exactly why the tip auction competes margins to near zero. BAM (TEE-based block assembly, mainnet Sept 2025) and the upcoming **Alpenglow** sub-150ms finality upgrade both *tighten* the latency window rather than loosen it.

**Ethereum mainnet.** 12s slots, MEV-Boost sealed-bid builder auction. Concentration is extreme: as of Jan 2026 roughly **five builders produce ~97% of MEV blocks**, with BuilderNet around a quarter ([arXiv 2605.04471](https://arxiv.org/html/2605.04471v1)). Private order flow is ~12% of transactions but over half of block value ([arXiv 2410.12352](https://arxiv.org/html/2410.12352v3)). **Glamsterdam** (targeted Q3 2026) enshrines PBS via EIP-7732 and adds block-level access lists; note that **EIP-7782's 6-second slots was dropped** from the fork ([Conduit summary](https://www.conduit.xyz/blog/ethereum-glamsterdam-eips/)) — do not build assuming 6s slots.

**Avalanche C-Chain.** Granite (Nov 2025) shipped [ACP-226 dynamic minimum block times](https://build.avax.network/docs/acps/226-dynamic-minimum-block-times), enabling sub-second blocks tuned to a stake-weighted validator median, on top of [ACP-176](https://build.avax.network/docs/acps/176-dynamic-evm-gas-limit-and-price-discovery-updates) dynamic gas. There is a real public mempool and no PBS layer — the most "2021-shaped" venue in this table. That is also why the flow is thin.

### Decision rules

- **If the chain has no public mempool** (Base, OP, Arbitrum, Solana), you cannot react to pending user transactions. Any strategy premised on seeing a swap before it lands is dead on arrival. Design for reacting to *state after inclusion*, or don't build it.
- **If ordering is FCFS** (Arbitrum), your competitive axis is network latency, i.e. colocation spend. Skip unless you're prepared to buy that.
- **If ordering is a sealed-bid auction** (Ethereum, BSC, Solana-via-Jito, Arbitrum Timeboost), you can enter permissionlessly but your profit is bounded by the auction. Assume you bid away 80–95% of any edge you find.
- **If there's a single privileged producer** (Polygon post-Rio, all OP Stack sequencers), the surplus accrues to whoever holds that seat or has a relationship with it. There is no code path to that seat.
- **Cheap gas is not an edge.** Sub-cent fees mean everyone else can also spam attempts. Low gas correlates with *more* competitors per opportunity, not fewer.

### What this means for a solo operator

Read Tables 1 and 2 as a single filter: on every chain here, the surplus lands on **exclusive order flow, the sequencing seat, or a colocation budget**. None of those is code. Base and OP hand ordering to one operator; Arbitrum sells latency and then auctions it again; Polygon collapsed to one elected producer per span; Ethereum's builder market is a five-firm oligopoly gated by private flow; Solana's entry is open precisely because the tip auction extracts the edge; BSC's open bundle market has the profitable class filtered out by policy. The one venue with a naive public mempool and no PBS — Avalanche C-Chain — is thin enough that the opportunity set doesn't repay the infrastructure.

If a plan's competitive advantage is "better code," none of these eight rows will reward it. The honest use of this table is as a **disqualification checklist**: identify which of the three moats a strategy needs, confirm you hold none of them, and stop before the compute bill starts.

**Changing fast:** Flashblock intervals and OP Stack sequencing (Flashbots + OP Labs are shipping configurable/verifiable sequencing across the Superchain), Ethereum's Glamsterdam ePBS in Q3 2026, and Solana's Alpenglow. Re-check each before relying on this page past late 2026.


---

## DEX architectures — what you must know before integrating one

From an integrator's seat, a DEX is exactly three things: **a state-read shape** (how you price it off-chain), **a calldata shape** (how you encode the call), and **a settlement pattern** (who pulls the tokens, and when). Everything else is marketing. Each new family costs you a new encoder, a new quoter, and a new indexer — and none of that work is where the money is. Hold that thought; it returns at the end.

All selectors below were computed from the canonical signature, not copied from a blog post.

### Router addresses are not factory addresses

This is the single most expensive one-line bug in this reference, and it is drawn from a real config, not a hypothetical. In `project-chimera`'s `config/routing.yaml`, **every one of five configured venues had the factory address in the `router_address` field**. Result: 100% of collateral→debt swaps reverted. A factory has no `swapExactTokensForTokens`; the call hits the fallback and reverts with no reason string, which reads exactly like a slippage failure and sent the debugging in the wrong direction for days.

| Venue | Wrong (factory, was configured) | Right (router) |
|---|---|---|
| SushiSwap V2, Base | `0x71524B4f93c58fcbF659783284E38825f0622859` | `0x6BDED42c6DA8FBf0d2bA55B2fa120C5e0c8D7891` |
| Uniswap V3, Base | `0x33128a8fC17869897dcE68Ed026d694621f6FDfD` | `0x2626664c2603336E57B271c5C0b26F421741e481` (SwapRouter02) |
| Aerodrome, Base | `0x420DD381b31aEf6683db6B902084cB0FFECe40Da` (PoolFactory) | `0xcF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43` |
| Camelot, Arbitrum | `0x6EcCab422D763aC031210895C81787E87B43A652` | `0xc873fEcbd354f5A56E00E710B90EF4201db2448d` |

**Decision rule — startup assertion, not a code review.** For every configured router, at boot: (1) `eth_getCode` is non-empty; (2) an `eth_call` to a cheap view function on the expected ABI succeeds — `getAmountsOut(1e18, [tokenA, tokenB])` (`0xd06ca61f`) for V2, `poolFor` / `getAmountOut` for Solidly. A factory fails (2) instantly. Also assert the *shape*: a config field named `router_compatibility: "v2"` must be checked before the V2 encoder runs, or a Solidly router silently accepts a V2 config entry and reverts at broadcast time.

### Constant product — Uniswap V2 and its clones

`x * y = k`. Read state with `getReserves()` (`0x0902f1ac`) on the **pair**, which returns `(uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)`. Reserves are ordered by `token0 < token1` as raw address integers — not by your call order. Getting this backwards inverts every price you compute.

Output, with the 30 bps fee taken from the *input*:
`amountOut = (amountIn * 997 * reserveOut) / (reserveIn * 1000 + amountIn * 997)`

**Do not hardcode 997.** PancakeSwap V2 uses 9975/10000 (25 bps), Camelot uses per-pair variable fees, and several forks let the factory change it. Read the fee or you will systematically misprice.

Swap entry: `swapExactTokensForTokens(uint256,uint256,address[],address,uint256)` = **`0x38ed1739`**. Path is a plain `address[]`; a single hop is 2 addresses. The router does `transferFrom` on you, so approve the router (not the pair). Tokens with transfer fees require `...SupportingFeeOnTransferTokens`, which returns nothing.

### Uniswap V3 — concentrated liquidity

There is no single reserve pair. Liquidity is placed in **tick ranges**, and price moves through a *ladder*: as the swap crosses each initialized tick, active liquidity changes by that tick's `liquidityNet`. Pricing a large swap from `slot0` alone is wrong beyond the current tick range.

- `slot0()` (`0x3850c7bd`) → `(uint160 sqrtPriceX96, int24 tick, uint16 observationIndex, uint16 observationCardinality, uint16 observationCardinalityNext, uint8 feeProtocol, bool unlocked)`.
- Spot price of token1 per token0 = `(sqrtPriceX96 / 2**96) ** 2`, then adjust by `10**(decimals0 - decimals1)`.
- A correct quoter needs `tickBitmap` + `ticks(i)` walks, or an on-chain Quoter call. Most integrators use the Quoter (`quoteExactInputSingle`, `0xc6a5026a`) — it reverts and returns data, so it is `eth_call`-only, never gas-estimable.

**Selector trap.** `SwapRouter` (v1) `exactInputSingle` takes a `deadline` → `0x414bf389`. `SwapRouter02` dropped `deadline` → **`0x04e45aaf`**. Same name, different struct, different selector. `exactInput` on SwapRouter02 is `0xb858183f`.

Packed path for multi-hop: `abi.encodePacked(tokenIn, uint24 fee, tokenOut)` = 43 bytes for one hop, 66 for two. Fee tiers: 100 / 500 / 3000 / 10000.

If you call the **pool** directly (cheaper, no router) via `swap(address,bool,int256,uint160,bytes)` (`0x128acb08`), you must implement `uniswapV3SwapCallback(int256,int256,bytes)` (`0xfa461e33`) and pay inside it. The pool pushes output first and trusts your callback — verify `msg.sender` is the real pool (recompute the CREATE2 address) or anyone can drain your contract by calling the callback directly.

### Uniswap V4 — and why it breaks naive integrations

Four structural changes, each of which invalidates a V2/V3-shaped integration:

1. **Singleton `PoolManager`.** All pools live in one contract — `0x000000000004444c5dc75cB358380D2e3dE08A90` on Ethereum, `0x498581fF718922c3f8e6A244956aF099B2652b2b` on Base. There is **no per-pool address to point a router at**. A pool is a `PoolKey{currency0, currency1, fee, tickSpacing, hooks}` hashed into a `bytes32` id. Your pool index cannot key on addresses anymore.
2. **Flash accounting.** You call `unlock(bytes)` (`0x48c89491`); the manager calls you back; you run any number of actions; net deltas must reach **zero** by the end or the whole thing reverts. Balances move via `settle`/`take`, using EIP-1153 transient storage. There is no "approve and call" path.
3. **Hooks.** One hook contract per pool, chosen permanently at initialization, with permissions encoded in the hook's *address bits*. `beforeSwap` can override the fee (only if the pool was created with `DYNAMIC_FEE_FLAG`) and can return a `BeforeSwapDelta` that **replaces the curve entirely**.
4. **Consequence:** your off-chain quote can be wrong for reasons not visible in pool state. A hook may charge you, reorder you, or refuse you. Quote V4 by simulation against the actual hook, or don't quote it.

### Solidly forks — Aerodrome (Base), Velodrome (Optimism)

The single most common integration failure after the factory/router mixup. Solidly routers do **not** take `address[]`. They take an array of structs:

```solidity
struct Route { address from; address to; bool stable; address factory; }
```

- Aerodrome/Velodrome V2 (4 fields): `swapExactTokensForTokens(uint256,uint256,(address,address,bool,address)[],address,uint256)` = **`0xcac88ea9`**
- Original Solidly (3 fields, no factory): = **`0xf41766d8`**
- Uniswap V2: `0x38ed1739`

Three different selectors. **Your V2 `path[]` encoder does not "mostly work" against Aerodrome — it reverts on the first byte**, because `0x38ed1739` does not exist on that contract. Chimera's config correctly demotes `aerodrome-base` to `router_compatibility: "custom"` and the resolver skips it, which is the right failure mode: excluded, not silently broken.

Two curves per venue: **volatile** (`x*y=k`) and **stable** (`x³y + y³x = k`, near-flat around parity). The `bool stable` flag selects which pool — pick wrong and you get a nonexistent-pool revert or a badly mispriced route. Fees are factory-set per pool, not a constant.

**ve(3,3) emissions gotcha:** liquidity follows weekly gauge votes and bribes. A pool that had $2M of depth last Wednesday can have $200k this Wednesday. Any cached Aerodrome/Velodrome pool index needs a **weekly** refresh at minimum, tied to the epoch flip.

### Curve StableSwap

Not constant product. The invariant blends constant-sum and constant-product via an amplification coefficient `A`:
`A·n^n·Σxᵢ + D = A·D·n^n + D^(n+1) / (n^n · Πxᵢ)`

Practically: near-zero slippage while the pool is balanced, then a sharp cliff. There is no path array — you pass **coin indices**. And the index type differs by pool generation:

- StableSwap (stable pairs): `exchange(int128,int128,uint256,uint256)` = `0x3df02124`; quote with `get_dy(int128,int128,uint256)` = `0x5e0d443f`
- CryptoSwap (volatile pairs): `exchange(uint256,uint256,uint256,uint256)` = `0x5b41b908`

Passing `uint256` indices to an `int128` pool is a revert, not a mispricing. **When it matters:** any stable↔stable leg (USDC/USDT/DAI, wstETH/ETH, LST pairs). Routing those through a constant-product pool costs you 10–50 bps for nothing. During a depeg, Curve is where the real depth is and your V2-only router is blind to it.

### Balancer, and 0% flash loans

Balancer V2 is also a singleton **Vault** holding all pools; a pool is a `bytes32 poolId`. Weighted pools generalize x*y=k to arbitrary weights (`Π (Bᵢ)^wᵢ = k`), so an 80/20 pool prices very differently from 50/50.

- `swap((bytes32,uint8,address,address,uint256,bytes),(address,bool,address,bool),uint256,uint256)` = `0x52bbbe29`
- `getPoolTokens(bytes32)` = `0xf94d4668`

**Flash loan economics** — this is the part worth memorizing:

| Source | Entry point | Selector | Callback | Fee (mid-2026) |
|---|---|---|---|---|
| Aave V3 | `flashLoanSimple(address,address,uint256,bytes,uint16)` | `0x42b0b77c` | `executeOperation` | **5 bps (0.05%)** |
| Balancer V2 Vault | `flashLoan(address,address[],uint256[],bytes)` | `0x5c38449e` | `receiveFlashLoan` (`0xf04f2707`) | **0%** (governance-settable) |
| Morpho Blue | `flashLoan(address,uint256,bytes)` | `0xe0232b42` | `onMorphoFlashLoan` (`0x31f57072`) | **0%** |

**Decision rule:** if your strategy borrows and repays in the same transaction and the asset is available on Balancer or Morpho, using Aave burns 5 bps of gross margin for nothing. On a $50k flash loan that is $25 per attempt — often larger than the entire expected edge. Chimera's `compute_amount_out_min` bakes `FLASH_LOAN_PREMIUM_BPS = 5` into its slippage math; that constant is a *choice*, and it is the wrong one whenever a 0% venue holds the asset.

### Solana, briefly — a different universe

There are no selectors and no `msg.sender`-style calldata. You send an **instruction** (8-byte Anchor discriminator + Borsh args) plus an explicit **list of every account** the program will touch. Getting the account list wrong is the dominant failure mode, and it changes per pool.

| Program | Program ID | Model |
|---|---|---|
| Raydium AMM v4 | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` | Constant product (legacy, OpenBook market accounts) |
| Raydium CLMM | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK` | Concentrated, tick arrays |
| Raydium CPMM | `CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1` | Constant product, no OpenBook dependency |
| Orca Whirlpools | `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc` | Concentrated (tick arrays of 88 ticks) |
| Meteora DLMM | `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo` | Discrete bins, not a continuous curve |
| Jupiter (aggregator) | `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4` | Routes across all of the above |

**Decision rule for Solana:** unless you specifically need to beat Jupiter's router, don't write six program integrations. Call Jupiter's quote/swap API, get back a serialized transaction, sign it. Jupiter routes the large majority of Solana swap volume as of mid-2026; direct integration only pays if you have a reason Jupiter's path is structurally worse for you.

### Master reference table

| DEX | Chain(s) | Entry point / selector | Calldata shape | Gotcha |
|---|---|---|---|---|
| Uniswap V2 / Sushi | EVM, all | Router `0x38ed1739` | `address[]` path | Fee constant varies by fork; reserves ordered by `token0 < token1` |
| Uniswap V3 | EVM, all | SwapRouter02 `0x04e45aaf` / `0xb858183f` | Packed path `token+uint24+token` | v1 router uses `0x414bf389` (has `deadline`) — different selector |
| Uniswap V3 pool (direct) | EVM | `swap` `0x128acb08` | callback `0xfa461e33` | Must verify callback caller is a real pool |
| Uniswap V4 | ETH, Base, others | `PoolManager.unlock` `0x48c89491` | `PoolKey` + delta settlement | No pool address; hooks can override fee and curve |
| Aerodrome / Velodrome | Base / OP | Router `0xcac88ea9` | `Route[]{from,to,stable,factory}` | V2 `path[]` encoder **reverts**; weekly gauge-driven liquidity churn |
| Solidly (original forks) | various | `0xf41766d8` | `Route[]{from,to,stable}` | 3-field struct — *different selector again* |
| Curve StableSwap | EVM | `exchange` `0x3df02124` | `int128` coin indices | CryptoSwap pools want `uint256` (`0x5b41b908`) |
| Balancer V2 | EVM | Vault `swap` `0x52bbbe29` | `bytes32 poolId` | Singleton vault; weights ≠ 50/50 |
| Raydium / Orca / Meteora | Solana | Anchor discriminator | Explicit account list | No calldata ABI; account list is the integration |
| Jupiter | Solana | HTTP quote/swap API | Server-returned serialized tx | Not a contract call — a service dependency |

### Reading pool state without dying of latency

Three primitives cover 90% of it:

- **`getReserves()`** (`0x0902f1ac`) for V2-family, **`slot0()`** (`0x3850c7bd`) for V3-family, and program-account reads on Solana.
- **`Multicall3.aggregate3((address,bool,bytes)[])`** (`0x82ad56cb`) at `0xcA11bde05977b3631167028862bE2a173976CA11` — the same address on 100+ chains. Batch 200–500 calls per request; beyond that you hit provider gas caps and get an opaque failure.
- **Pin the block.** Pass an explicit `blockNumber` to every batch so all reads are one consistent snapshot. Reads at `latest` across several requests will straddle a block boundary and hand you an arbitrage that does not exist. This is the #1 source of phantom opportunities in backtests.

### The hidden 6–10 week cost: maintaining a pool index

Nobody budgets for this, and it is the real schedule risk on any arbitrage project. What it actually involves:

1. **Enumeration** — replay `PairCreated` / `PoolCreated` logs from each factory's deploy block. On Base that's tens of thousands of pools across a dozen factories, and public RPCs will rate-limit you into a multi-day backfill. Budget 1–2 weeks.
2. **Liquidity filtering and token metadata** — decimals, fee-on-transfer, rebasing, blacklists, honeypots. Every one of these silently breaks quoting. 1–2 weeks.
3. **Per-family adapters** — a V3 tick walker, a Solidly stable-curve quoter, a Curve `get_dy` caller. Each is a week, and each needs its own fork-test fixtures.
4. **V4 reindexing** — no pool addresses; you must index `Initialize` events on the singleton and reconstruct `PoolKey`s, then classify hooks by their permission bits. 1–2 weeks on its own.
5. **Refresh cadence** — new pools daily, Solidly liquidity migrating weekly, V3 ranges shifting every block. This is not a build; it is an ongoing operational load.

**Red flags — stop and reprice the project if you see these**
- You are on encoder #3 and have not yet executed a single profitable transaction. The encoders are not the constraint.
- Your quoter and the chain disagree by more than a few bps. You have a decimals bug, a stale block, or an unmodeled hook — not an edge.
- Your index refresh is manual. It will go stale during the exact volatility window you built it for.
- You are pricing a route your contract cannot encode. Chimera's resolver skips non-`"v2"` venues by design; a system that *quotes* Aerodrome but cannot *call* it produces confident, unexecutable opportunities.

### What this means for a solo operator

Every item above is tractable engineering. That is precisely the problem. DEX integration breadth is unbounded work with a bounded payoff, because the surplus in on-chain arbitrage accrues to whoever holds exclusive order flow, the block-building/sequencing seat, or a colocation budget — none of which is an encoder. Chimera's config had all five routers wrong, fixed them, and then ran six days live with zero liquidations. The routers were never the binding constraint; flow was.

**The decision rule that follows:** before you write venue integration N+1, price the aggregator alternative (1inch / 0x / Odos on EVM, Jupiter on Solana — an API call, an afternoon, ~5–15 bps of routing spread). Then ask what specifically is blocked by *not* having direct access to venue N+1. If the answer is "nothing measured," you are buying a capability you have no demand for. Build the direct integration only where you have already proven, with real fills, that the aggregator's spread is what's eating the trade.

Sources: [Aerodrome Router (BaseScan)](https://basescan.org/address/0xcf77a3ba9a5ca399b7c97c74d54e5b1beb874e43), [Aerodrome Router.sol](https://github.com/aerodrome-finance/contracts/blob/main/contracts/Router.sol), [Uniswap V4 PoolManager, Base](https://basescan.org/address/0x498581ff718922c3f8e6a244956af099b2652b2b), [Uniswap V4 PoolManager, Ethereum](https://etherscan.io/address/0x000000000004444c5dc75cb358380d2e3de08a90), [Uniswap V4 flash accounting](https://docs.uniswap.org/contracts/v4/guides/flash-accounting), [Uniswap V4 dynamic fees](https://docs.uniswap.org/contracts/v4/concepts/dynamic-fees), [Balancer V2 FlashLoans.sol](https://github.com/balancer/balancer-v2-monorepo/blob/master/pkg/vault/contracts/FlashLoans.sol), [Morpho fees](https://docs.morpho.org/build/earn/concepts/yield-fees/), [Meteora DLMM program ID](https://docs.bitquery.io/docs/blockchain/Solana/Meteora-DLMM-API/).


---

## Smart contracts — patterns, hazards, and when to write your own

You are a strong engineer, not a Solidity specialist. That is a good starting position, because most on-chain code failures for a solo operator are not exotic cryptography — they are ordinary engineering mistakes (frozen interfaces, untested paths, unverified callers) that happen to be irreversible and cost money. This section is decision rules, not a Solidity tutorial.

### Solidity vs Yul vs Huff: the extensibility trade

| | Write it in | Justified when | Real cost |
|---|---|---|---|
| **Solidity** | Default. Always start here. | Everything, until proven otherwise | ~5-15% more gas than hand-tuned Yul |
| **Yul** (inline or standalone `.yul` object) | Narrow hot paths inside a Solidity contract | You have measured gas as the binding constraint on a live, revenue-positive strategy | Static analysers stop working; every ABI change is a rewrite |
| **Huff** | Essentially never for a solo operator | Contest/CTF, or a single-purpose contract deployed thousands of times a day | No type system, no tooling, no auditor pool |

The [project-chimera](https://github.com/) executor was written as a standalone 387-line `Executor.yul` object with a hand-rolled `switch` dispatch table. On Base, where a liquidation transaction costs a fraction of a cent, the gas saved was economically zero. What it cost was concrete and compounding:

- The DEX call is `callSwapExactTokens`, which hardcodes selector `0x38ed1739` (`swapExactTokensForTokens`) and a fixed two-element path. Adding a Uniswap V3 router, a three-hop path, or an Arbitrum router meant rewriting the memory layout by hand — not adding a parameter. A later Arbitrum port stalled on exactly this.
- The dispatch is a fixed `switch` with `default { revert }`. Safe (no fallback, no delegatecall, no proxy) but unextendable — no eleventh function without redeploying and re-pointing every off-chain caller.
- Both analysers were configured to *skip it*: `foundry.toml` has `[lint] ignore = ["src/Executor.yul"]` and `slither.config.json` has `filter_paths: "lib|node_modules|out|cache|Executor.yul"`. CI runs `slither contracts --fail-high` and goes green — on the 87-line `FundDistributor.sol`. The 387 lines holding all the money logic are the only lines no tool ever checks.

**Decision rule:** optimize for extensibility until you have proven revenue. Gas is a rounding error on an L2; the ability to add a second venue next week is not. If you cannot state the dollar value of the gas you are saving, write Solidity.

### Flash loans: cheap capital, not free edge

A flash loan removes the *capital* constraint. It does not create edge. Fee as of mid-2026 (verify on-chain before trusting any of these — Aave's is a governance parameter):

| Venue | Fee | Entry point | Notes |
|---|---|---|---|
| Aave V3 | 5 bps (0.05%), read `FLASHLOAN_PREMIUM_TOTAL()` | `flashLoanSimple` → `executeOperation` | [Governance-adjustable](https://aave.com/docs/aave-v3/guides/flash-loans) |
| Balancer V2/V3 | 0% | `flashLoan` / vault `unlock` | [Zero by governance, not by design](https://docs.balancer.fi/concepts/vault/flash-loans.html) |
| Morpho | 0% | `flashLoan` (not ERC-3156) | [`flashFee` is zero](https://docs.morpho.org/learn/concepts/flashloans/) |
| Uniswap V3 | Pool fee tier (5/30/100 bps) | `flash` / `uniswapV3SwapCallback` | Fee is the pool's, not a loan premium |
| Uniswap V4 | No separate flash fee | [`unlock` flash accounting](https://docs.uniswap.org/contracts/v4/guides/flash-accounting) | Net settlement at transaction end |

**The trap.** Express the fee in bps of notional and compare it to your gross edge *in the same units*, before you write any code. A 5 bps premium on a $20,000 loan is $10. If the arb is a two-leg round trip through 5 bps Uniswap pools, you have already spent 10 bps in swap fees; add Aave's 5 and you need >15 bps of dislocation before gas to break even. On a sub-dollar arbitrage the premium alone can exceed the entire margin.

Where 5 bps is *irrelevant*: Aave liquidations, where the liquidation bonus is typically 500-1000 bps. Paying 5 bps out of a 750 bps bonus is noise. **Decision rule:** if your edge is measured in whole percent, use whatever venue has the deepest liquidity in your asset. If your edge is measured in single-digit bps, use a 0% venue or do not do the trade.

### Callback security: any address can call your callback

The moment your contract implements `executeOperation`, `uniswapV3SwapCallback`, `uniswapV2Call`, or `onMorphoFlashLoan`, it has a public function that an attacker can call directly with fabricated arguments. Two checks, both mandatory:

1. **`msg.sender` is the real pool/vault.** For Aave and Balancer this is a single stored address — compare against it. For Uniswap V3 there are thousands of pools, so you *cannot* store one: you must recompute the pool address via CREATE2 from `(factory, token0, token1, fee)` and compare. Storing "the router" and checking that instead is the classic bug.
2. **`initiator == address(this)`.** Otherwise someone else can start a flash loan naming your contract as receiver and make your callback body run on their terms.

The chimera executor does both (`if iszero(eq(caller(), sload(1)))` → `InvalidPool()`, `if iszero(eq(initiator, address()))` → `Unauthorized()`) plus strict `calldatasize()` equality checks. That is the right shape. The general lesson: **your attack surface grows discontinuously the first time you add a DEX callback**, because you have converted an internal step into a public entry point.

### Access control and reentrancy

Use `owner` for configuration and a separate `mapping(address => bool) workers` for hot-path execution, so a compromised bot key cannot re-point the pool address or drain the contract. Keep them in different storage slots and gate them separately.

**Two-step ownership transfer is not optional.** The same repo contains both patterns: `FundDistributor.sol` has `transferOwnership` → `acceptOwnership` with a `pendingOwner` slot; `Executor.yul`'s `transferOwnership` does a bare `sstore(0, newOwner)` — one typo'd address and the contract is permanently unowned, with any residual token balance stranded. Two-step everywhere, no exceptions.

Reentrancy: the classic hazard is state written after an external call. A fixed dispatch table with no delegatecall and no arbitrary `call(data)` has a small surface. But note that chimera's `execute` takes a **caller-supplied `dexRouter` address and calls it** — an arbitrary external call. The only thing preventing an attacker-controlled router from re-entering is that `execute` is owner/worker-gated, and there is no lock in the contract. That is acceptable *only* while the gate holds. **Decision rule: arbitrary-target call + access control = tolerable; arbitrary-target call + public entry = add a lock.**

Use an [EIP-1153](https://eips.ethereum.org/EIPS/eip-1153) transient-storage lock rather than a storage bool: `tload`/`tstore` cost 100 gas versus ~7,100 for the cold-`sstore` guard, and the slot self-clears at end of transaction. Available in Solidity via inline assembly since 0.8.24, and as a first-class [`transient` data location since 0.8.28](https://www.soliditylang.org/blog/2024/01/26/transient-storage/). Requires `evm_version = "cancun"` (chimera's `foundry.toml` already sets this).

### Approvals and non-standard tokens

- `approve` then `transferFrom` is a two-transaction dance with a well-known race: changing a non-zero allowance to another non-zero value lets the spender front-run and spend both. Set to zero first, or use `increaseAllowance`-style semantics.
- **Infinite approvals** (`type(uint256).max`) save gas per trade and are fine to a canonical, immutable pool. They are not fine to a router address you can reconfigure — a compromised owner key then drains every approved token. Approve exact amounts to anything mutable.
- **USDT and friends return nothing** from `approve`/`transfer` instead of a bool. A naive `bool ok = IERC20(t).approve(...)` reverts on decode. Use OpenZeppelin `SafeERC20` in Solidity. Chimera hand-rolls the equivalent in Yul (`if returnSize { ... }` — treats empty returndata as success, non-empty as requiring a truthy word), correctly, twice, duplicated. That duplication is the Yul tax.

### Profit gates are a measurement, not a defence

Chimera's gate requires `balanceAfter > balanceBefore + premium + minProfit + tip` on the debt asset. That is a good sanity check with three specific blind spots worth naming:

1. **Anyone can inflate the measured balance** by plainly `transfer`-ing tokens to your contract. Any structure that can donate into the balance mid-transaction — including one you build yourself — passes the gate without generating profit.
2. **It measures one asset.** Gas paid in ETH, and any collateral dust left behind, are outside the gate. Balance delta ≠ economic P&L.
3. **It cannot see why the balance moved.** A manipulated pool that hands you the asset while you lose more elsewhere still clears the check.

Treat a profit gate as a circuit breaker against bad *execution*, never as proof a *strategy* is profitable. The strategy question is answered by measuring realised flow at the venue, before you deploy anything.

### Testing: make a skipped test visibly skipped

Foundry (`forge test`, `forge fuzz`, `forge invariant`) plus fork testing is genuinely the best free tooling in the industry. It is also easy to make it lie to you.

Real failure mode from this repo, `contracts/test/ExecutorBaseFork.t.sol:24`:

```solidity
string memory forkUrl = vm.envOr("BASE_FORK_URL", string(""));
if (bytes(forkUrl).length == 0) return;
```

With `BASE_FORK_URL` unset — which is the default in CI and on any fresh clone — the function returns immediately and Foundry reports it as **PASS**. The second test in the file compounds it: six env vars, and missing *any one* silently returns. The suite is green and has never touched Base mainnet. A green run proved nothing about the only tests that exercise real protocol state.

**Fix:** use `vm.skip(true)` so the runner prints `[SKIP]`, and make CI fail if the count of skipped tests exceeds a known number. The general rule: **an untaken code path in a test must be visible in the output.** Any `if (config missing) return;` in a test is a silent liar.

### Audit reality for a solo operator

| Can self-serve (free, do all of it) | Cannot self-serve |
|---|---|
| `slither` — but check `filter_paths` actually covers your money code | Economic/game-theoretic review of the strategy |
| `forge test --fuzz-runs 10000`, invariant tests | Oracle-manipulation and cross-protocol composition review |
| Fork tests against pinned mainnet blocks | Anything requiring a second pair of expert eyes |
| `forge coverage`, `cast storage` layout diffs | Formal verification (Certora et al. — enterprise pricing) |
| Re-deriving every selector by hand (`cast sig`) | A firm's name on a report, if anyone else's money is involved |
| Reading the audits of protocols you integrate | |

A real audit for a contract this size runs five figures. If the strategy has not produced revenue, that spend is unjustifiable — which means **your risk budget is capped at what you are willing to lose**, and the contract should hold only working capital, never a treasury.

### Red flags

- Any analyser exclusion (`filter_paths`, `[lint] ignore`, `// slither-disable`) that covers the file where the money moves.
- A test that returns early on missing configuration and reports PASS.
- One-step `transferOwnership`.
- A callback that checks `msg.sender` against a stored *router* rather than the actual pool.
- Reaching for Yul or Huff before the strategy has earned a dollar.
- Infinite approval to any address the owner can change.

### Before you deploy

1. Every callback verifies both `msg.sender` (pool identity, CREATE2-derived where there are many pools) and `initiator == address(this)`.
2. Ownership transfer is two-step; owner and worker roles are separate.
3. There is a withdraw/sweep path for every token the contract can ever hold, including one it receives unexpectedly.
4. `slither` runs over the actual contract source, not a filtered subset. Read the config, don't trust the exit code.
5. Fork tests run against a **pinned block** and are visibly skipped (`vm.skip`) rather than vacuously passed when unconfigured.
6. Fee stack is written down in bps and compared to measured edge — flash premium, swap fees, gas, tip.
7. Approvals are exact, or infinite only to immutable addresses.
8. Deploy to testnet, then to mainnet with the smallest capital that can execute one real transaction. Verify the source on the block explorer.
9. Kill switch exists and is tested: `setPool(0)` or an equivalent that halts execution without needing a redeploy.
10. You know the private key's blast radius. Assume it leaks.

### What this means for a solo operator

The uncomfortable summary from months of building this: the contract was the easy part, and it was never the constraint. `Executor.yul` is careful, correct code — validated calldata sizes, non-standard-token handling, both callback checks, a profit gate — and it produced zero revenue over six days live on Base mainnet, because in market-microstructure niches the surplus goes to whoever holds exclusive order flow, the block-building seat, or a colocation budget. None of those is code.

So: write the contract simply, in Solidity, keep it extensible, and spend the saved effort on *measuring the venue* first. If a real audit is not affordable, the strategy is not yet at a scale where the contract is the risk. The contract is only ever the last 10% of the problem, and optimising it is the most satisfying possible way to avoid the other 90%.


---

## Lending protocols and liquidations

Liquidation is the most legible business in DeFi: the rules are in public Solidity, the events are indexed, and the reward is a fixed percentage. That legibility is the problem. Everything a good engineer can do here, everyone else has already done. What follows is the mechanics plus the numbers that closed the door, measured on Base in July 2026.

### Aave V3: the mechanics that matter

A position is liquidatable when `healthFactor < 1e18`, where HF = Σ(collateral × liquidationThreshold) / Σ(debt), all legs priced by `AaveOracle` — Chainlink feeds, never a DEX. Debt legs use the variable borrow index, so HF drifts downward on its own even with a frozen oracle.

The entry point is one function:

```
liquidationCall(address collateralAsset, address debtAsset, address user,
                uint256 debtToCover, bool receiveAToken)   // selector 0x00a718a9
```

| Parameter | Decision it forces |
|---|---|
| `debtToCover` | In the **debt token's native units**, for **one reserve** — not the position's aggregate USD. Mixing those truncates to dust for any 18-decimal debt asset above ~$1. |
| `receiveAToken` | `false` gives underlying (needed if you flash-loan and must repay atomically). `true` gives the interest-bearing aToken — no withdraw step, but you now hold protocol exposure and must `withdraw()` later, and it can fail on a paused/illiquid reserve. |
| collateral choice | Pick by bonus × seizable value, not by size alone. |

**Close factor.** It is not a flat 50%. `CLOSE_FACTOR_HF_THRESHOLD` is 0.95: at HF ≤ 0.95 you may take `MAX_LIQUIDATION_CLOSE_FACTOR` = 100% of that reserve's debt; above it you get the default branch. In the 0.95–1.0 band the effective coverage varies rather than sitting at a constant half — project-chimera's detector (`core/src/detector/liquidation.rs:513`) models it as a linear ramp `closeFactor = 0.5 + 10 × (1 − HF)` in RAY, while Aave V3.3's `LiquidationLogic` layers on `MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD` and `MIN_LEFTOVER_BASE` dust rules that force a full liquidation when a partial one would strand a small remainder.

> **Red flag:** the exact form is version- and deployment-specific. Chimera's own compendium (`docs/research/aave-v3-liquidation-compendium.md`) lists "confirm default and max close factor" as an *unverified* checklist item, and it stayed unverified through six days of live-armed running. Decision rule: read the constants out of the deployed [aave-v3-origin](https://github.com/aave-dao/aave-v3-origin) commit for your chain, or simulate the real `liquidationCall` and read `actualDebtToLiquidate` from the receipt. Never hardcode 50%.

**Liquidation bonus** lives in bits 32–47 of the packed reserve config, typically 4.5–10% (Base Aave's clean events measured 4.2–8.4%). Aave then skims a `liquidationProtocolFee` off *your* bonus, not off the principal. Read both per reserve; do not assume a chain-wide constant.

### The key economic point: the bonus is oracle-priced

Seized collateral is computed as `debtToCover × (oraclePrice_debt / oraclePrice_collateral) × (1 + bonus)`. **Every price in that expression is the oracle's.** The bonus is therefore an additive constant handed to you at the moment of the call, completely independent of any AMM state.

Consequences, and they are the whole strategy:

- You **cannot inflate the bonus** by moving DEX prices. Sandwiching your own liquidation, JIT liquidity, cross-pool routing — none of it touches the numerator. There is no AMM game that makes a liquidation pay more.
- DEX mechanics only ever appear on the **exit**, and only subtractively: slippage, fees, and gas. On Base Morpho, a $50,501 gross bonus realized ~$32,400 after measured slippage — a 36% haircut.
- Therefore the total revenue pool of a venue is `Σ(repaid × bonusRate)` and is **fully computable from historical events before you write a line of code.** It is identical for every participant. Skill can only differentiate on the cost side, which is bounded below by zero.

### Morpho Blue: different math, usually deeper

Morpho Blue (`0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` — code on Ethereum and Base only; other chains run separate deployments) is a set of immutable isolated markets, each keyed by `(loanToken, collateralToken, oracle, irm, lltv)`.

| | Aave V3 | Morpho Blue |
|---|---|---|
| Scope | one shared pool, cross-collateral | isolated market per pair |
| Close factor | piecewise, ≤100% | **none** — repay up to the full position |
| Bonus | per-reserve config value | `LIF = min(1.15, 1 / (1 − 0.3 × (1 − LLTV)))` → ≤13.04% of seized |
| eMode / isolation | yes | n/a |
| Flash loans | 5 bps | **free** |
| Chainlink SVR coverage | yes (Aave, Compound, Venus) | **no** |

It runs deeper on the same chain because market creation is permissionless, LLTVs go higher, and curated vaults funnel supply into a few pairs. On Base over 30 days to 2026-07-25: **Morpho $50,501 gross bonus vs Aave V3 $4,404 — roughly 11×.** A detector is a rewrite, not a port: no close factor, no eMode, no isolation mode.

### Compound III (Comet) and Moonwell, briefly

Comet splits liquidation into two steps: `absorb()` moves the underwater position onto the protocol balance sheet, then anyone calls `buyCollateral()` to buy it at a discount from the protocol. That is **capital-gated, not latency-gated** — median 47 blocks between absorb and next buy, which is an eternity. Mechanically the best-shaped opportunity found anywhere. Measured flow: **$1.16 across two buys in 30 days**, zero absorbs on Ethereum. An earlier $39k/30d estimate was simply wrong. Moonwell (Compound V2 fork, Base) is honest and open at the oracle gate but tiny: 242 events, ~$4.6k/30d bonus — Base-Aave sized.

### Why this stopped being a latency race

[Chainlink SVR](https://docs.chain.link/data-feeds/svr-feeds) (Smart Value Recapture) bundles the oracle update with the right to act on it and auctions that bundle. On Base it settles through **FastLane Atlas v1.6.4** (`0x583dcFef…9b77`, `metacall` selector `0x4317ca01`, dApp control `ChainlinkOevDAppControl`). Chainlink acquired Atlas in January 2026; it now serves SVR exclusively.

Decoding the Base incumbent's winning transaction: log[0] is the SVR round update and log[14] is the liquidation, **same transaction**. Two solver bids (0.05837 vs 0.05477 ETH), first-price, FastLane's share zero — essentially the whole bid returns to the protocol. The winner's measured ETH residual on that transaction was **~$0.11**.

| Mitigation | Effect on a latency-optimized bot |
|---|---|
| SVR / OEV auction | Trigger price is unobservable on a public RPC — it arrives inside the winner's bundle or 30s later on fallback. Faster RPC, colo, mempool subscription: all worthless. |
| First-price sealed bid | Clearing price → ~100% of the bonus. Winning means bidding away your entire margin. |
| Reputation gate | Atlas takes 2 bids/tx, one slot reserved for high-reputation solvers. |
| Parallel BTC+ETH auctions | Bidding one wins ~50% even with the best bid; covering both needs multiple bonded EOAs. |

**99.5% of Base Aave liquidation value over 28 days went through Atlas. The non-Atlas residual was $17.92 of bonus.**

**Two-call openness gate, run before anything else:** call `getSourceOfAsset(address)` (`0x92bf2be0`) on the protocol's oracle, then `typeAndVersion()` (`0x181f5a77`) on the returned aggregator. If it says `DualAggregator`, the venue is auction-closed — walk away. Costs two `eth_call`s. As of mid-2026 SVR covers Aave, Compound and Venus but **not Morpho**; Aave governance has pending SVR expansions, so anything open today can close on a governance schedule.

### Size the opportunity before you build

The whole procedure is one day of stdlib Python (`scripts/measure_liquidation_flow.py`, `scripts/scan_liquidation_venues.py`):

1. `eth_getLogs` for `LiquidationCall` topic0 `0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286` (Morpho `Liquidate`: `0xa4946ede…67e41`) over 30 days, checkpointed.
2. Sum the bonus — but **re-derive it against the protocol's structural incentive factor, or read the oracle at each event's block.** Current-price valuation inflated Base Morpho's bonus by 63%.
3. Group by `msg.sender`; compute the top incumbent's share and the pool remaining below rank 3.
4. Divide the residual by 12 and subtract infrastructure.

**The real result — Base Aave V3, 30 days to 2026-07-25** (Pool `0xA238Dd80C259a72e81d7e4664a9801593F98d1c5`):

| Metric | Value |
|---|---|
| Events | 116 |
| Collateral seized | $79,773 |
| **Gross bonus, entire market** | **≈$4,450 / month** |
| Unique liquidators | 22 |
| Top incumbent (`0xd128…4b3f`) | **85.4%** |
| Residual for perfect execution | **≈$650 / month gross**, before gas and slippage |
| Pool below the top three | **$604 / 30d** (~$36/yr) |
| Distribution | 72 of 116 events seized **< $1**; p50 event $0.04 |

Standing liquidatable stock across **214,180 indexed Base borrowers was $9.15 of dust.** Coverage was never the constraint. Latency was assumed to be the constraint. **Market size was the constraint** — and one 30-day scan would have said so before the first commit.

### Checklist for evaluating any lending venue's liquidation flow

1. **Openness gate (2 calls).** `getSourceOfAsset` → `typeAndVersion`. `DualAggregator` = closed. Stop.
2. **Exit gate (2 calls).** Is there a same-chain DEX route for ≥3× your intended clip at <2% impact, and is `|oraclePrice / tradedPrice − 1| ≤ 1%`? A +4% oracle premium cancels a +4% bonus exactly. This check would have killed one dead-end market in 2 calls instead of ~1,400.
3. **Size it.** 30 days of events, bonus re-derived structurally. Below ~$100k/30d for the *whole market*, a solo entrant cannot clear infrastructure.
4. **Concentration.** Top-1 and top-3 share by `msg.sender` — and verify each is a real entity. Morpho Bundler3 (`0x6566…0245`) is a public router with 12+ callers, not a competitor; counting it as one inflated concentration by ~25 points.
5. **One-off check.** Strip single-market unwinds. 73.8% of Ethereum Morpho's 30-day value was one vault unwinding; 84% of Base Morpho was one cbBTC market.
6. **Trigger class.** Price-push flow is a race (98.8% of Base Morpho). Interest-accrual and rate-drift insolvencies emit no oracle update, spawn no auction, and are a *polling* problem — the only structurally open class found anywhere.
7. **Set the kill threshold in writing before you look at the number.** Chimera did this once, on the last survey, and it worked.

### What this means for a solo operator

The liquidation bonus is a public constant, the trigger is auctioned, and the residual accrues to whoever holds the auction seat, the inventory, or the reputation slot. None of those is code. The Base incumbent runs ~39 liquidations/month, was funded by a $65 retail Binance withdrawal, and pays a median priority tip of ~0.10 gwei — nobody is racing them, because there is nothing to win. Their edge is inventory in the three assets that actually liquidate (no swap engine needed), free Morpho flash loans, and being registered in the auction.

If you want to test this category, spend one day on the scan and zero on the engine. The scan is the only artifact from a months-long build that ever changed a decision.


---

## MEV — the honest map

Maximal Extractable Value is the profit available from *choosing what goes in a block and in what order*. That definition is the whole story: MEV is a property of the sequencing seat, not of the code that finds opportunities. Everything below follows from that.

### Taxonomy, and where the surplus actually comes from

| Strategy | Who pays | Binding constraint | Solo-viable in 2026? |
|---|---|---|---|
| **Atomic / cyclic arbitrage** (A→B→C→A in one tx) | Liquidity providers on the stale pool | Latency + inclusion priority | No — top-of-block is auctioned, not earned |
| **CEX–DEX arbitrage** | LPs on the DEX side | Colocation, CEX inventory, credit lines | No — needs exchange accounts, capital on both legs, ~ms links |
| **Backrunning** (trade immediately after someone else's) | Nobody directly; captures post-trade imbalance | Access to the flow *before* it's public | Only via an OFA, and you split the take |
| **Liquidations** | The undercollateralised borrower (via the protocol's bonus) | Oracle-update timing; often an auction, not a race | Rarely — see the Base Aave measurement below |
| **Sandwiching** | A specific identified trader | A public mempool that still contains victims | Increasingly no — see next section |
| **JIT liquidity** | Passive LPs in the same tick range | Capital + top-of-block adjacency | No — capital-gated |
| **Long-tail / NFT / new-listing** | Whoever is on the other side | Domain-specific edge, tolerance for scams | Sometimes — but it is not an engineering problem |

Note what is *absent* from the "binding constraint" column: nowhere does it say "code quality." That is the finding, not an opinion.

### Extractive vs competitive — a distinction that has practical teeth

**Competitive MEV** (arbitrage, most liquidations) closes a price gap that already exists. Somebody was going to close it; the question is only who. Post-trade, the market is *more* efficient. There is no identifiable victim — the loss is spread across LPs who were already exposed to adverse selection, and against liquidations, the loss is a bonus the protocol *deliberately posts* to get bad debt cleared.

**Extractive MEV** (sandwiching, and most frontrunning) manufactures a worse price for one specific counterparty who was doing nothing wrong, then pockets the difference. There is a named victim, a measurable harm, and a transaction hash proving both.

Why the line matters, in order of how fast it will bite you:

1. **Practically.** Competitive MEV survives infrastructure changes; extractive MEV is what infrastructure is actively being built to kill. You are betting on different halves of a trend.
2. **Reputationally.** Sandwich addresses get tagged, published, and blocklisted by builders. Arb addresses don't.
3. **Legally.** The one clean US precedent — the 2023 DOJ indictment of the Peraire-Bueno brothers — was about *deceiving* validators with a fake bundle, not about arbitrage. Untested law is not the same as safe law, and "there is no victim" is a materially easier position to hold than "there is a victim and I chose them."

Non-preachy version: extraction is a legible target and arbitrage isn't. Choose accordingly.

### Why sandwiching specifically is closing (as of mid-2026)

- **BNB Chain's Good Will Alliance**, launched March 2025, got every major BSC builder (48Club, BlockRazor, bloXroute, NodeReal) to embed sandwich filters in block-building. Daily attack frequency fell from ~140,000 to under 1,000 — [over 95% reduction](https://www.bnbchain.org/en/blog/how-the-goodwill-alliance-slashed-sandwich-attacks-by-95). 48Club's [**bscexorcist**](https://www.bnbchain.org/en/blog/bnb-good-will-alliance) detects buy→buy→sell / sell→sell→buy patterns *across blocks*, so splitting the bundle doesn't evade it.
- **Private routing is now the default path for value.** Blocknative measured the ["flippening"](https://www.blocknative.com/blog/ethereum-private-transactions-the-flippening) — roughly half of Ethereum *gas* now arrives privately. Be precise here: by transaction *count* it's lower (~30%+), and the [ACM Web Conf 2025 study](https://arxiv.org/abs/2410.12352) found 12% of transactions carrying **54.59% of block value** (Jan 2023–May 2024). Either way, the half that matters is not in the public mempool.
- **OFAs pay users to leave.** MEV Blocker returns up to 90% of backrun value to the sender; Flashbots Protect and [MEV-Share](https://docs.flashbots.net/flashbots-mev-share/introduction) do the same. Wallets (Trust, Binance, OKX, MetaMask) ship protected RPCs on by default.
- **Getting sandwiched is now a one-time event.** Users switch RPC and never return. The victim pool is self-draining.

### The arithmetic lesson: you cannot sandwich your own transaction

This is worth stating as a theorem because it is the single most expensive thing to learn empirically.

Under a constant-product invariant `x · y = k`, a sequence of your own swaps that returns your inventory to flat returns the **reserves to their starting point**. Buy `a` of X for Y, then sell that exact Y back: with zero fees you receive exactly `a`. The endpoints are identical to having done nothing — and identical to having done the middle trade alone.

Therefore a self-sandwich nets **exactly zero gross**, and **strictly negative** after fees and gas. A $10,000 self-round-trip on a 0.30% pool burns ~$60 in LP fees across two legs, plus three sets of gas if you wrap a middle trade in it.

**Price impact is not a harvestable resource.** It is not sitting on the curve waiting to be collected — it is paid *to the liquidity providers*, and it is fully reversed when you return to flat. The only thing a sandwich harvests is the delta a **third party** is forced to pay because you moved the price before them. No third party, no revenue. There is no clever bundle construction, no flash-loan sizing, no multi-hop routing that changes this; it is the invariant, not the implementation.

*(This is exactly the arithmetic that killed the "bespoke tranche" idea in `docs/tranche-strategy.md` — the design had no third party in it, so it had no profit source.)*

### How a bundle actually reaches a block (Ethereum, PBS)

`searcher → builder → relay → proposer (validator)`

1. You submit a **bundle** — an ordered, atomic, all-or-nothing tx list — to one or more builders.
2. The builder merges bundles into a full block, maximising total value, and bids for the slot. Your bid is a direct ETH transfer to `block.coinbase` and/or an inflated priority fee.
3. The **relay** holds the block body, shows the proposer only the bid (blinded), and releases the body after the proposer signs the header. This is what stops the proposer from stealing your bundle.
4. The proposer takes the highest bid. [MEV-Boost](https://docs.flashbots.net/flashbots-auction/overview) is the sidecar that runs this.

Two facts about the auction that determine your economics:

- **It is first-price and sealed-bid.** You must shade your bid against unknown competitors. Vertically integrated builder-searchers see their own flow *and* everyone else's, so they effectively bid last. You do not.
- **Almost all of the gross leaves you.** EigenPhi measured [72% of searcher MEV revenue flowing to validators](https://eigenphi.substack.com/p/30m-72-of-searchers-mev-revenue-went) over a two-month window; on contested atomic arbs — where every competitor sees the identical opportunity — bids routinely exceed 90% of gross. Competition on a *public* opportunity drives your margin to roughly the value of your private information, which for a solo operator is zero.

Concentration compounds this: the [top three builders win >95% of slots](https://arxiv.org/abs/2410.12352), and BuilderNet (Flashbots + Beaverbuild + Nethermind, TEE-based, live since Nov 2024) is the main counterweight.

### Bundle RPC methods — and which chains actually serve them

| Chain | Where bundles go | Methods | Reality check |
|---|---|---|---|
| **Ethereum L1** | `relay.flashbots.net`, BuilderNet, Titan, beaverbuild, rsync | `eth_sendBundle`, `eth_callBundle`, `eth_cancelBundle`, `mev_sendBundle`, `mev_simBundle`, `eth_sendPrivateTransaction` ([docs](https://docs.flashbots.net/flashbots-auction/advanced/rpc-endpoint)) | The only chain with a real multi-builder bundle market. Max 100 txs / 300 KB per bundle |
| **BNB Chain** | 48Club Puissant, BlockRazor, bloXroute | `eth_sendBundle`-style | Works — but sandwich patterns are filtered by GWA builders |
| **Polygon PoS** | bloXroute, FastLane/Atlas | `eth_sendBundle` | Thin |
| **Solana** | Jito Block Engine | `sendBundle` (own JSON-RPC, not `eth_*`) | Jito shut its public mempool in March 2024 |
| **Base / OP Stack** | **Nothing public** | — | Single sequencer, private mempool, Flashblocks (200 ms preconfs) since July 2025. There is no bundle to send |
| **Arbitrum** | **Nothing public** | — | No mempool at all. [Timeboost](https://docs.arbitrum.io/how-arbitrum-works/timeboost/gentle-introduction) auctions a 60-second express lane instead |

**Decision rule:** before writing a line of strategy code for a chain, confirm it serves a bundle method you can call. If the answer is "no public mempool and no bundle RPC," the chain is telling you the sequencing seat is not for sale to you — believe it.

### Order flow auctions, and why exclusivity is the actual moat

An OFA sells the *right to backrun* a user's transaction and rebates the proceeds to the user. That is the honest, non-extractive shape of the business — and it also proves the point: the OFA operator earns because they hold the flow. You bid into it.

Arbitrum's Timeboost is the cleanest natural experiment. Since April 2025 it has raised [~$3M in its first three months](https://www.dlnews.com/articles/defi/arbitrum-gets-3m-revenue-bump-from-timeboost/) (1,090 ETH by Sept 2025), and an [empirical study of 151,000 auctions](https://arxiv.org/abs/2509.22143) found **two entities won >90% of them**. When the sequencing advantage was put up for open sale — no relationships needed, just money — it consolidated to two winners in three months. The secondary market for reselling express-lane rights collapsed.

### Should a solo operator do MEV in 2026? Verdict: no

Not as a revenue project. The reasoning, without hedging:

- Every profitable niche is gated on **exclusive flow, the sequencing seat, or colocation**. None is code. You can be the best engineer in the venue and still have no edge, because code quality is not the scarce input.
- The auction structure guarantees that any opportunity **visible to more than one party** clears at ~90%+ of gross paid away. Your margin is your private information; solo, that is zero.
- The residual niches (long-tail, new listings) reward **social access and risk tolerance**, not engineering — you would be competing outside your actual advantage.
- Direct measurement beats theory, and it agreed: Base Aave V3 liquidations were measured at **≈$4.4k/month across the entire venue**, settled by a sealed-bid OEV auction rather than a latency race. Six days live, zero fills. ~$350 of compute for $0.

> **What this means for a solo operator**
> The correct use of MEV knowledge is *defensive and adjacent*, not extractive. Route your own swaps through a protected RPC. Understand adverse selection before you LP. Sell tooling, indexing, or simulation to people who already hold flow. The one thing you should not do is build a bot that competes for public opportunities in a first-price auction against vertically integrated firms — that is a solved game and you are not the solver.

### Red flags — abandon the venue when you see these

- No public mempool **and** no bundle RPC → you cannot sequence, only hope.
- Winner concentration above ~80% in any published auction study → the seat is already owned.
- The strategy's profit source is your *own* price impact → it is arithmetically zero (see above).
- Total venue flow measured in low single-digit thousands of dollars per month → gas and RPC costs eat it regardless of win rate.
- "Our edge is faster/cleaner code" → name the counterparty whose money you are taking. If you can't, there isn't one.
- The opportunity is settled by a **sealed-bid auction** (Chainlink OEV, Atlas, Timeboost) rather than a race → you are bidding against balance sheets, and latency work buys nothing.

*Fast-moving area. Filtering regimes, private-flow shares, and L2 sequencing designs all changed materially between 2024 and 2026 — re-verify any number here before acting on it.*


---

## Exchanges, stablecoins, custody, and operational security

This is the plumbing layer. It produces no alpha — but it is the one layer where a strong engineer's discipline *is* the binding constraint. The venue surveys concluded that surplus in market-microstructure niches goes to whoever holds exclusive flow, the sequencing seat, or a colocation budget, and none of those is code. Ops security inverts that: a leaked key is a 100% loss, and avoiding it is pure engineering. **Spend your rigor here, not on shaving 5ms off a race you were never going to win.**

### CEX vs DEX: choose by what you need custody of

| | Centralized exchange (Coinbase, Kraken, Binance) | DEX / on-chain (Uniswap, Aerodrome, Aave) |
|---|---|---|
| Custody | Theirs. You hold an IOU. | Yours. You hold the key. |
| KYC | Mandatory, full identity | None at protocol level |
| API auth | HMAC key + secret, IP allowlist, withdraw-disabled scope | A private key that can do *anything* |
| Fees | Maker/taker bps, tiered by 30d volume | Pool fee (5–100bps) + gas + slippage/MEV |
| Failure mode | Counterparty (FTX, Celsius) | Contract bug, oracle, your own bug |
| Best for | Fiat on/off-ramp, spot conversion, holding idle capital | Anything programmatic, permissionless assets |

**Decision rules.** Use a CEX for fiat rails and for converting revenue to dollars — nothing else. Never leave working capital there longer than the settlement takes. Create a **withdraw-disabled, IP-allowlisted, read+trade-only API key** for anything automated; a key with withdrawal scope on a box you also SSH into is a bearer instrument. And note the structural point: CEX maker rebates and colocation tiers are gated on volume you do not have. You cannot engineer your way into tier 5.

### Stablecoins: which dollar, and on which chain

| Token | Issuer | Backing | Key risk | Notable depeg |
|---|---|---|---|---|
| USDC (~$73B, [DefiLlama](https://defillama.com/stablecoins), Jul 2026) | Circle | T-bills + bank cash | Bank exposure, freeze authority | $0.87, Mar 2023 (SVB, $3.3B stranded) |
| USDT (~$184B) | Tether | T-bills, gold, BTC, secured loans | Attestation ≠ audit; opacity | ~$0.95, May 2022 |
| DAI / USDS | Sky (ex-MakerDAO) | Overcollateralized + heavy USDC/RWA | Inherits USDC risk via PSM | Followed USDC to ~$0.90, Mar 2023 |

USDS is Sky's 2024 successor to DAI, upgradeable 1:1; combined supply is roughly $11–13B in 2026 with USDS now the larger leg. The Sky Savings Rate on sUSDS ran ~3.75–4.5% in early 2026 — **verify live before assuming**, it is a governance parameter.

**The bridged-vs-native trap.** "USDC" on an L2 may be two different ERC-20s. Native USDC is minted by Circle and redeemable 1:1 with Circle; bridged (USDC.e, USDbC) is a lock-and-wrap claim you must unwind through the bridge first. They are not fungible, they have separate liquidity, and routing into the wrong one is the single most common self-inflicted loss for people writing their first bot.

| Chain | Native USDC | Bridged legacy |
|---|---|---|
| Base | `0x8335…02913` | USDbC `0xd9aA…10b6CA` |
| Arbitrum | `0xaf88…e5831` | USDC.e `0xFF97…DB5CC8` |
| Optimism | native since 2023 | USDC.e persists |

**Decision rule: hardcode the address, never the symbol.** Resolve every token from a pinned constant checked against [Circle's multi-chain USDC page](https://www.circle.com/multi-chain-usdc) and the chain's own docs, and assert `decimals()` on startup (USDC/USDT are 6, DAI/USDS are 18 — a 10^12 error is a total loss).

Regulatory note, dated: the GENIUS Act (signed Jul 2025) is in rulemaking, with [Treasury](https://home.treasury.gov/news/press-releases/sb0435) and OCC proposals through 2026 and practical effect landing 2026–2027. Expect issuer disclosure and foreign-issuer rules to shift. Re-check before building anything that depends on a specific issuer's status.

### Bridging

Bridges are the **largest single loss category in crypto history** — roughly $2.9B cumulative, ~40% of all Web3 value hacked. The canonical failures were validator-key compromise (Ronin, ~$625M, 5-of-9 keys), signature verification bugs (Wormhole, ~$326M), and an initialization bug that made replay free for anyone (Nomad, ~$190M). Bridge exploits were still ~42% of exploit losses in mid-2026.

| Route | Trust | Time | Use when |
|---|---|---|---|
| Canonical optimistic (Base/OP) | Rollup security | **7-day** challenge window out | Large, non-urgent, security-first |
| Canonical Arbitrum | Rollup security | ~6.4 days out | Same |
| Canonical ZK (zkSync/Scroll) | Validity proof | Hours | Same, faster |
| CCTP (USDC burn/mint) | Circle | Minutes | Moving USDC specifically |
| Third-party (Across, Stargate, LayerZero) | Extra validator/relayer set | Minutes | Small, time-sensitive, accepted risk |

Deposits *into* an L2 are fast; withdrawals eat the fraud-proof window. Third-party "fast bridges" are liquidity providers fronting you the funds and eating the delay — you are paying to skip the window and adding their trust assumptions. Check the actual trust model on [L2BEAT](https://l2beat.com/) before moving size. **Rule: never bridge more than you would accept losing outright, and never route a bot's operating float through a third-party bridge on a schedule.**

### Wallets and key management for an automated system

An automated signer needs a hot key by definition. The design goal is not "no hot key" — it is **minimizing what one hot key can lose.**

| Store | Cleartext on disk? | Fit |
|---|---|---|
| `.env` with `PRIVATE_KEY=0x…` | **Yes** | Never in production |
| Keystore JSON (scrypt/AES) + env password | No | Correct default for a worker |
| Hardware wallet (Ledger/Trezor) | No | Treasury, manual ops only |
| Safe multisig | N/A | Treasury and contract ownership |

`project-chimera` already does the right thing, and it is worth naming as the pattern: keystores live outside the repo at `$HOME/.chimera/keystores/`, only *addresses* go in `.env.live`, and treasury is a separate role from workers (`core/src/signer_registry/mod.rs`, `.env.live.example`).

**The systemd hazard, specifically.** `deploy/chimera.service` uses `EnvironmentFile=/opt/chimera/.env.live`, which loads the whole file into the process environment. That means `CHIMERA_KEYSTORE_PASSWORD` sits in `/proc/<pid>/environ` (readable by the process UID and root), gets inherited by every subprocess, and lands in any crash dump or telemetry payload that serializes the environment. `Restart=on-failure` means a leak-on-crash repeats. Mitigations: `chmod 600`, `LoadCredential=`/`systemd-creds` instead of a flat env file, and never shell out from the signer process. Also check the unit's `ProtectHome=yes` against your keystore path — if the keystores are genuinely under `/home/chimera`, that directive makes them unreadable to the service, and you will discover it at go-live.

### Key hygiene rules

1. **Treasury ≠ operational.** Treasury is a Safe or hardware wallet. Workers are hot keys holding only gas plus one trade's float.
2. **One key per worker.** Nonce isolation is the operational reason; blast-radius isolation is the security reason.
3. **Contract ownership goes to the Safe, not the hot key.** If the executor has an owner-only sweep, the owner must not be the key running unattended.
4. **A key that touched a repo is burned.** `git filter-repo` does not help — assume it was cloned and scraped within minutes. Generate new, move funds, revoke every approval (`revoke.cash`), rotate. Same for any key ever pasted into a log, a chat, or a terminal you screenshotted.
5. **Rotate on schedule, not on incident** — quarterly, or whenever a machine is decommissioned.
6. **Approvals are keys too.** An unlimited ERC-20 approval to a compromised contract drains you without touching your private key.

### RPC providers

| Provider | Free tier | Paid entry | Notes |
|---|---|---|---|
| Alchemy | 30M CU/mo, archive included | ~$49/mo | Archive on free tier is the differentiator |
| QuickNode | 50M credits | $49 → $299 → $900 | Flat-rate RPS from $799/mo (Mar 2026) |
| Infura | 100k req/day | ~$225/mo | Expensive per eth_call |

**Compute units are not comparable across providers.** One `eth_call` is billed 1 unit on some providers, 26 on Alchemy, 80 on Infura. Budget in *requests you will actually make*, then convert per provider — this is where a survey script quietly burns a month of credits in an afternoon.

Archive vs full: a full node only serves state for the last ~128 blocks. **Any historical `eth_call` — backtesting, "what was this position's health factor at block N" — requires archive.** Self-hosting archive on Base or Ethereum means multiple TB of NVMe and a week of sync; at ~$150–300/mo for the hardware it only beats a hosted plan if you are doing sustained historical work. For everything else, buy it. And keep two providers configured with failover: a single provider outage is a silent 100% downtime for a bot whose WebSocket just stopped delivering heads.

### Gas and fee mechanics

- **L1 (EIP-1559):** `base fee` is burned and adjusts ±12.5% per block toward 50% target utilization; `priority fee` is the tip to the proposer. You set `maxFeePerGas` and `maxPriorityFeePerGas`; you pay base + tip and are refunded the rest. Setting `maxFeePerGas` too low doesn't fail fast — it leaves your tx pending across a base-fee spike, which is how nonces get stuck.
- **L2 total cost = L2 execution + L1 data availability.** On OP Stack chains the DA component dominates and is computed by the `GasPriceOracle` predeploy at `0x420000000000000000000000000000000000000F` (`getL1Fee(bytes)`, `l1BaseFee`, `blobBaseFee`). Post-Ecotone it is priced off blobs. If your profitability model only uses `gasUsed * gasPrice`, **it is wrong on every L2** — you are omitting the larger half.
- **Blobs (EIP-4844):** a separate 1D fee market with its own base fee, historically pinned near the 1-wei minimum. [EIP-7918](https://eips.ethereum.org/EIPS/eip-7918) (Fusaka) floors it relative to execution cost. Capacity was raised via BPO forks to a target of ~14 blobs and max 21 as of Jan 2026, with further increases planned — which is why an L2 tx that cost ~$0.50 in late 2025 ran $0.20–0.30 after. **Dated claim; re-check before modeling.**

### Monitoring an automated system

Alert on **state that silently invalidates the whole system**, not on individual missed opportunities.

| Alert | Threshold | Why |
|---|---|---|
| Hot wallet gas balance | < 3× worst-case tx cost | The #1 cause of silent death |
| Head block age | > 2× block time | WebSocket subscriptions fail *open* |
| Pending tx age | > 20 blocks | Stuck nonce blocks everything behind it |
| RPC error rate / credit burn | >2% errors; >daily budget | Rate-limiting looks like "no opportunities" |
| **Unexpected outflow** from any managed address | any transfer you didn't sign | The only alert that pages you at 3am |
| Execute-mode flag | any transition, and a daily "current mode" heartbeat | Chimera's `.env.live` reportedly still reads `EXECUTE_MODE=live` against a real mainnet executor — a mode you *believe* is off is worse than one you know is on |
| Restart count | >3 in 10 min | `Restart=on-failure` masks crashloops |
| Cumulative gas spent with zero revenue | your own kill number | The alarm that would have saved months |

That last row is the real lesson. Instrument the burn, set a number in advance, and let it fire.

### Taxes and records

Not tax advice — this is the record-keeping that makes tax work *possible* later, and reconstructing it after the fact from a block explorer is brutal.

Append one JSONL line per on-chain action, written **at execution time**, never derived later:

```json
{"ts_utc":"2026-07-26T14:03:11Z","chain_id":8453,"block":33812901,
 "tx_hash":"0x…","from":"0x…","to":"0x…","direction":"out",
 "asset":"USDC","token":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
 "amount_raw":"1500000000","decimals":6,
 "usd_price":1.0002,"price_source":"chainlink:0x7104…Bb70@block",
 "usd_value":1500.30,"gas_used":214553,"effective_gas_price_wei":"4210000",
 "l1_fee_wei":"38210000000000","fee_usd":0.19,
 "status":"success","tag":"liquidation_v2","note":""}
```

Non-obvious rules: (1) **every swap is a disposal** in most jurisdictions, including stable-to-stable; (2) **failed transactions still cost gas** and are still a real outflow — log reverts with the same schema; (3) record *which* price source you used and at which block, because "the price of ETH" is not a single number; (4) append-only, one file per chain per month, backed up off the box — the box is a hot wallet host and may need to be destroyed on short notice.

### What this means for a solo operator

Every item above is a loss-avoidance control, not a revenue source. That is exactly why they deserve your attention: in the venues surveyed, code quality could not buy alpha, but it can reliably buy *not getting rekt*. The asymmetry is stark — months of engineering produced zero revenue, while one cleartext key in a repo or one unlimited approval to the wrong contract produces an instant, total, unrecoverable loss. Treat the operational floor as the actual deliverable of a solo crypto project, and treat every strategy on top of it as an experiment you expect to fail.
