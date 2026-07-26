# Project Chimera — Executive Brief

**Aave V3 liquidation engine on Base (chain 8453) · Rust + Yul**
**Report date:** 2026-07-25 · **Repo state:** 6 commits on `main`, unpushed; 9 untracked analysis artifacts
**Evidence base:** 14 subagents across 3 workflows, ~4.2M subagent tokens, all headline numbers measured on-chain

---

## How to use this document

This is a handoff brief written to be read by another model or engineer with **no
prior context on the project**. It is organised so that:

- **§1–2** give the verdict and the system as built.
- **§3–5** give what worked, what failed, and why — the failure taxonomy is the
  most transferable part.
- **§6–8** give the measurement program, the competitive landscape, and the two
  adversarial judge panels verbatim.
- **§9** is a **corrections log**: every claim this investigation got wrong and
  then overturned. Read it before trusting any number here, including the ones
  here.
- **§10–12** give the decision matrix, the recommended plan, and the salvageable
  assets.
- **§13** lists the tooling (connectors, skills, plugins) for continuing this work.
- **Appendices** carry every contract address, transaction hash, event topic and
  selector needed to reproduce the findings independently.

**Epistemic status marker used throughout:** `[MEASURED]` = verified on-chain in
this investigation. `[REPORTED]` = from documentation or web sources, not
independently confirmed. `[UNMEASURED]` = explicitly unknown.

---

## 1. Verdict

1. **The market Project Chimera was built for does not exist at the assumed
   size.** The *entire* Base Aave V3 liquidation bonus pool was **$4,404 over 30
   days**, across 22 liquidators. `[MEASURED]`
2. **It is also structurally closed.** 99.5% of that value routes through a
   Chainlink SVR sealed-bid oracle auction (via FastLane Atlas) that the engine
   does not participate in. The trigger price arrives *inside the winning
   solver's own bundle*. This is not a latency problem and cannot be fixed with a
   faster node. `[MEASURED]`
3. **Every venue examined fails in one of exactly three ways** — gated, a latency
   race, or denominated in collateral nobody has proven is sellable (§3.1).
4. **The one open, non-race niche is real** — interest-accrual liquidations,
   ~$3.45M/yr gross — **but 96% of it is one venue, 59% is one market, and its
   collateral has an unproven exit.** `[MEASURED]` / `[UNMEASURED]` exit
5. **Recommended next action is a 3-day, zero-code, zero-capital kill test**
   (§11). Honest outcome distribution: **40% dead on day 3, 25% dead on day 13,
   20% earns a few thousand a year, 15% clears $25k/yr.**

> The engineering was not the problem. The engine was pointed at a market that
> was measured for the first time on day 5 of its live deployment — *after* 22
> design documents, five remediation waves, a threat model, a license migration
> and seven runbooks had been produced. The measurement cost one day of
> stdlib Python.

---

## 2. What was built

**~30,138 tracked lines.** A single-process Rust scanner with a non-upgradeable
Yul executor contract, throttled by a pacing engine, submitting through a plain
public RPC.

### Architecture

| Layer | Implementation | Status |
|---|---|---|
| **Discovery** | `scripts/snapshot_generator.py` — Multicall3-batched sweep of Aave `Borrow` events; 214,180 Base borrowers indexed through block 49,112,085; checkpointed concurrent backfill | Works; offline batch, not a live index |
| **Detection** | `core/src/detector/liquidation.rs` — RAY fixed-point health factor, two-tier close factor, eMode LT override, isolation mode, siloed borrowing, bad-debt detection | Best code in the repo |
| **Pricing** | `core/src/oracle/{aave,chainlink}.rs` + `snapshot_refresh.rs` — batched `getAssetsPrices` every 2s, per-asset feed quarantine with TTL re-admission | Sound |
| **Simulation** | `core/src/simulator/` — REVM + `CacheDB<WrapDatabaseAsync<AlloyDB>>`; `seeding.rs` discovers ERC20 storage layouts by probing, memoised per token | `seeding.rs` is genuinely original; `mod.rs` simulates the wrong transaction (§4.2) |
| **Risk governor** | `core/src/pacing_engine.rs` — Decimal-only money math, rolling daily/weekly windows, breaker state machine, crash-safe JSONL, cross-process reservation | Mechanism sound, defaults fatal (§4.1) |
| **Execution** | `contracts/src/Executor.yul` (387 lines) — Aave `flashLoanSimple` → `liquidationCall` → V2 swap → profit gate | Cannot be reused (§4.3) |
| **Submission** | `core/src/executor/submitter.rs` + `orchestrator.rs` — plain `send_raw_transaction` | No protected/auction path at all |
| **Custody** | `core/src/signer_registry/` — encrypted keystores, per-address atomic nonces, treasury/worker split | Sound |
| **Ops** | Prometheus metrics, Grafana, Alertmanager, emergency flag, 20 operational scripts, 7 runbooks | Disproportionately mature |

### Test and gate status

`cargo test` 224 pass · `clippy` clean · `forge test` 26/26 · `slither` zero-HIGH.

**Blind spots a green gate does not cover** `[MEASURED]`:
- `slither` never analyses `Executor.yul` (it fails with `KeyError:'ast'` on Yul;
  the CI procedure moves `.yul` files aside, so the executor is never scanned).
- `ExecutorBaseFork.t.sol` opens with `if (bytes(forkUrl).length == 0) return;` —
  **it silently no-ops without `BASE_FORK_URL` set.**

---

## 3. The central finding: three failure modes

### 3.1 Taxonomy

Every venue measured resolves to one of these:

**(1) GATED — the trigger price is auctioned before you can see it.**
Chainlink SVR (Smart Value Recapture) routes a protocol's liquidation flow into a
sealed-bid oracle auction. Covers **Aave, Compound and Venus** — Base Aave,
Ethereum Aave, Compound's absorb trigger. On Base the auction runs through
FastLane Atlas; on Ethereum through Flashbots MEV-Share. `[MEASURED]`

**(2) A LATENCY RACE — open, but won on speed you do not have.**
Base Morpho (98.8% of its bonus is price-push triggered), all six dutch-auction
venues, crvUSD LLAMMA, Compound's 65.9% same-transaction capture. **The
"capital and patience beat latency" premise is falsified: with flash loans
available, a descending-price auction is a latency race.** `[MEASURED]`

**(3) UNSELLABLE COLLATERAL — open, not a race, but denominated in assets with
no proven exit.** AVLT, AZND, ROY-ST-apyUSD, ynRWAx. `[MEASURED]` that these
dominate; `[UNMEASURED]` whether they can be exited.

**The niche this investigation spent its whole budget hunting — open, large, not
a race — exists, is worth ~$2–3.45M/yr gross, and lands entirely in category 3.**

### 3.2 The openness gate (reusable, 2 RPC calls)

```
Pool → ADDRESSES_PROVIDER() → getPriceOracle() → getSourceOfAsset(asset)
     → typeAndVersion() on the returned aggregator
```

`"DualAggregator"` ⇒ **CLOSED**. Plain `EACAggregatorProxy` /
`AccessControlledOCR2Aggregator` ⇒ **OPEN**.

Implemented as `scripts/check_venue_open.py`. Results `[MEASURED]`:

| Venue | Asset | Aggregator | Status |
|---|---|---|---|
| Aave V3 (Base) | WETH, cbBTC | `DualAggregator 1.0.0` | **CLOSED** |
| Aave V3 (Ethereum) | WETH | `DualAggregator 1.0.0` | **CLOSED** |
| Spark (Ethereum) | WETH, wstETH | aggregated price feed | OPEN |
| Seamless (Base) | WETH, USDC | `AccessControlledOCR2Aggregator 1.0.0` | OPEN |
| Morpho Blue (both) | per-market | not SVR | **OPEN by construction** |

**Morpho is open for a structural reason, not an empirical one: SVR covers
Aave/Compound/Venus and Morpho carries its own per-market oracles. It cannot be
auction-gated on any chain.** This is the single most useful fact in the report.

**Caveat:** Base/Ethereum USDC and WBTC sit behind "Capped" adapters that do not
expose `typeAndVersion`; earlier work indicates these wrap DualAggregators, so
treat them as closed. `[REPORTED]`

---

## 4. What failed

### 4.1 The pacing engine is built for the opposite problem

Four hard caps, and **they are not editable in YAML** — `Config::validate()`
(`core/src/config.rs:188-197`) hard-rejects values above the limits. This is a
code change, not a config change. `[MEASURED]`

| Setting | Effect |
|---|---|
| `min_interval_hours: 6` (+ 0–12h jitter) | ≤4 attempts/day against venues producing 4–11 events/day. **The jitter is dead code** — `release_at` is computed at `pacing_engine.rs:187-189` and its only non-test consumer, `orchestrator.rs:639`, matches the variant and discards the payload. |
| `max_single_transfer_usd: 1000` | **An anti-selection filter, not a throttle.** `pacing_engine.rs:135` compares it against `expected_net_usd` — *net profit*. It rejects precisely the tail that carries the value on every venue measured, while admitting dust. **Inversely correlated with income.** |
| `max_daily_net_usd: 2000` | Combined ceiling: **$2,000/day**. |
| `venue_rotation_count: 5` | The deque holds count−1 = 4. On Base **exactly one** venue passes the resolver filter (`sushi-base`), so it enters the window and can never be evicted: **the engine can execute at most ONE non-same-asset liquidation per process restart, forever.** Structural deadlock, and it fires in shadow mode too. |

This is a "don't look like a bot" OPSEC posture applied to a latency-and-capital
game. It is the project's stated risk policy, not a defect — but **you cannot
keep the posture and win.**

### 4.2 The simulator validates a transaction that is never sent

`core/src/simulator/mod.rs` calls the Aave Pool's `liquidationCall` directly from
a synthetic address. **The flash loan, the DEX swap, the repayment and the
Executor's own profit gate never execute.** Consequences: `gas_used` is the gas
of a bare `liquidationCall` (and it sets `gas_limit` and feeds the profit
multiplier); slippage is never validated despite `README.md:55` claiming it is;
and `min_profit` handed to the on-chain gate is computed without the swap leg, so
a transaction can pass simulation and revert on-chain, burning full gas.

Worse, two supporting files are actively harmful:
- **`simulator/prewarm.rs` (746 lines)** hardcodes Aave's `_reserves` mapping at
  slot 53 and writes fabricated balances over a live fork that would otherwise
  have fetched real values. It can only do nothing or produce a confidently wrong
  number. **Its own sibling `seeding.rs` condemns exactly this practice in its
  module doc** — the repo contains both the disease and the cure.
- **`simulator/golden.rs`** prints profitability and **asserts nothing**, and
  constructs every candidate with zeroed prices, so it never exercises the
  pricing code it exists to validate. It encodes the claim "we have
  replay-validated the simulator against on-chain reality." Nothing here compares
  a simulated number to a real one.

### 4.3 `Executor.yul` cannot be wrapped — "cannot", not "shouldn't"

Full dispatch table: `execute(bytes)`, `executeOperation`, `owner`, `pool`,
`setPool`, `setWorker`, `isWorker`, `withdraw`, `transferOwnership`, default →
revert. **There is no arbitrary-call primitive.** Every external call is a
hardcoded selector with a fixed argument layout, and the entry point hard-asserts
`calldatasize() == 420` with an exactly-352-byte payload. Any call to
`execute(bytes)` unconditionally initiates an Aave `flashLoanSimple`.

**No control path through this contract can reach Morpho, Compound, or any other
protocol.** Good security design; zero reusability. Any pivot needs a new contract.

Downstream, the hardcoded V2 swap selector `0x38ed1739` with a fixed 2-address
path collapsed the venue set to **one executable pair on Base: WETH→USDC**.
cbBTC — 45% of the month's seized value — appears nowhere in the codebase.
Verified by bytecode probe `[MEASURED]`:

| Router | Chain | Implements `0x38ed1739`? |
|---|---|---|
| sushi-base *(control)* | Base | **yes** |
| Aerodrome | Base | no (Solidly `Route[]`) |
| **camelot-arbitrum** — the only `v2`-tagged Arbitrum venue | Arbitrum | **no** |
| uniswap-v3-arbitrum | Arbitrum | no — the address in config is the V3 **factory** |

**An Arbitrum port would have had zero working swap routes.**

### 4.4 Latent silent failures

- **`execute_live` books a SUCCESS outcome the instant `send_raw_transaction`
  returns a hash and never polls the receipt** (`orchestrator.rs:930-944`), even
  though `RpcSubmitter::poll_receipt` exists and is bypassed. In live mode the
  consecutive-reverts breaker is dead code and PnL is fiction. Meanwhile the
  README's daily checklist asks the operator to confirm "inclusion >85%" — a
  number the system does not export.
- **Historical (fixed):** the simulator ran as `Address::ZERO`; ERC20s reject
  zero-address senders, so every `liquidationCall` reverted and returned $0 profit
  **while logging nothing** — five days of silent zero revenue.
- **Historical (fixed):** gas priced in integer gwei; `div_ceil` rounded Base's
  0.005 gwei to 1 gwei (~200×), creating a phantom $0.70 profit floor.
- **Historical (fixed):** detector flagged HF < 1.05 against Aave's actual cutoff
  of 1.0 — everything in 1.00–1.05 reverted with `HealthFactorNotBelowThreshold()`.

### 4.5 The strategic failure

**The measurement came last.** `docs/flow-measurement-2026-07-25.md` is the newest
artifact in the repo. Before it: 22 documents, five remediation waves, a
proprietary license migration, a release-readiness gap register, a monetization
compliance checklist, a threat model and seven runbooks — all built on an
unmeasured assumption about market size. **It should have been commit #2.**

---

## 5. What worked

Genuinely good, and worth preserving:

1. **`core/src/detector/liquidation.rs`** — correct Aave V3 health-factor math
   including the parts most implementations get wrong (two-tier close factor,
   eMode LT override, isolation-mode debt ceiling, siloed borrowing, `assetUnit`
   decimal normalisation). Protocol-specific but chain-agnostic.
2. **`core/src/simulator/seeding.rs`** — discovers ERC20 storage layouts *by
   probing* rather than hardcoding, memoised per token in a `LayoutCache`. This is
   the most original work in the repo and it is protocol- and chain-agnostic. Its
   module doc contains the project's best engineering sentence: *"A hardcoded slot
   table that drifts would silently seed the wrong storage and produce a
   confidently wrong profit number, which is a worse failure than reverting."*
3. **The pacing/risk mechanism** — Decimal-only money math, crash-safe JSONL
   persistence, cross-process reservation, breaker state machine. Defaults are
   wrong; the machine is right.
4. **Discovery at scale** — Multicall3 batching with concurrent checkpointed
   backfill: 46.75M blocks swept, 0 failed chunks, 214,180 borrowers.
5. **The operational surface** — metrics, recovery, emergency flag, runbooks,
   20 scripts. More mature than the strategy it serves.
6. **The measurement tooling** — and this is the asset that actually paid.
   Three stdlib-only, checkpointed scanners that between them overturned the
   project's core thesis and six of their own intermediate conclusions.

---

## 6. The measurement program

### 6.1 Base Aave V3, trailing 30 days `[MEASURED]`

Blocks 47,819,899–49,115,899 (2026-06-25 → 07-25 UTC).

| Metric | Value |
|---|---|
| LiquidationCall events | 116 |
| Collateral seized | $79,773 |
| Debt repaid | $75,324 |
| **Gross liquidation bonus (whole market)** | **$4,404** |
| Unique liquidators | 22 |
| Top-1 share | **85.4%** |
| Events seizing < $1 | **72 of 116** (p50 event size $0.04) |
| Events ≥ $1k | 14 (all the value) |
| Largest single event | $20,698 seized |

**Verified four ways:** independent recompute from raw logs (identical to the
cent), cross-RPC check on a public endpoint (exact match), oracle-vs-Coinbase
price drift ≤0.016%, and a full incumbent forensic profile.

### 6.2 Cross-venue, same window `[MEASURED]`

Bonus computed **structurally** — `seized × (LIF−1)/LIF` at each market's real
liquidation incentive factor. Using `seized − repaid` at current prices inflated
Base Morpho by **63%**.

| Venue | Events | Seized | Bonus | Liquidators | Real top-1 | Tail beyond rank 8 |
|---|---:|---:|---:|---:|---:|---:|
| Morpho Blue (Ethereum) | 261 | $7.93M | **$281,050** | 78 | 40.5% | ~$503k/yr |
| Aave V3 (Ethereum) | 174 | $3.44M | $295,194 | 27 | 56.1% | ~$21k/yr |
| Morpho Blue (Base) | 317 | $851,775 | $56,062 | 62 | 46.8% | ~$20k/yr |
| Spark (Ethereum) | 48 | $316,042 | $71,942 | 5 | 100% (one event) | ~$0 |
| **Aave V3 (Base)** | **116** | **$79,790** | **$4,404** | **22** | **85.4%** | **~$36/yr** |
| Seamless (Base) | 44 | $146 | $7 | 5 | 69.5% | $0 |

### 6.3 The interest-accrual niche `[MEASURED]`

Every open liquidation bucketed as PRICE-PUSH / ACCRUAL / NO-PUSH-FEED, using a
[block−2, block] window against the relevant Chainlink aggregators.

| Venue | n | Price-push | Accrual | No-push-feed |
|---|---:|---:|---:|---:|
| Ethereum Aave V3 | 174 | 63 / **$154,119** (98.2%) | 110 / $2,861 (1.8%) | 1 / $0 |
| Base Morpho Blue | 120* | 97 / $55,392 (98.8%) | 15 / $380 | 8 / $290 |
| **Ethereum Morpho Blue** | 130* | 2 / $958 (0.3%) | 15 / $11,847 | **113 / $268,245 (95.4%)** |

\* largest-first samples covering 99.4–99.9% of value, rescaled to established totals.

**Not-price-push total: $283,623/30d ≈ $3.45M/yr of structural bonus.**

**The evidence is unusually hard.** The AVLT/USDC market's oracle feed
(`0x28b82a…7520`, RedStone `AVLT_FUNDAMENTAL/USD`) returned a **byte-identical
`latestAnswer` at four blocks spanning the window and emitted zero logs in 30
days.** The collateral price never moved — all 138 liquidations there were caused
by debt-side interest accrual alone. AZND/USDC has its price hardcoded to exactly
1.0. ROY-ST-apyUSD (+3.89%) and ynETHx (+0.26%) rose monotonically, and a rising
collateral cannot trigger a liquidation.

| Accrual market | n | Bonus/30d | Bonus/yr | Liquidators | Price drift 30d |
|---|---:|---:|---:|---:|---|
| AVLT/USDC | 138 | $165,867 | $2,018,043 | 32 | **0.0000% (frozen)** |
| AZND/USDC | 69 | $13,747 | $167,252 | 9 | 0.0000% (hardcoded 1.0) |
| ROY-ST-apyUSD/USDC | 6 | $13,233 | $160,999 | 3 | +3.89% (monotone up) |
| ynETHx/wstETH | 1 | $69,846 | n/a | 1 | +0.26% (monotone up) |

AVLT: `lltv` 0.86 → LIF 1.0438 (bonus = 4.20% of seized); **22 borrowers**, top
borrower liquidated 53×; median bonus per event **$71**, p90 $1,724, max $37,816.

**Sanity check passed:** the pipeline reproduced Base Aave's known split exactly
(84 open events / $420.05 vs $420 established; 32 gated / $79,462 vs $79,451).

**Window sensitivity, stated honestly:** Ethereum Aave's accrual bonus collapses
from $17,647 (same-block) → $2,861 (+2) → $296 (+5) → $285 (+100). The large Aave
events are oracle-driven but land 1–2 blocks *after* the push, not in it.

**This is a polling problem, not a latency problem.** There is no trigger event to
subscribe to — the AVLT feed emitted zero logs. Winning means continuously
evaluating health factors over a small known borrower set. That inverts which
code matters: the swap engine, `Executor.yul` and the mempool watcher are latency
machinery for a race that does not exist here.

**Open nuance worth resolving:** AVLT's apparent top liquidator
(`0x6566…0245`, 46.7%) is **Morpho Bundler3, a public multicall router with ≥12
distinct EOA callers** — not a single competitor. Real concentration in AVLT is
therefore *lower* than 47%, but the number of distinct actors is *higher*.
`[MEASURED]` that it is Bundler3; `[UNMEASURED]` what the true per-entity split is.

### 6.4 Venues measured and closed

- **Compound III `buyCollateral`** — mechanically attractive (capital-gated, not
  latency-gated; median 47 blocks between an absorb and the next buy). **But
  measured empty: 2 buys totalling $1.16 across both cUSDCv3 comets in 30 days,
  zero absorbs on Ethereum, 82% gas burn in the reachable band.** An earlier
  intermediate estimate of $39k/30d was wrong and should not be carried forward.
- **Dutch auctions** (Liquity V2, Sky clip auctions, Ajna, Curve LlamaLend,
  Euler v2, Fluid) — premise falsified. Euler's ~$415k/yr pool is held by 22
  faster bots, 80% in RWA collateral with the same unmeasured exit.
- **Arbitrum Aave V3** — SVR-gated, more contested (the Base incumbent ranks #4
  there at 4.3%), and blocked by the missing router selector. Value not measured:
  Infura caps L2 `eth_getLogs` at 10k blocks and Arbitrum's 30-day window is
  10.37M blocks (~1,040 requests), which exhausted the rate limit. `[UNMEASURED]`

---

## 7. The competitive landscape

### 7.1 The incumbent

`0xd12810b19b596347a3afac206d3ca65d08594b3f` — 85.4% of Base Aave seized value.

- **Genuinely unattributed** `[MEASURED]` — zero hits across four search backends
  and Blockscout name tags on five chains. No Dune dashboard, no ENS, no firm.
- **Funded by a retail Binance withdrawal of 0.0402 ETH (~$65).** The master
  operator EOA has pre-bot personal history (USDT transfers, Uniswap swaps, a
  2023 bridge interaction). No MEV firm reuses a personal wallet as a master key.
- **~39 liquidations/month across all chains.** A side project at the same
  operator scale as this one.
- **Same address on six chains** via `CREATE(deployer, nonce=1)` mirroring
  (verified by recomputing `keccak256(RLP([deployer,1]))`). Only Base and Arbitrum
  are active; **Optimism did zero liquidations in 30 days** despite a 2-year-old
  deployment.
- **Off Base they are ordinary:** #4 on Arbitrum Aave (4.3%), and **completely
  absent from Base Morpho Blue.**

**Their actual edge:** inventory in the three assets that liquidate (USDC $20.8k /
cbBTC $10.9k / WETH $1.1k on-contract — so they need no swap engine), **zero-fee
Morpho flash loans** for the tail (Chimera pays Aave 5bps), a UUPS proxy they
upgrade freely, and membership in the auction.

### 7.2 The auction — decoded from their winning transaction

Transaction `0x2cad356c…d496`, Base block 49,054,512:

- Submission contract `0x583dcFef…9b77` is **canonical FastLane Atlas v1.6.4** —
  confirmed by FastLane's published `atlas-config` for chain 8453 *and* on-chain
  `name()` = "Atlas ETH", `symbol()` = "atlETH", with matching
  `VERIFICATION()`/`SIMULATOR()`.
- Selector `0x4317ca01` = `metacall(UserOperation, SolverOperation[],
  DAppOperation, address)`, verified by recomputing the keccak of the expanded
  tuple signature.
- The UserOperation is an oracle `update(...)`/`forward(...)` against
  **`ChainlinkOevDAppControl`**. `log[0]` is the SVR round update; `log[14]` is
  the liquidation — **same transaction**.
- **Two competing solver bids**: incumbent **0.05837 ETH**, runner-up
  **0.05477 ETH**. Won by 6.6%.
- **First-price settlement**, proved by balance diff across the block: the
  protocol destination gained *exactly* the winner's bid, to the wei. FastLane's
  share is **0**.

**Economic consequence:** the winner's measured ETH residual on that transaction
was **~$0.11** while Atlas paid the bundler ~$0.47. A first-price auction against
a zero-marginal-cost incumbent drives the clearing price to ~100% of the bonus.
**99.5% of Base Aave value went through Atlas; the non-Atlas residual was $17.92
of bonus across 81 transactions.**

*Caveat: the ~$0.11 residual is ETH-denominated; profit retained as token
inventory was not measured. Direction unambiguous, exact residual `[UNMEASURED]`.*

### 7.3 Atlas solver economics — why joining does not help

`[REPORTED]`, from Chainlink SVR searcher documentation:

- Solver onboarding is **permissionless** — no allowlist in
  `ChainlinkOevDAppControl` (49 functions enumerated), relay auth is a self-signed
  EIP-191 timestamp, competitors bond only 0.365 and 1.285 ETH (recoverable float).
- **But there is a reputation gate:** at most **2 searcher bids per transaction**,
  with **one slot permanently reserved for the high-reputation set**. The "two
  competing solvers" seen in every sample is a *consequence of the slot cap*.
- **Parallel auctions:** BTC and ETH feeds spawn two independent auctions per
  opportunity. Bid one and you win ~50% of the time *even holding the best bid*.
- **One SolverOperation per block per EOA** — covering both auctions requires
  multiple separately-bonded EOAs. That is what the incumbent's rotating 6+ signer
  fleet is for. It is auction coverage, not throughput parallelism.
- **"Losing bids cost zero gas" is only half true** — a solver op that executes
  and *reverts* still pays gas to the bundler.
- Chainlink acquired Atlas from FastLane (announced 2026-01-22); **Atlas now
  exclusively serves Chainlink SVR**, so an outsider cannot route arbitrary Aave
  liquidations through it — confirmed on-chain by the single fixed
  `authorizedUserOpSigner`.

### 7.4 Fee-for-service alternatives — all measured dead

| Path | Measured reality |
|---|---|
| **Chainlink Automation** | **$4,147/yr (Ethereum) + $675/yr (Base) — the entire network.** 10 distinct transmitters, a fixed permissioned DON. ~$415/yr and ~$75/yr per operator. *You cannot join, and there is nothing to join.* |
| **Gelato** | `gelato()` resolves to a **contract at nonce 1** on both chains — a single Gelato-controlled module, not an open executor set. Executor fee is 2% of gas = **$0.0016/task**. |
| **OpenZeppelin Defender** | Not a revenue source in any configuration; sign-ups disabled 2025-06-30, full shutdown 2026-07-01. |
| **Sky/Maker `bark()`** | `tip()` = **250 DAI flat** + `chip()` = 0.1% of tab. Fired **once** in 30 days. TAM ~$3.6k/yr. |
| **Liquity V1** | 200 LUSD + 0.5% — genuinely fixed. **Zero** liquidations in 30 days. |
| **Maker MIP63 keeper streams** | The only real retainer that ever existed (1,000 DAI/day each to Gelato and Keep3r) — **being deactivated**. |
| **Risk data to DAOs** | **Chaos Labs exited as Aave's risk manager April 2026** after 3 years of operating losses (wanted $8M, offered $5M). Gauntlet pivoted to asset management. Nansen fell $999→$49/mo; Dune's free tier already publishes Aave health-factor dashboards. |

> **The unifying finding:** every fee-for-service keeper design prices its fee as
> *gas × premium* or a flat stablecoin tip hardcoded years ago. **Gas collapsed to
> ~$0.08/tx.** The "low-variance alternative to competitive MEV" does not exist —
> it is the same pie, denominated in gas, after gas went to zero. Same root cause
> as SVR: the protocol captures the value, the executor gets a reimbursement stub.

**What that leaves:** the genuinely non-competitive revenue available here is *not
on-chain execution at all* — it is selling the ability to **prove things about EVM
state**. Two forms: **audit contests** (PoC-mandatory; the fork-simulation harness
is exactly the edge — but note it is a *proof-of-concept* edge, **not a
bug-finding edge**; it makes a report survive triage, it does not find the
vulnerability) and **borrower-side liquidation protection** (the DeFi Saver model —
the only idea where the existing code *is* the product; the hard problem is
distribution, not technology).

Bug bounty context `[REPORTED]`: Immunefi paid $107.3M for confirmed criticals
2021–2026; median critical $20,000, **median across all confirmed reports
$2,000**; ~200 criticals found *globally* per year; "no fix, no pay"; 21% of
banned accounts are AI-generated spam. Audit contests pay a denser distribution:
ordinary active participants $5–30k/yr, top-100 $30–80k, top-50 $80–200k.

---

## 8. The judge panels

Two adversarial panels reviewed the complete evidence independently.

### 8.1 The Skeptic — *"a completed negative result"*

> **RANK 1 — STOP BUILDING.** Not one row in this matrix clears its bar on a
> MEASURED number. Every row that has a large number has an unmeasured gate in
> front of it, and every row with a clean measurement has a small or zero number.
> **That is not a portfolio to choose from; it is a completed negative result.**

> **RANK 2 — the 1-day deletion**, for repo hygiene and artifact value only.
> Worth ~$0 on its own; do it because the code is currently wrong in ways that
> would corrupt any future decision, not because a strategy needs it.

> **RANK 3 — one day of measurement, zero code, and only this.** Three questions,
> all answerable from chain, all binary. *(a)* Can seized AVLT and AZND be sold or
> redeemed by an arbitrary address? *(b)* Is the RedStone feed a frozen PUSH feed
> or a PULL feed whose price arrives in the liquidator's calldata? If pull, the
> market is not open and this entire branch closes. *(c)* Does the AVLT market
> survive past this window, or is it one borrower being ground down like AZND?
> **If (a) is NO or (b) is PULL, the answer is terminal: rank 2 becomes the final
> state and the project ends there. That outcome should be treated as the base
> case, not the sad case.**

> **RANK 5 and below — CLOSED**, in descending order of how much effort they would
> have consumed: Euler v2 / all dutch auctions; Compound III (measured $0 in 30d);
> Morpho Blue Base (98.8% is the race already lost); Atlas SVR solver (every cell
> unmeasured, access granted by a third party); borrower-side protection (a
> startup, unmeasured as one); audit contests (unmeasured — **and ranking it on
> zero numbers is exactly how the last six assumptions got made**).

> **THE PATTERN:** every venue resolves to GATED, A LATENCY RACE, or DENOMINATED
> IN COLLATERAL NOBODY HAS PROVEN IS SELLABLE. The residual niche the whole
> investigation was hunting exists, is worth roughly $2M/yr gross, and lands
> entirely in category three. **Assumption seven, if it falls, falls there.**

### 8.2 The Builder — *"its value is that it costs three days to find out"*

> **1.** Interest-accrual niche on Ethereum Morpho — **but gated behind a 3-day,
> zero-code kill test.** It is the only row with a measured six-figure structural
> pool that is genuinely not a latency race, and its decisive unknown (collateral
> exit) can be resolved with about 40 RPC calls and no capital.
> **2.** Audit contests — runs in parallel at ~3 hrs/week, zero capital, the only
> line item that can produce a first dollar independent of the on-chain thesis.
> **3.** STOP — the standing default. If the Day-3 or Day-13 gate fails, this is
> what wins, and the plan is designed so failure costs 3 or 13 days rather than 24.

> **HONEST OUTCOME DISTRIBUTION:** ~40% Gate 1 kills it on Day 3 · ~25% Gate 2
> kills it on Day 13 · ~20% it runs and earns a few thousand dollars a year ·
> ~15% it clears $25k/yr. **The expected value of this plan is a few thousand
> dollars a year, not a business. Its actual value is that it costs 3 days to
> find that out.**

**Where they agree:** the 3-day kill test is the correct next action, STOP is the
base case, and the deletion should happen regardless. **Where they differ:** the
Skeptic would not build even if the gate passes without re-scoring reuse first.

---

## 9. Corrections log

Every claim this investigation made and then overturned. **This is the most
important section for calibration** — the error rate on first-pass conclusions was
roughly 50%, and every correction came from re-deriving against a protocol
invariant or reading chain state, never from more reading.

| # | First claim | Corrected to | How it was caught |
|---|---|---|---|
| 1 | Base Morpho bonus $137,587 | **$50,501** (oracle-anchored) | 16.2% bonus/seized exceeds Morpho's 13.04% structural ceiling; re-derived against per-market LIF and confirmed by reading each market's oracle at each event's block (invariant held on **320/322 events**) |
| 2 | Bad debt contaminates the Morpho figure | **False** — 4 events, $0.00 | Direct decode of `badDebtAssets` |
| 3 | Ethereum Morpho is fragmented (top-1 39%) | Its rank-2 "liquidator" is **Morpho Bundler3, a public router** with ≥12 EOA callers | Contract inspection |
| 4 | The incumbent nets ~$1,200/month | **~0% residual** — first-price auction bids ~100% of bonus back | Wei-exact balance diff across the block |
| 5 | The incumbent has no structural edge | **Three edges**: reserved high-rep slot, parallel auctions, one solver-op per block per EOA | Chainlink SVR searcher docs |
| 6 | Losing Atlas bids cost zero gas | Only half true — **reverting solver ops pay gas** | FastLane repo README |
| 7 | Ethereum gas is 100–1000× Base | **19.5× per unit, 3.7× per liquidation tx** ($0.083 median) | Direct fee measurement over 426 blocks |
| 8 | Ethereum Aave is open (0% Atlas-routed) | **CLOSED** — ETH/USD is a `DualAggregator`; Ethereum SVR routes via MEV-Share, so the Atlas test cannot see it | Oracle gate |
| 9 | Compound III ≈ $39k/30d, worth a 6-day probe | **$0** — 2 buys, $1.16 total | Direct event scan |
| 10 | Dutch auctions reward capital over latency | **Falsified** — with flash loans a descending auction *is* a latency race | Participant analysis |
| 11 | The pacing caps are a 1-day YAML deletion | **A code change** — `Config::validate()` hard-rejects the values; plus a **fourth** unnamed killer (`max_daily_net_usd`) | Source read |
| 12 | The simulation harness is an audit-contest edge | It is a **proof-of-concept** edge, not a **bug-finding** edge | Payout-structure analysis |
| 13 | Four agents died from unbounded scope | **Transient network outage** (`ENOTFOUND`) | Workflow failure log |

**Also corrected mid-investigation by external verification:** the SVR fallback is
**30 seconds, not 60** (the 60 came from a governance *proposal*, not deployed
state), and the widely-repeated "89% / 51.5% / 11% residual" recapture split
**has no traceable source** and is contradicted by direct measurement.

---

## 10. Decision matrix

| Option | Open? | Size /30d | New-entrant /yr | Capital | Repo reuse | Effort | Killer risk | Verdict |
|---|---|---:|---:|---|---:|---|---|---|
| **Interest-accrual — ETH Morpho** | **OPEN, not a race** | **$283.6k** | **$50–250k** *(inferred)* | Low | see §12 | 3-day test, then ~24d | **Can seized AVLT be sold? `[UNMEASURED]`** | **Only live candidate** |
| Audit contests (PoC-mandatory) | n/a | — | $5–30k ordinary | **None** | 30% code / ~90% artifacts | ~3 hrs/wk | Unmeasured; PoC ≠ bug-finding edge | **Run in parallel** |
| Morpho Blue — Ethereum (latency framing) | OPEN | $281k | ~$503k tail | Low | 62% | 20–22d | 98.8% is a race | Subsumed by row 1 |
| Morpho Blue — Base | OPEN | $56k | **$0–2k** | Low | 62% | 8–10d | Race already lost | Cheapest test only |
| Compound III `buyCollateral` | open post-absorb | **$0** | ~$0 | — | 45% | — | **Measured empty** | **Closed** |
| Dutch auctions | open | ~$415k/yr pool | ~$0 | med–high | 50% | 8–12wk | Premise falsified | **Closed** |
| Aave V3 — Base *(current)* / Ethereum | **CLOSED** | $4.4k / $295k | ~$36 / ~$21k | — | 100% / 70% | n/a | SVR sealed auction | **Exit** |
| Atlas SVR solver | gated entry | — | ~0% residual | 0.1–1.5 ETH | 35% | 2–4wk | Reputation slot; ~50% win | **Closed** |
| Keeper networks | n/a | **$4,147+$675/yr total** | ~$415 / ~$75 per operator | — | — | — | Nothing to join | **Dead** |
| Protocol fixed-fee roles | n/a | Sky 250 DAI flat | ~$3.6k/yr TAM | — | — | — | Fires once/never | **Dead** |
| Risk data to DAOs | n/a | — | — | — | — | months | Leader exited at a loss | **Dead** |
| Borrower-side protection | n/a | `[UNMEASURED]` | `[UNMEASURED]` | None | code **is** the product | wks–months | It is a startup | Parked |
| **STOP** | — | — | — | none | — | 0 | Opportunity cost | **The base case** |

---

## 11. Recommended plan

### Phase 0 — the kill test (Days 1–3). Zero code, zero capital, ~40 RPC calls.

**(a) Exit-route census for AVLT and AZND.** Read the token contracts for a
transfer allowlist, pause flag or transfer hook. Enumerate Uniswap v2/v3/v4,
Curve and Balancer pools against USDC and WETH; read reserves; compute routable
depth at 1%, 2% and 5% slippage against the median seized size (~$1,690) and p90
(~$41,000). *If AVLT cannot be received by an arbitrary address, the thesis dies
in one `eth_call`.*

**(b) Trace what incumbents do with seized collateral.** Pull post-liquidation
transfers for `0x6566…0245` (n=35), `0xbd2c…a213` (n=3, $26,277) and
`0xadc3…6770` (n=7, $9,752). Do they sell, redeem through a vault, or **hold**?
*If they hold, this is a directional long on illiquid RWA dressed as arbitrage —
kill it.*

**(c) Resolve RedStone push vs pull.** Inspect Morpho oracle
`0x2c7aa77c…a841` and feed `0x28b82a7f…7520`. Does the oracle read a stored
`latestAnswer`, or does the liquidation path require a signed payload in calldata
(RedStone consumer pattern: `extractTimestampsAndAssertAllAreEqual`, signer
allowlist)? *If liquidator-supplied under an authorised-signer set, the market is
gated, not open.*

**Gate 1:** proceed only if ≥$40k routable at <2% slippage, incumbents
demonstrably sell, and the feed is push. Otherwise **stop — and treat that as the
base case.**

### Phase 1 — repo hygiene (Days 4–6). Regardless of branch.

Remove the four pacing killers (**code change**, not YAML), delete `prewarm.rs`
and `golden.rs`, wire `RpcSubmitter::poll_receipt` into `execute_live`. Extend
`check_venue_open.py` with the push-feed test (30-day log count per aggregator +
two-block drift check) so the tool that *found* the niche becomes the standing
venue gate. Run it across **all** Ethereum Morpho markets and emit
`config/accrual_markets.json` — this also answers whether the four known markets
are the whole niche or a sample of it.

### Phase 2 — the poller (Days 7–13). Read-only, no contract, no capital.

Morpho position reader (`marketParams` + `position(id,user)` + accrued interest)
reusing the Multicall3 batching from commit `30e6996`; borrower discovery from
`Supply`/`Borrow`/`SupplyCollateral` logs reusing the checkpointed backfill from
`7fef2d2` (AVLT has 22 borrowers, AZND has 1 — the value is never missing a new
one, not scale); a crossing detector; then a shadow run.

**Gate 2:** proceed only if detection recall ≥80%, median lead ≥1 block, ≥3
crossings observed. Otherwise stop, having spent 13 days and zero dollars.

### Phase 3–4 — contract and live (Days 14–30). $3,000 capital, none before Day 18.

`MorphoAccrualLiquidator.sol`, **~150 lines** — `liquidate(...)` with an
`onMorphoLiquidate` callback pulling debt from a pre-funded balance (**not a flash
loan** — Morpho liquidation is callback-financed, which is precisely why the
flash-loan half of the old design is unnecessary), a single-hop exit, owner-only
sweep, `rescue()`. **Do not touch `Executor.yul`.** Fork tests replayed at the
exact blocks of five historical AVLT liquidations — the golden-replay this repo
never actually had. **Set `BASE_FORK_URL`, or the fork tests silently no-op.**

Live with hard caps: one liquidation per 6h, max $3,000 repay, abort if simulated
net < $25. Track crossings detected, attempts, wins, and realised net **after exit
slippage and gas**. *The most likely quiet failure is that execution costs eat the
$71 median event entirely and only the p90 tail is profitable.*

### Parallel track (Days 1–30, ~3 hrs/week)

Register for one PoC-mandatory audit contest; submit at least one finding by
Day 30. Zero capital, no shared engineer-hours.

---

## 12. Salvageable assets

**Survives any pivot (~62% for Morpho, measured against 30,138 tracked lines):**
`detector/liquidation.rs` (Aave-specific but chain-agnostic), `simulator/seeding.rs`
+ the REVM harness, `pacing_engine.rs` (mechanism, not defaults),
`snapshot_refresh.rs`, `signer_registry/`, the entire ops surface, and the three
measurement scanners.

**Does not survive:** `Executor.yul` and its 794 lines of contract tests,
`oracle/aave.rs`, `simulator/prewarm.rs` + `golden.rs`, the venue/pair tables,
`config/snapshot.json` (52,912 lines) and its six `.bak-*` copies, the Base
borrower set, and the hardcoded per-chain token constants.

**A distinction worth naming:** Compound III scored 60% reuse "in principle" but
**45% in use** — Comet needs no simulator, no router, no snapshot refresher, no
contract. *A pivot that makes your code redundant is not the same as a pivot that
reuses it.* And the 92% score for the accrual niche is **a warning, not a
recommendation**: it is highest precisely because it asks the least of you, and
the niche is a polling problem while the repo's most expensive code is latency
machinery.

---

## 13. Tooling for continued research

### 13.1 MCP connectors — verified working in this investigation

| Connector | Use | Notes |
|---|---|---|
| **Context7** (`resolve-library-id` → `query-docs`) | Protocol/SDK docs | How Morpho's LIF formula and `ConstantsLib` were confirmed. **Prefer over web search for library docs.** |
| **Tavily** (`search`, `extract`, `crawl`, `map`, `research`) | Web research | Best general research backend here |
| **Jina** (`read_url`, `search_web`) | Page extraction | Works well against Blockscout HTML |
| **Exa** (`web_search_exa`, `web_fetch_exa`) | Semantic search | Good for finding MEV research writeups |
| **GitHub** (`list_issues`, `list_pulls`, `push_file`) | Read protocol repos | Used for `FastLane-Labs/atlas`, `morpho-blue` |
| **Claude Browser / Playwright** | Dune dashboards, explorer UIs | For pages that resist extraction |
| **Desktop Commander** | Local file/process ops | Long-running scans |

**Highest-value connector NOT available: BigQuery.** Google's public blockchain
datasets (`bigquery-public-data.crypto_ethereum.logs`) would remove the RPC
rate-limit ceiling entirely — the constraint that left Arbitrum unmeasured.
**Authorising this should be the first tooling change.**

**Practical RPC notes** `[MEASURED]`: Alchemy keys are app-scoped (the one here
reached only Ethereum + Base). Infura reaches more chains but **caps
`eth_getLogs` at 10,000 blocks on L2 endpoints** — Arbitrum's 30-day window is
10.37M blocks ≈ 1,040 requests. Public RPCs were largely 403-blocked from the
sandbox. **Always parse the provider's stated limit from the error and latch to
it; never rediscover it by halving.**

### 13.2 Skills

| Skill | When |
|---|---|
| `/commit`, `/commit-push-pr` | **Immediately** — 9 untracked paths including all three scanners |
| `revise-claude-md` | Persist the invariants (the SVR gate, the current-price valuation trap, the four pacing killers) so they are never re-derived |
| `/schedule` | A recurring monthly `scan_liquidation_venues.py` run — the landscape moves; SVR is expanding to more chains and Morpho markets open continuously |
| `/code-review ultra` | On `MorphoAccrualLiquidator.sol`. User-triggered and billed; an agent cannot launch it |
| `/security-review` | Before deploying any new contract. Note: this would be the **first** contract in the project slither can actually parse |
| `/simplify` | Quality pass on changed code |
| `tavily:tavily-research`, `exa:agent` | Deep multi-source research rounds |
| `data:analyze`, `dataviz` | If the borrower/flow data becomes a product |
| `mcp-server-dev:build-mcp-server` | If a dedicated chain-data MCP is worth building |

### 13.3 Plugins

- **`pr-review-toolkit`** — especially **`silent-failure-hunter`**. This repo's
  history is a catalogue of silent failures (the `Address::ZERO` simulator, the
  venue-rotation deadlock, the unpolled receipt). This agent is aimed exactly at
  that failure class.
- **`feature-dev`** (`code-explorer`, `code-architect`, `code-reviewer`) — for
  the Morpho build if Gate 1 passes.
- **`hookify`** — encode the hard-won invariants as hooks so regressions are
  blocked mechanically (e.g. refuse a commit that reintroduces a hardcoded
  storage slot, or a fee cap compared against net profit).
- **`engineering`** (`debug`, `architecture`, `incident-response`).

### 13.4 Method notes that generalise

1. **Measure the market before building for it.** One day of stdlib Python
   invalidated ~30k lines of work.
2. **Never value a bonus with current prices over a long window.** Re-derive
   against the protocol's structural incentive factor, or read the oracle at each
   event's block. This single error inflated a headline figure by 63%.
3. **Run the openness gate before any venue work.** Two RPC calls.
4. **Prefer on-chain tests to documentation.** In this investigation, web-sourced
   claims failed on-chain verification **three times out of four**.
5. **Distinguish shared public routers from competitors** when computing
   concentration. Morpho Bundler3 appeared as the top "liquidator" on two venues.
6. **Adversarial verification pays.** Every one of the 13 corrections in §9 came
   from a second agent attacking the first agent's number.

---

## Appendix A — On-chain reference

### Protocols
| Contract | Chain | Address |
|---|---|---|
| Aave V3 Pool | Base | `0xA238Dd80C259a72e81d7e4664a9801593F98d1c5` |
| Aave V3 Pool | Ethereum | `0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2` |
| Aave Oracle | Base | `0x2cc0fc26ed4563a5ce5e8bdcfe1a2878676ae156` |
| Aave Oracle | Ethereum | `0x54586be62e3c3580375ae3723c145253060ca0c2` |
| Spark Pool / Oracle | Ethereum | `0xC13e21B648A5Ee794902342038FF3aDAB66BE987` / `0x8105f69d9c41644c6a0803fda7d03aa70996cfd9` |
| Seamless Pool / Oracle | Base | `0x8F44Fd754285aa6A2b8B9B97739B79746e0475a7` / `0xfdd4e83890bccd1fbf9b10d71a5cc0a738753b01` |
| **Morpho Blue** | Base + Ethereum | `0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb` |

### Auction infrastructure
| Contract | Address |
|---|---|
| Atlas v1.6.4 (Base) | `0x583dcFef0D240DC80753F0F0B26513feE27D9B77` |
| AtlasVerification | `0x6e1886aEca75160BAa5610B7c1D3a895198C61cf` |
| Atlas Sorter / Simulator | `0x2fAbdbc28307d838C623cDb575567B8Cc8898d6F` / `0x3BF81d7D921E7a6A1999ce3dfa3B348c50fE8DFd` |
| ChainlinkOevDAppControl | `0xa5E1a36938769cbd5a26f5e19D8FCB379f597c83` |
| ⚠ Stale v1.1 Atlas (do not use) | `0x3efbaBE0ee916A4677D281c417E895a3e7411Ac2` |

### The incumbent
| Role | Address |
|---|---|
| Solver proxy (6 chains, same address) | `0xd12810b19b596347a3afac206d3ca65d08594b3f` |
| Implementation (Base) | `0x2b88b31f3cd6ed4206b2643dfc6d604b901740a7` |
| Deployer / operator EOA | `0x498470039F170a763552Efb52DEA116c26F1832F` / `0xF16E7c5079480fa8F16C68734484263bc17e8647` |
| Rival solver | `0xa00f5db88ae795121a0ddfa1a8a9704473e994d7` |

### SVR feeds (the closed marker)
| Feed | Chain | Address |
|---|---|---|
| ETH/USD DualAggregator | Base | `0x9da00d23465282005db222a441a663ee7b9dfcc8` |
| BTC/USD DualAggregator | Base | `0x3a932b286715abc4a86a4acaf68a6cdd89e0d446` |
| ETH/USD DualAggregator | Ethereum | `0x5424384b256154046e9667ddfaaa5e550145215e` |

### Accrual-niche markets
| Market | Oracle | Feed |
|---|---|---|
| AVLT/USDC | `0x2c7aa77cc726b35b1f182d90d45292e0be38a841` | `0x28b82a7fee03281dc63f02570560d1f4690b7520` (RedStone AVLT_FUNDAMENTAL/USD, **frozen 30d**) |
| AZND/USDC | `0x270b2bd4cc6d935aa08b70eac518e2907eb5588b` | none — price hardcoded to 1.0 |
| ROY-ST-apyUSD/USDC | `0xfcc6676c57e70daa29478cc6633122d135cd5a6f` | NAV +3.89% monotone |
| ynETHx/wstETH | `0xb1e676190a86da2cb99afd0496538abe1d4c164d` | NAV +0.26% monotone |

### Event topics
| Event | topic0 |
|---|---|
| Aave `LiquidationCall` | `0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286` |
| Morpho `Liquidate` | `0xa4946ede45d0c6f06a0f5ce92c9ad3b4751452d2fe0e25010783bcab57a67e41` |
| Chainlink `AnswerUpdated` | `0x0559884fd3a460db3073b7fc896cc77986f16e378210ded43186175bf646fc5f` |
| Comet `AbsorbCollateral` | `0x9850ab1af75177e4a9201c65a2cf7976d5d28e40ef63494b44366f86b2f9412e` |
| Comet `BuyCollateral` | `0xf891b2a411b0e66a5f0a6ff1368670fefa287a13f541eb633a386a1a9cc7046b` |
| Compound V2 `LiquidateBorrow` | `0x298637f684da70674f26509b10f07ec2fbc77a335ab1e7d6215a4b2484d8bb52` |

### Selectors
| Function | Selector |
|---|---|
| `getSourceOfAsset(address)` | `0x92bf2be0` |
| `typeAndVersion()` | `0x181f5a77` |
| `idToMarketParams(bytes32)` | `0x2c3c9157` |
| Atlas `metacall(...)` | `0x4317ca01` |
| Comet `isLiquidatable(address)` | `0x042e02cf` |
| Comet `quoteCollateral(address,uint256)` | `0x7ac88ed1` |
| `swapExactTokensForTokens(...)` | `0x38ed1739` |
| Aave `flashLoanSimple` / `liquidationCall` | `0x42b0b77c` / `0x00a718a9` |

### Key transactions
| Purpose | Hash |
|---|---|
| Incumbent's winning Atlas bid (decoded in §7.2) | `0x2cad356c3d212532eb54c61da329bd1080ea31494c75da31c450f6af89eed496` |
| Second structurally identical sample | `0xd0c240ab66bcc976528308fec519690b8b5307d39c422b44e3d31be275100f2a` |
| Largest AVLT accrual liquidation ($37,816 bonus) | `0x559725311503bb47662f65936115668e1be51a7781dfb020e0344d5e6c44c36e` |
| Largest Base Morpho event (VVV/FXUSD, $234,680) | `0x06a103f8f4530c51409ddafc7f15a5e7e48cd00bcfd89bb9ef776ffb7f2ee7df` |
| Largest Base Aave event ($20,698) | `0xe76b52bdd6fc5d84204f7d064a94df00d5f883bc6a27d479cc0a3a547f6c8971` |
| Largest Ethereum Aave accrual event | `0x4a0ab31c053fbeefc54f0d61011a6fee61218fd66e00de5f099258123a03e3c6` |

### Formulas
```
Morpho LIF      = min(1.15, 1 / (1 − 0.3 × (1 − lltv)))
Bonus as % of seized = (LIF − 1) / LIF          # ceiling 13.04%, NOT 15%
  lltv 0.860 → LIF 1.0438 → 4.20% of seized
  lltv 0.625 → LIF 1.1268 → 11.25% of seized
Aave bonus       = per-reserve liquidation bonus, typically 5–10% of seized
```

---

## Appendix B — Sources

**Primary (on-chain):** all measurements reproducible from the addresses, topics
and selectors in Appendix A against any archive-capable Base/Ethereum RPC.

**Protocol documentation** `[REPORTED]`:
- `docs.atlasevm.com/atlas/guides/solvers` — *"Solving in the Atlas ecosystem is permissionless"*; atlETH bonding
- `docs.chain.link/data-feeds/svr-feeds/searcher-onboarding-atlas` — reputation two-group system, 2-slot cap, parallel auctions, bonding recommendations
- `docs.chain.link/data-feeds/svr-feeds` — SVR covers Aave/Compound/Venus; Ethereum uses MEV-Share, Base/Arbitrum/BNB use Atlas
- `github.com/FastLane-Labs/atlas` — `SolverBase.sol`, `AtlETH.sol`, *"Solvers do not benefit from 'free reverts'"*
- `github.com/FastLane-Labs/atlas-config` → `configs/chain-configs-multi-version.json` — canonical per-chain addresses (**use the multi-version file; the single-version file is stale**)
- `morpho-blue/src/libraries/ConstantsLib.sol` — `MAX_LIQUIDATION_INCENTIVE_FACTOR = 1.15e18`, `LIQUIDATION_CURSOR = 0.3e18`
- Chainlink acquires Atlas from FastLane, 2026-01-22 (PR Newswire; corroborated by The Block)
- Sky Forum, 2026-05-29 — deactivation of the GELATO and KEEP3R keeper lanes

**Pricing:** DefiLlama `coins.llama.fi/prices/current/{chain}:{address}` (verified
within 0.02% of Coinbase spot at measurement time).

**Repo artifacts produced by this investigation:**
- `scripts/measure_liquidation_flow.py` — single-venue checkpointed flow scanner
- `scripts/scan_liquidation_venues.py` — multi-chain, multi-protocol flow + concentration
- `scripts/check_venue_open.py` — the two-call openness gate
- `docs/flow-measurement-2026-07-25.md` · `docs/venue-landscape-2026-07-25.md` · `docs/pivot-matrix-2026-07-25.md`
- `docs/data/liquidation-flow-2026-07-25/` — raw logs, summary JSON, per-event CSV

---

## Appendix C — Open questions

Ranked by how much they change the decision:

1. **Can seized AVLT/AZND be sold or redeemed by an arbitrary address?** The
   entire remaining thesis rests on this. `[UNMEASURED]`
2. **Is the RedStone AVLT feed push or pull?** If pull, the market is gated and
   the branch closes. `[UNMEASURED]`
3. **Does the AVLT market survive past this window,** or is it one borrower being
   ground down (as AZND clearly is — 69 liquidations, 1 borrower)? `[UNMEASURED]`
4. **What is AVLT's true per-entity concentration** once Morpho Bundler3's ≥12
   callers are attributed individually? `[UNMEASURED]`
5. **Are the four known accrual markets the whole niche or a sample?** Answered by
   running the extended gate across all Ethereum Morpho markets. `[UNMEASURED]`
6. **Arbitrum Aave V3 by USD value** — blocked by RPC rate limits, not by
   difficulty. Event counts known (348/30d, top-1 27.3%). `[UNMEASURED]`
7. **Does token-denominated profit change the incumbent's ~0% ETH residual?**
   `[UNMEASURED]`
