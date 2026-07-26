# Where the Liquidation Money Actually Is — and Why It Isn't Us

**Date:** 2026-07-25
**Extends** [flow-measurement-2026-07-25.md](flow-measurement-2026-07-25.md)
**Scanner:** [`scripts/scan_liquidation_venues.py`](../scripts/scan_liquidation_venues.py)
**Status:** measured, then adversarially verified. Several of this document's own
first-pass numbers were overturned by that verification and are corrected below.

---

## 1. Headline

Base Aave V3 — the venue this engine was built for — is **not a market**. Its
entire 30-day liquidation bonus pool is **$4,404** across 22 liquidators, and
**~100% of that is bid away to the protocol** through a Chainlink OEV auction the
engine does not participate in.

| Venue (30d to 2026-07-25) | Events | Seized USD | Bonus (gross) | Liquidators | Real top-3 | New-entrant $/yr, net of infra |
|---|---:|---:|---:|---:|---:|---|
| ethereum / Morpho Blue | 261 | $7.93M | ~$146k | 79 | ≤47.7% (but 73.8% of value is one market) | +$7.3k … +$95.1k — **contested, see §7** |
| ethereum / Aave V3 | 174 | $3.44M | $295,194 | 27 | 82.8% | −$5.0k … +$122.7k — marginal, SVR-taxed |
| base / Morpho Blue | 317 | $851,775 | **$50,501** | 61 | 88.6% | −$3.4k … +$37.0k — marginal, cheapest place to fail |
| ethereum / Spark | 48 | $316,042 | $71,942 | 5 | 100% (one event) | **DO NOT ENTER** |
| **base / Aave V3** | **116** | **$79,790** | **$4,404** | **22** | **99.2%** | **−$6.0k … −$1.1k — DO NOT ENTER** |
| base / Seamless | 44 | $146 | $7 | 5 | 92.6% | **DO NOT ENTER** |

Base Aave's *entire* pool beyond the top three liquidators is **$604 over 30 days**
(~$36/yr). The 4th-place liquidator on Ethereum Aave V3 seized more in 30 days
than the whole Base Aave market produced for everyone.

### Two corrections this document had to make to itself

**Base Morpho was overstated by 63%.** The first pass reported $137,587 — a 16.2%
bonus-to-seized ratio, structurally impossible against Morpho's LIF ceiling of
13.04% of seized. Bad debt was ruled out (4 of 322 events, $0.00 contribution).
The cause was valuing both legs at *current* prices while collateral appreciated
over the window. Three independent re-derivations converge:

| method | bonus | % of seized |
|---|---:|---:|
| structural per-market LIF | $56,193 | 6.6% |
| DefiLlama historical prices | $55,528 | 7.2% |
| **market oracle read at each event's block** | **$50,501** | **6.60%** |

The oracle-anchored figure is authoritative: reading each market's `price()` at
each event's block reproduced the protocol's LIF invariant on **320 of 322
events to 4+ decimals**. Net of measured DEX slippage, *realized* liquidator
profit was **~$32,400**. Note the same current-price artifact inflates every
gross-bonus figure in the table above; only Base Morpho has been re-derived.

**Concentration was overstated on Ethereum Morpho.** Its rank-#2 "liquidator"
(`0x6566…0245`, 25.3%) is **Morpho Bundler3 — a public open-source multicall
router with ≥12 distinct EOA callers**, not a competitor. Real single-entity
top-3 is **≤47.7%**, making Ethereum Morpho the most contestable venue measured,
not one of the most concentrated.

---

## 2. Base Aave is a closed channel — measured, not inferred

Decoding the incumbent's winning transaction (`0x2cad356c…d496`) settles the
mechanism:

- The submission contract `0x583dcFef…9b77` is **canonical FastLane Atlas
  v1.6.4** — confirmed by FastLane's published `atlas-config` for chain 8453 and
  by on-chain `name()`="Atlas ETH" / `symbol()`="atlETH" with matching
  `VERIFICATION()`/`SIMULATOR()`.
- Selector `0x4317ca01` = `metacall(UserOperation, SolverOperation[],
  DAppOperation, address)`, verified by recomputing the keccak of the expanded
  tuple signature.
- The UserOperation is an oracle update against **`ChainlinkOevDAppControl`**.
  In that transaction, **log[0] is the SVR round update and log[14] is the
  liquidation — same transaction.**
- **Two competing solver bids**: incumbent 0.05837 ETH, runner-up 0.05477 ETH.
  Won by 6.6%. **First-price settlement**, proved by a wei-exact balance diff;
  FastLane's share is 0, so the whole bid goes to the protocol.

**Consequences, in order of importance:**

1. **The trigger price is not observable on a public RPC in time.** Aave V3 Base
   reads SVR DualAggregators across effectively its whole book (WETH and cbBTC
   directly; USDC, cbETH, wstETH, weETH, wrsETH, EURC via Capped adapters
   wrapping DualAggregators). The price that makes a position liquidatable
   arrives either inside the winning solver's bundle or **30 seconds later** via
   primary-feed fallback. A public-RPC bot sees the opportunity only after it is
   gone. **This is not a latency problem** and cannot be fixed with a faster RPC,
   a colocated node, or a mempool subscription.
2. **There is nothing left outside the channel.** **99.5% of Base Aave
   liquidation value over 28 days went through Atlas.** The residual 81 non-SVR
   transactions total **$364 of debt repaid and $17.92 of bonus** — dust,
   self-liquidations and interest-accrual cleanups.
3. **The residual to the winner is ~0%, not the ~11% first believed.** In the
   flagship transaction the winning solver's ETH balance rose **~$0.11** while
   Atlas paid the bundler ~$0.47. A first-price auction against a
   zero-marginal-cost incumbent drives the clearing price to ~100% of the bonus.
   *An earlier draft of this document estimated the incumbent nets
   $1,000–1,500/month. That was wrong* — it assumed the bid retained a margin.
   The reason only one serious bidder exists is not that the opportunity is
   undiscovered; it is that there is nothing to win.

> **Caveat, stated honestly:** the ~$0.11 residual is *ETH-denominated*. The
> incumbent holds token inventory (USDC $20.8k / cbBTC $10.9k / WETH $1.1k), so
> profit retained as token balances was not measured. The direction is
> unambiguous; the exact residual is not.

### The incumbent does have structural edges (correcting an earlier claim)

An earlier draft said "the incumbent has no privileged position — they just bid
6.6% more." **That is wrong.** Chainlink's own SVR searcher documentation
establishes three real barriers:

1. **A reputation gate.** Atlas includes **at most 2 searcher bids per
   transaction**, and **one slot is always reserved for the high-reputation
   set.** The "two competing solvers" seen in every sampled transaction is a
   *consequence of the slot cap* — not evidence of open competition, as I first
   read it.
2. **Parallel auctions.** BTC and ETH feeds on Base spawn **two independent,
   uncoordinated auctions** per opportunity. Bid only one and you win roughly
   **50% of the time even holding the best bid**.
3. **One SolverOperation per block per EOA.** Covering both parallel auctions
   therefore requires **multiple separately-bonded EOAs** — which is exactly what
   the incumbent's rotating 6+ signer fleet is for. That fleet is not
   nonce-parallelism for throughput; it is auction coverage.

Also correcting: **"losing bids cost zero gas" is only half true.** Losing the
auction or failing simulation is free, but a solver operation that *executes
on-chain and reverts* still pays its gas to the bundler.

And on scope: since Chainlink's acquisition of Atlas (announced 2026-01-22),
**Atlas exclusively serves Chainlink SVR**. An outsider cannot route arbitrary
Aave liquidations through it — confirmed on-chain by the single fixed
`authorizedUserOpSigner`. One genuine opening remains: **Aave liquidations driven
by non-SVR feeds or by pure interest accrual never generate an Atlas auction at
all** and remain an ordinary public race.

---

## 3. Who the incumbent is

`0xd12810b19b596347a3afac206d3ca65d08594b3f` — **genuinely unattributed**. Zero
hits across four search backends and Blockscout name tags on five chains. No
firm, no Dune dashboard, no ENS.

- Funded by a **retail Binance withdrawal of 0.0402 ETH (~$65)**; its master
  operator EOA has pre-bot personal history (USDT transfers, Uniswap swaps, a
  2023 bridge interaction). No MEV firm reuses a personal wallet as a master key.
- **~39 liquidations/month across all chains.** A side project at the same
  operator scale as Project Chimera.
- Same address on six chains via `CREATE(deployer, nonce=1)` mirroring (verified
  by recomputing `keccak256(RLP([deployer,1]))`). Only Base and Arbitrum are
  active; **Optimism did zero liquidations in 30 days** despite a 2-year-old
  deployment.
- **Off Base they are ordinary:** #4 on Arbitrum Aave (4.3%), and **completely
  absent from Base Morpho Blue** — 11× the money on the same chain.

Their edge is not latency, capital or sophistication. It is inventory in the
three assets that actually liquidate (so they need no swap engine), **zero-fee
Morpho flash loans** for the tail (Chimera pays Aave 5bps), a UUPS proxy they
upgrade freely, and being registered in the auction. Median priority tip ~0.10
gwei (~$0.20–0.50) — nobody is racing them.

---

## 4. Why it isn't us — ranked

1. **Structural: the prize does not exist.** $4,404/month gross for the entire
   market, ~100% auctioned away, $604/30d beyond the top three. No engineering
   fix changes this.
2. **Structural: we are not in the auction.** `docs/monetization.md` names
   "protected/private tx submission — not wired" as the deficiency. That framing
   is **wrong for Base**: there is no public mempool to be front-run from, so a
   Flashbots-style private RPC buys nothing. The real gap is auction absence.
3. **Self-inflicted: the pacing engine is built for the opposite problem.** Four
   actions/day max, a **$1,000 cap that would refuse the largest event of the
   month** ($20.7k seized), and no absolute profit floor — so the single slot per
   6 hours is burned on $0.04 dust, which was 72 of 116 events.
4. **Self-inflicted: the Yul froze the swap shape.** `Executor.yul:368-379`
   hardcodes selector `0x38ed1739` with a fixed 2-address path, in assembly →
   only `router_compatibility: "v2"` venues → only `sushi-base` → **exactly one
   pair, WETH→USDC**. cbBTC (45% of Base Aave's seized value) and EURC appear
   nowhere in the codebase.
5. **Self-inflicted: the simulator validates a transaction that is never sent.**
   It calls the Pool's `liquidationCall` from a synthetic address; the flash
   loan, swap, repay and the Executor's own profit gate never execute. `gas_used`
   is therefore wrong, and it sets `gas_limit` and feeds the profit multiplier.
6. **Self-inflicted: the measurement came last.** 22 documents, five remediation
   waves, a threat model and seven runbooks preceded one day of stdlib Python
   that invalidated the roadmap.
7. **Latent bugs that would make a soak look healthy while producing nothing.**
   Venue rotation **permanently self-locks** the only Base venue after the first
   swap-requiring opportunity (in shadow mode too). `execute_live` books SUCCESS
   the instant `send_raw_transaction` returns and never polls the receipt — so
   the revert breaker is dead code in live mode and PnL is fiction.

---

## 5. Arbitrum: a verified dead end today

1. **No working swap route exists.** Bytecode probe for the selector
   `Executor.yul` requires:

   | Router | Chain | Has `0x38ed1739`? |
   |---|---|---|
   | sushi-base (positive control) | Base | **YES** |
   | Aerodrome | Base | no |
   | **camelot-arbitrum** — the only `v2`-tagged venue | Arbitrum | **no** |
   | uniswap-v3-arbitrum (`# TODO`) | Arbitrum | no — it is the V3 *factory* |

   Chimera on Arbitrum could exit **no** collateral.
2. **Arbitrum Aave is also SVR-routed**, so the same closed-channel logic applies.
3. **It is a more contested market**, not less: the Base incumbent ranks #4 there
   behind three larger professionals.
4. Porting is **1–2 weeks** — new deploy + multisig, borrower discovery from
   scratch, and the Arbitrum L1-fee model is a literal guess
   (`calldata_len * 32 + 1400`) despite `architecture.md` claiming otherwise.

---

## 6. A premise this project had wrong

**Ethereum gas is not prohibitive.** Measured over the window: median base fee in
blocks containing liquidations is 0.097 gwei on Ethereum vs 0.005 on Base
(**19.5×**), and median all-in fee per liquidation transaction is **3.7×** —
**$0.083 median on Ethereum Aave**. The assumption that Ethereum is 100–1000×
costlier, which kept attention on L2s, is false. Ethereum's larger tickets
(avg $74.5k vs Base Aave's $5.4k) dwarf the fee difference.

---

## 7. Recommendation

**Stop work on Base Aave V3.** It is a closed channel worth −$1.1k to −$6.0k/yr
net of infrastructure. Do not build the swap engine, do not wire "private
submission" as currently framed, do not port to Arbitrum.

**Do not become an Atlas/SVR solver as a Base Aave revenue plan.** Onboarding has
no gatekeeper (~0.1 ETH recommended bond, an `atlasSolverCall` contract, a
websocket) — but with ~0% residual, entering means bidding ≥100% and losing money
on gas, or bidding less and never winning. *The "~11% residual" figure that made
this look investable has no traceable source and is contradicted by direct
measurement.* And per §2 you would enter against a reserved high-reputation slot,
a 2-bid cap, and parallel auctions that require multiple bonded EOAs to cover.

**Adopt a standing two-call gate before any new target:** call
`getSourceOfAsset` on the protocol's oracle, then `typeAndVersion` on the
aggregator. **If it returns `DualAggregator`, the venue is closed to public-RPC
competition — drop it immediately.**

**If work continues, the ranked targets are — and two independent measurements
disagree about the first one, so treat it as unresolved:**

1. **Ethereum Morpho Blue — best of a poor set, but its headline size is
   inflated by one-off events.** One measurement puts the rank-9+ open pool at
   **$265k/yr across 71 addresses** (+$7.3k to +$95.1k/yr net of infrastructure).
   A second, adversarial measurement finds **73.8% of its 30-day value is a
   single market** (an AVLT/USDC vault unwind across just 22 borrowers); strip
   that and recurring flow is $1.53M repaid at top-1 39.6% / top-3 68.9% — about
   **4.6 effective firms**, i.e. concentrated, not fragmented. Both agree there
   is *some* contestable tail; they disagree by roughly an order of magnitude on
   its size. **Resolve this before writing code** — the deciding question is what
   the tail looks like with one-off market unwinds excluded.
2. **Liquidation classes that need no fresh price at all** — interest-accrual and
   rate-drift insolvencies generate no Atlas auction and remain open to anyone.
   This is the one structurally open niche identified anywhere in this work, and
   it is the cheapest to test because it needs no new venue, chain, or contract.
3. **Base Morpho Blue — as a proving ground only.** Same chain, same RPC, same
   operational surface, zero-fee flash loans, incumbent absent. But **84% of its
   value is the single cbBTC/USDC market**, where two contracts take 85.5%; the
   tail outside cbBTC is **$4–8k of gross bonus per 30 days** — the same order as
   the Base Aave pool this document rejects as too small.

**The honest summary of all three: no venue measured offers many distinct winners
sharing meaningful value.** This is a mature, concentrated category everywhere
checked. Option 2 is the only one that is structurally open rather than merely
smaller.

**What survives any pivot:** the Aave V3 health-factor math
(`detector/liquidation.rs` — the best code in the repo), the pacing/risk engine
mechanism, the REVM simulation harness with its ERC20 storage-layout probing,
signer/nonce management, the operational surface, and the measurement tooling —
which is the only artifact here that has ever changed a decision.

**What does not:** `Executor.yul` entirely, the venue/pair tables, the Base
borrower set, and the hardcoded per-chain token constants.

---

## 8. Method note

Web-sourced claims failed repeatedly under on-chain testing: the SVR fallback is
**30 seconds, not 60** (the 60 came from a governance *proposal*, not deployed
state), and the "89% / 51.5% / 11% residual" split **has no source at all** and
is contradicted by measurement. The on-chain test was decisive and cost two RPC
calls.

But the measurements in this document were wrong too, and more often. Corrected
on the way to this version: Base Morpho's bonus (overstated 63% by current-price
valuation), Ethereum Morpho's concentration (its #2 "liquidator" is a public
router), the incumbent's monthly take (assumed a bid margin that does not exist),
the incumbent's structural edges (asserted as none; there are three), and the
claim that losing bids never cost gas. **Every one was caught by re-deriving
against a protocol invariant or reading chain state — none by more reading.**

Two independent measurements still disagree by roughly an order of magnitude on
the size of Ethereum Morpho's contestable tail (§7). That is flagged rather than
resolved, and no recommendation should rest on it until it is.
