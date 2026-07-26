# Work Order — Gate-0 Feasibility Pass (v2, broad-survey)

Paste everything after `## THE PROMPT` as a single new message in a fresh session.
Self-contained; assumes no prior context.

**What changed from v1:** the point of making Gate-0 cheap is so you can afford to
point it at *everything*. v1 pointed it at four markets. v2 adds (a) aggregator
quotes instead of hand-enumerated factories as the depth test, (b) a third Gate-0
check that classifies the *type* of exit — because PT and RWA collateral redeem
rather than trade, (c) a broad venue sweep including **Aave on the chains SVR does
not cover**, and (d) explicit tool wiring.

---

## THE PROMPT

You are picking up Project Chimera at `C:\Users\cld-main\Desktop\github-projects\project-chimera`
(Rust + Yul liquidation engine, originally Aave V3 on Base). Read
`docs/executive-brief-2026-07-25.md` and `docs/pivot-matrix-2026-07-25.md` first —
they carry the full history. This message defines a **bounded, ~1.5-day survey
pass**. Do not start any build. Recommend; do not implement a strategy.

### Background you must not re-derive

- **Base Aave V3 is auction-closed.** 99.5% of its liquidation value routes
  through a Chainlink SVR sealed-bid oracle auction via FastLane Atlas. Its entire
  30-day bonus pool was $4,404. Ethereum Aave is closed too (ETH/USD resolves to a
  `DualAggregator`). **Morpho Blue is structurally open** — SVR covers
  Aave/Compound/Venus, and Morpho carries per-market oracles.
- **The interest-accrual niche** (liquidations triggered by debt accrual, not a
  price move — no oracle update, no auction, no race) measured **$283.6k/30d**,
  96% on Ethereum Morpho Blue, **59% in one market (AVLT/USDC)**.
- **AVLT was killed on 2026-07-26 by an oracle-premium finding.** The RedStone
  `AVLT_FUNDAMENTAL/USD` feed reads **$1.09449**; AVLT trades at
  **$1.0294–$1.0512**. The oracle sits **+4.18% above market** while Morpho's LIF
  at `lltv` 0.86 is **+4.38%**. **They cancel.** Round-trip per $1 repaid at zero
  slippage: +0.25% best pool, **−0.83%** deepest. Median clip nets ~+$2 before gas
  and bridge; p90 nets **−$911**.
- **Therefore the $165,867/30d attributed to AVLT — and by extension the
  $283.6k/30d niche total — is a mark-to-oracle artifact.** Morpho's
  `seizedAssets` is oracle-denominated by construction, so any oracle-vs-market
  spread becomes phantom bonus. **That is the defect you fix in Task 1.**

### Task 1 — Fix the scanner so its numbers mean something (half day, highest value)

`scripts/scan_liquidation_venues.py` derives bonus from `seized_usd − repaid_usd`
at **current** DefiLlama prices. Two separate errors live there: current-price
valuation over a 30-day window inflated Base Morpho by **63%**, and for Morpho the
seizure is oracle-denominated so an oracle premium reads as profit.

1. **Verify the denomination assumption first** — load-bearing, ten minutes.
   Confirm from `morpho-blue` source that `Liquidate.seizedAssets` is computed at
   the market oracle price. Use `mcp__context7__resolve-library-id` →
   `query-docs` for "morpho-blue", or read the GitHub source. Report before
   changing anything.
2. Emit **three numbers side by side per venue**, never one:
   - `bonus_oracle` = `seized × (LIF − 1) / LIF`, with
     `LIF = min(1.15, 1 / (1 − 0.3 × (1 − lltv)))`. Ceiling is **13.04% of
     seized, not 15%**.
   - `bonus_exit` = the same seizure re-denominated at an **achievable exit
     price** from Task 2's quote probe.
   - `oracle_premium_pct` = `oraclePrice / tradedPrice − 1`.
3. Keep existing checkpointing, provider-span latching and key redaction.
4. **Validation gate — do not proceed until this passes.** Reproduce the Base
   Aave control exactly: **116 events, $79,790 seized, $4,404 bonus, top-1
   85.4%**. Then confirm `bonus_exit` collapses toward zero on AVLT/USDC where
   `bonus_oracle` says $165,867. If the control does not reproduce, your change is
   wrong.

### Task 2 — Gate-0: three cheap checks, designed to be run across hundreds of markets

Extend `scripts/check_venue_open.py` (which already implements the SVR openness
gate) or add `scripts/gate0_market_filter.py`. **Target: ≤5 network calls per
market.** Cheapness is the whole point — it is what lets Task 3 be broad.

**Check (i) — ROUTABLE EXIT, measured with aggregators, not factories.**
Do **not** hand-enumerate DEX factories. Aggregators already route across every
venue and return the true realisable output for a given input size. Query one or
more of these public quote endpoints for `collateral → debtAsset` at three sizes
(median clip, 3× median, p90 clip) and record output and implied price impact:
- 1inch (`api.1inch.dev` / public v6 quote), 0x Swap API (`api.0x.org/swap/...`),
  Odos (`api.odos.xyz/sor/quote/v2`), KyberSwap
  (`aggregator-api.kyberswap.com/{chain}/api/v1/routes`), ParaSwap
  (`api.paraswap.io/prices`), CowSwap (`api.cow.fi/mainnet/api/v1/quote`),
  LiFi (`li.quest/v1/quote`) for cross-chain.
- Use whichever respond without a key; note which required one.
- **Fail check (i) if <3× the intended clip is routable at <2% price impact,
  on the same chain as the debt asset.** Same-chain matters: AVLT's only real
  venue is HyperEVM behind a non-atomic LayerZero bridge, which does not count.
- If every aggregator refuses to quote the pair, that is itself a fail — record
  it as `NO_ROUTE` rather than an error.
- Only fall back to reading pools directly when aggregators are unavailable.
  Factory addresses are in the reference section for that case.

**Check (ii) — ORACLE PREMIUM.** Fail if `|oraclePrice / tradedPrice − 1| > 1%`.
`tradedPrice` comes from the aggregator quote in check (i) (or DefiLlama /
GeckoTerminal as a cross-check). **This is the check that actually kills things —
it, not transfer restrictions, is the discriminator.**

**Check (iii) — CLASSIFY THE EXIT TYPE (new, and it prevents a false negative).**
Not all collateral exits via a DEX. Classify each market's collateral as one of:
- `DEX` — normal token with routable depth. Judge on check (i).
- `PT_REDEEMABLE` — a Pendle Principal Token. **These appeared in the accrual
  data** (`PT-apyUSD-5NOV2026/USDC`, `PT-stcUSD-29JAN2026/USDC`). A PT converges
  deterministically to par at maturity and can be redeemed for the underlying —
  **so thin DEX depth is not disqualifying.** For these, record instead:
  time-to-maturity, current discount to par, the redemption function and whether
  it is permissionless. This is a *time-based* opportunity, which suits a polling
  engine far better than a latency race. Do not fail a PT market on check (i)
  alone; judge it on discount-vs-time-to-maturity.
- `VAULT_REDEEMABLE` — ERC-4626 or similar with a `redeem`/`withdraw` path. Record
  whether redemption is permissionless, any queue or lockup, and the NAV-vs-market
  spread.
- `BRIDGE_ONLY` — the only venue is on another chain (AVLT's case). **Automatic
  fail**: non-atomic, warehouses price risk.
- `NONE` — no exit found. Automatic fail.

**Traps, all learned the hard way:**
- **Uniswap V4 pool existence is not liquidity.** AVLT has 9 V4 pools with fee
  tiers of **90%–99.99%** (V4 fee is 1e-6 units). Dumping 1M AVLT into every
  Ethereum venue yields **$2,046**. This is exactly why aggregator quotes replace
  factory enumeration — an aggregator will not route through a 99% fee pool.
- **Do NOT build a `roundId`-never-increments filter.** `roundId = 1` is
  structural for `PriceFeedWithoutRoundsForMultiFeedAdapter` and would blacklist
  every healthy RedStone market. The AVLT feed is **not frozen** — 146 updates in
  12 days on a 2h heartbeat; the *value* is pinned, the feed is live, reads are
  ungated (PUSH, not PULL).
- Transfer-restriction checks are **not** the discriminator. AVLT is a plain
  verified LayerZero OFT with no allowlist, pause or hook. Keep a cheap
  transferability simulation, but do not treat it as the gate.

### Task 3 — Point Gate-0 at everything (the survey)

Because Gate-0 is ≤5 calls, breadth is affordable. Work outward in this order and
**stop early if a tier yields nothing** — report what you skipped and why.

**Tier A — finish the accrual thesis (highest priority).**
- **AZND** (`0x52c66b5e7f8fde20843de900c5c8b4b0f23708a0`, market
  `0xfd0d72a4f0469598b566b1bc5fe64835f828f90b1fb7d746148c086164cd4cc2`, price
  hardcoded to 1.0, 69 liquidations of a **single borrower**). Never measured — do
  not assume it inherits AVLT's shape; assuming that is exactly the error that
  produced the failed pivot. Note that a hardcoded price *cannot* drift from
  market, so it may pass check (ii) trivially while failing on substance.
- **Every** Ethereum Morpho Blue market, not just the top 130 by bonus that
  earlier sampling covered. Enumerate via `CreateMarket` logs on Morpho Blue.
- **The PT markets specifically**, under check (iii).

**Tier B — Aave V3 on the chains SVR does NOT cover (largest unexplored surface).**
SVR is deployed on Ethereum, Base, Arbitrum and BNB. Aave V3 also runs on
**Gnosis, Scroll, Metis, Optimism, Polygon, Avalanche, Sonic, Linea, zkSync Era,
Celo**. Prior work only tested Base and Ethereum. Run the existing openness gate
(`getSourceOfAsset` → `typeAndVersion`) plus Gate-0 on each reachable one. A
`DualAggregator` means closed; anything else may still be open. Aave V3 Pool is
`0x794a61358D6845594F94dc1DB02A252b5b4814aD` on most L2s — verify with
`eth_getCode` before trusting it. Note the Aave ARFC roadmap lists several of
these as *pending* SVR deployments, so anything open here has a shelf life —
record that as a risk, not a disqualifier.

**Tier C — Morpho Blue on other chains.** Same contract address
(`0xBBBB…FFCb`) on Base (done) and other deployments — Arbitrum, Polygon,
Unichain, Optimism, Katana. Structurally open by the same argument.

**Tier D — other lending protocols, openness gate + Gate-0 only.**
Euler v2, Fluid, Silo V2, Spark (done: open), Seamless (done: open), Zerolend,
Radiant, Moonwell, Venus, Compound V2 forks, Term Finance, Notional. For each:
does it have a liquidation event with a bonus, is its oracle SVR-wrapped, and does
Gate-0 pass. **Do not deep-dive any of them** — one pass, pass/fail, move on.

**Tier E — adjacent instrument classes, desk research only (max 2 hours).**
Identify, do not evaluate in depth, whether any of these reward *polling and
capital* rather than latency, and whether the repo's health-factor math / REVM
harness / Multicall3 discovery transfer: Pendle PT maturity convergence; Liquity
V2 and crvUSD redemption arbitrage; Ajna and LlamaLend auctions; perp-DEX
liquidator roles (GMX, Hyperliquid, Gains, Vertex, Drift); Gearbox credit-account
liquidations. Output a one-line verdict each with a size signal. Flag anything
worth its own work order.

Emit `config/gate0_survey.json`: one row per market/venue with chain, protocol,
market id, collateral, debt asset, `open_or_gated`, `exit_type`,
`routable_at_2pct`, `oracle_premium_pct`, `bonus_oracle`, `bonus_exit`,
`gate0_pass`, and a `notes` field.

### Task 4 — Decide against a threshold fixed now, before you see the answer

Write the threshold into your output **before** reporting any number.

- **If ≥ $100k/30d of `bonus_exit` survives Gate-0 across all tiers** → report it,
  name the venues, recommend re-opening with a fresh brief. Do not build.
- **If not** → state plainly that the accrual niche and the liquidation category
  are dead for this operator, and that it cost ~1.5 days rather than a quarter.
- **Report Tier E separately** — it may surface something that is not a
  liquidation strategy at all, and that should not be scored against this
  threshold.

### Tooling — load these; do not work unarmed

**Batch ONE `ToolSearch` call at the start.** Suggested:
```
select:WebSearch,WebFetch,mcp__context7__resolve-library-id,mcp__context7__query-docs,
mcp__eaa1cc63-4561-4583-9d52-ee8350f26880__tavily_search,
mcp__eaa1cc63-4561-4583-9d52-ee8350f26880__tavily_extract,
mcp__jina__read_url,mcp__fc0d10a7-5e8b-4e94-8d15-d3389ea8801f__web_search_exa
```
- **Context7** — the preferred source for any protocol/SDK question (Morpho,
  Pendle, Euler, Aave, LayerZero). Use it before web search; it is how Morpho's
  LIF formula and `ConstantsLib` constants were confirmed.
- **Tavily / Jina / Exa** — cross-check web claims across at least two backends.
  In this investigation web-sourced claims failed on-chain verification **three
  times out of four**, so treat all of them as leads, not evidence.
- **GitHub connector** (`mcp__aaf197f5-1614-4868-8858-8a8c78fd03bb__*`) — read
  protocol sources directly (`morpho-org/morpho-blue`, `pendle-finance`,
  `euler-xyz`, `FastLane-Labs/atlas`).
- **Claude Browser / Playwright** — only for pages that resist extraction (Dune
  dashboards, DefiLlama, GeckoTerminal, `app.morpho.org`).
- **`mcp__Desktop_Commander__start_process`** — for scans that outlive a single
  tool call; checkpoint to disk so partial work survives.
- **Try BigQuery.** If a BigQuery connector is authorised in this session, the
  public `crypto_ethereum` dataset removes the RPC rate-limit ceiling entirely and
  makes Tier B/C trivially exhaustive. It was unavailable previously — check
  first, and say so if still absent.

**Subagents — fan out, do not serialise.** Use the `Explore` agent for read-only
breadth (finding protocol addresses, enumerating deployments) and
`general-purpose` for per-tier measurement. Tier B, C, D and E are independent —
run them concurrently. **Cap each at ~400 network calls and require it to
checkpoint to disk**; four agents previously died mid-run from a transient network
error and lost everything unsaved.

**Skills worth invoking:** `tavily:tavily-research` for Tier E desk research;
`data:analyze` / `dataviz` if the survey table wants summarising.

**Report to the operator, do not run yourself:** `/commit` (nothing from the prior
investigation is committed), `/code-review ultra` and `/security-review` (both
user-triggered and billed — mention them only if a build is later approved),
`/schedule` (a recurring monthly re-run of this survey is genuinely valuable
because SVR is expanding and Morpho markets open continuously), and
`/revise-claude-md` to persist whatever invariants you establish.

### Hard constraints

- **Do not touch `contracts/src/Executor.yul`, the swap engine, or the mempool
  watcher.** They are the repo's most expensive components and they are latency
  machinery, useless in a polling game. The failed pivot proposed reusing all
  three, which is what revealed it had been reverse-engineered to justify existing
  code rather than derived from the opportunity.
- **Write no new Solidity. Deploy nothing. Commit no capital.**
- **Never print an API key.** Read them from `.env.live` by regex.
- Mark every claim `[MEASURED]`, `[REPORTED]` or `[UNMEASURED]`. If something
  cannot be measured inside budget, say so rather than estimating.
- Prefer on-chain measurement to documentation, always.

### Reference data

**RPC** (keys in `.env.live`; Alchemy is scoped to Ethereum + Base only):
```
Ethereum  https://eth-mainnet.g.alchemy.com/v2/<alchemy_key>
Base      https://base-mainnet.g.alchemy.com/v2/<alchemy_key>
Others    https://<net>.infura.io/v3/<infura_key>   (arbitrum-mainnet, optimism-mainnet,
          polygon-mainnet, avalanche-mainnet, bsc-mainnet, linea-mainnet)
HyperEVM  https://rpc.hyperliquid.xyz/evm           (no key)
```
Infura **caps `eth_getLogs` at 10,000 blocks on L2 endpoints** and rate-limits
hard. Parse the stated limit from the error and latch to it; never rediscover it by
halving. Public RPCs were largely 403-blocked from this sandbox.

**Protocols**
```
Morpho Blue (all chains, same address) 0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb
Aave V3 Pool (Base)                    0xA238Dd80C259a72e81d7e4664a9801593F98d1c5
Aave V3 Pool (Ethereum)                0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2
Aave V3 Pool (most L2s — verify!)      0x794a61358D6845594F94dc1DB02A252b5b4814aD
Spark (Ethereum)                       0xC13e21B648A5Ee794902342038FF3aDAB66BE987
Seamless (Base)                        0x8F44Fd754285aa6A2b8B9B97739B79746e0475a7
```

**Known accrual markets (Ethereum Morpho)**
```
AVLT/USDC   market 0x49b89ee666acc242de6a2f25ffdbb25dcc693a7f4746d8d4afeb51c440fa938a
            collateral 0x74db7a52773a52699dbc0c01b1254e5301e3e119  lltv 0.860
            oracle 0x2c7aa77cc726b35b1f182d90d45292e0be38a841
            feed   0x28b82a7fee03281dc63f02570560d1f4690b7520
AZND/USDC   market 0xfd0d72a4f0469598b566b1bc5fe64835f828f90b1fb7d746148c086164cd4cc2
            collateral 0x52c66b5e7f8fde20843de900c5c8b4b0f23708a0  lltv 0.860
ynETHx/WETH market 0xf0edbb36183591ff28c56fdb283fdd6896cf1298990e5913208902adb87d2b75
rswETH/msETH market 0x0d55c325847ed87d53506c2aca7de046cb59d8c22928fd55fb2790c4811d20db
USDC = 0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48
```

**Selectors**
```
getSourceOfAsset(address) 0x92bf2be0   typeAndVersion()          0x181f5a77
idToMarketParams(bytes32) 0x2c3c9157   price()                   0xa035b1fe
market(bytes32)           0x5c60e39a   position(bytes32,address) 0x93c52062
decimals()  0x313ce567  symbol() 0x95d89b41  balanceOf(address) 0x70a08231
totalSupply() 0x18160ddd  UniV2 getPair 0xe6a43905  UniV3 getPool 0x1698ee82
```
Verify any uncertain selector with `cast sig` / `cast sig-event` via WSL —
`wsl bash -s <<'EOF' … EOF`, passing the script on **stdin**, not as an argument
(Git Bash mangles absolute paths given as args).

**DEX factories (Ethereum) — fallback only, if aggregators are unavailable**
```
Uniswap V2  0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f
Uniswap V3  0x1F98431c8aD98523631AE4a59f267346ea31F984  fees 100/500/3000/10000
Uniswap V4  PoolManager 0x000000000004444c5dc75cB358380D2e3dE08A90
            V4Quoter    0x52f0e24d1c21c8a0cb1e5a5dd6198556bd9e1203
Curve metaregistry 0xf98b45fa17de75fb1ad0e7afd971b0ca00e379fc
Balancer V2 0xBA12222222228d8Ba445958a75a0704d566BF2C8
Balancer V3 0xbA1333333333a1BA1108E8412f11850A5C319bA9
```

**Event topics**
```
Aave LiquidationCall    0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286
Morpho Liquidate        0xa4946ede45d0c6f06a0f5ce92c9ad3b4751452d2fe0e25010783bcab57a67e41
Chainlink AnswerUpdated 0x0559884fd3a460db3073b7fc896cc77986f16e378210ded43186175bf646fc5f
ERC20 Transfer          0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef
Comet AbsorbCollateral  0x9850ab1af75177e4a9201c65a2cf7976d5d28e40ef63494b44366f86b2f9412e
Compound V2 LiquidateBorrow 0x298637f684da70674f26509b10f07ec2fbc77a335ab1e7d6215a4b2484d8bb52
```
Morpho `Liquidate` data words: `0`=repaidAssets `1`=repaidShares `2`=seizedAssets
`3`=badDebtAssets `4`=badDebtShares. Indexed: `1`=marketId `2`=caller `3`=borrower.

**Control values your fixed scanner must reproduce**
```
Base Aave V3 30d : 116 events · $79,790 seized · $4,404 bonus · top-1 85.4%
Base Morpho 30d  : 322 events · $851,775 seized · $50,501 oracle-anchored bonus (6.60% of seized)
AVLT/USDC        : must now FAIL Gate-0 on the oracle-premium check (+4.18%) and on exit_type=BRIDGE_ONLY
```

**Do not treat as a competitor:** `0x6566194141eefa99af43bb5aa71460ca2dc90245` is
**Morpho Bundler3**, a public multicall router with ≥12 distinct EOA callers. It
appears as the top "liquidator" on two venues and is not one entity.

Prior raw logs are on disk at
`C:\Users\cld-main\AppData\Local\Temp\claude\C--Users-cld-main-Desktop-github-projects-project-chimera\4ef27e25-2ce8-4cd9-938a-2bda34ac766a\scratchpad\venues30\`
(`raw_{chain}_{venue}.jsonl`, `venue_report.json`, `gated_vs_open.json`). Reuse
where the window matches rather than re-fetching.

### Deliverable

One report containing: what Task 1 changed and the control-reproduction proof; the
Gate-0 implementation with all three checks; `config/gate0_survey.json` plus a
readable table of every market/venue surveyed across Tiers A–D; the Tier E
one-liners reported separately; and the Task 4 decision against the pre-declared
$100k threshold. Then stop.
