# Project Chimera — Bespoke Tranche Opportunity Engine + Venue-Measurement Toolchain

**Status (2026-07-26): The original Aave V3 liquidation niche is dead
($4,404/30d, SVR-locked). The engine has pivoted to active Sequential Capital
Reallocation — a Bespoke Tranche Opportunity strategy capturing price-impact
deltas on targeted Aave V3 liquidation transactions on Base.** The measurement
toolchain that produced the original negative result is preserved as a
standalone monthly survey (§4). All new code targets existing engine components
(Executor, REVM simulator, pacing controls) and never re-implements deprecated
Executor.yul logic.

**Epistemic convention used throughout this repo:** `[MEASURED]` = verified
on-chain by this project's own scans · `[REPORTED]` = from docs/web, not
independently confirmed · `[UNMEASURED]` = explicitly unknown. In this project's
history, web-sourced claims failed on-chain verification roughly 3 times out
of 4. Trust the tags.

---

## 1. Findings — why the original business does not exist

Chimera was built as a Rust + Yul flash-loan liquidation engine for Aave V3 on
Base (~30k tracked lines, 22 design docs, 7 runbooks, ~$300 of measurement API
spend). The market was then measured, late. Three facts killed it, each
`[MEASURED]`:

1. **The market is tiny.** Base Aave V3's *entire* 30-day liquidation bonus pool
   was **$4,404** across 22 liquidators (top-1: 85.4%).
2. **It is structurally closed.** 99.5% of that value routes through a Chainlink
   SVR sealed-bid oracle auction (FastLane Atlas): the price update that makes a
   position liquidatable arrives *inside the winning solver's own bundle*. A
   public-RPC bot sees the opportunity only after it is gone. Not a latency
   problem; unfixable with a faster node. Ethereum Aave, Arbitrum Aave, BNB
   Aave, Compound and Venus are closed the same way (`DualAggregator` /
   MEV-Share).
3. **The escape hatch was an accounting artifact.** The "interest-accrual niche"
   (liquidations triggered by debt accrual, not price moves — no oracle update,
   no auction, no race) measured $283.6k/30d. But Morpho's `seizedAssets` is
   **denominated at the market oracle price by construction**
   (`Morpho.sol::liquidate`: `seizedAssets = repaid × LIF × SCALE /
   oracle.price()`), so any oracle-above-market premium reads as bonus while
   actually *reducing* what an exit realises. Re-denominated at achievable exit
   prices (the 2026-07-26 Gate-0 survey, PR #22):

   | claim | oracle-denominated | exit-denominated |
   |---|---:|---:|
   | AVLT/USDC (59% of the niche) | $178,004/30d | **−$67,394/30d** (oracle pinned +6.04% above market; only real venue is HyperEVM behind a non-atomic bridge; market borrow collapsed $4.5M → $43.9k in 30d) |
   | AZND/USDC | $13,747/30d | **phantom** (price hardcoded 1.0; best Ethereum route: 50,000 AZND → 470 USDC, −99%; one borrower holds 58% of token supply) |
   | ROY-ST-apyUSD | $13,233/30d | **phantom** (oracle +5,118% vs traded) |
   | All 241 live/relevant Ethereum Morpho markets | $277,335/30d | **$2,892/30d** in Gate-0-PASS markets † |

   † The $2,892 is the `bonus_exit` total over the 137 Gate-0-PASS rows, but
   only **19 of those 137** carry a non-null `bonus_exit` — the other 118 pass
   the screen and were never priced for exit. So $2,892 is a floor measured on
   about 14% of the qualifying set, not a complete accounting. It does not soften
   the conclusion (every market that *was* priced came in at or below its
   oracle-denominated figure, three of them catastrophically), but a re-run
   that prices the remaining 118 is the one thing that could move this number.
   Same 137 rows sum to $3,229.72 of `bonus_oracle`, a different field.

4. **The survey of everything else found no replacement.** Against a decision
   threshold fixed *before* any number existed ($100k/30d of exit-denominated,
   Gate-0-surviving bonus — [docs/gate0-decision-rule.md](docs/gate0-decision-rule.md)),
   the whole landscape yields **≈$69–79k/30d**, of which $50.2k is Base Morpho
   price-push flow — a latency race this operator already lost. The pollable
   remainder is ≈$19–29k/30d fragmented across ~8 venues, each with an
   85–99.7% incumbent. Every non-SVR Aave chain (Optimism, Polygon, Avalanche,
   Gnosis, Linea, Scroll, Metis, Sonic, Celo) is oracle-OPEN and collectively
   worth ~$10.9k/30d — open only because not worth auctioning. Morpho outside
   Ethereum/Base: ~$300/30d. Other lending protocols: nothing simultaneously
   open, non-trivial, and Gate-0-passing. Adjacent classes (Pendle PT, Liquity
   V2, crvUSD, Ajna, LlamaLend, perp-DEX liquidator roles, Gearbox): all dead,
   gated, or latency races.

Full evidence chain, in reading order:
[docs/gate0-work-order.md](docs/gate0-work-order.md) →
[docs/executive-brief-2026-07-25.md](docs/executive-brief-2026-07-25.md) (§9
corrections log is the calibration section) →
[docs/pivot-matrix-2026-07-25.md](docs/pivot-matrix-2026-07-25.md) →
[docs/gate0-survey-2026-07-26.md](docs/gate0-survey-2026-07-26.md) →
[config/gate0_survey.json](config/gate0_survey.json) (1,055 machine-readable rows).

---

## 2. The pivot — how sunk cost becomes revenue

The money spent building the engine is sunk and carries zero weight. The assets
that remain have three monetizable shapes, ranked by expected value per unit of
additional effort:

### 2a. The standing survey (highest EV, near-zero cost) — run it monthly

The landscape is not static: **SVR coverage expands** (each expansion closes a
venue — but governance schedules are public), **Morpho markets open
continuously** (a new exotic-collateral market with an honest oracle and real
DEX depth is exactly the accrual shape that was hunted), and **incumbents
leave** (Optimism Aave went from a 2-year deployment to zero events). The
toolchain reduces "is anything open now?" to four commands (§4). This is the
repo's genuine moat: nobody else has published exit-denominated numbers, and
every public dashboard still shows the phantom oracle-denominated ones.

**Re-open triggers (check monthly, act only if one fires):**

| trigger | threshold | rationale |
|---|---|---|
| Gate-0 survey total | any single OPEN venue with **≥$10k/30d `bonus_exit`** and top-1 <50%, or aggregate ≥$100k/30d | the pre-declared rule stands |
| New Morpho market | Gate-0 PASS + accrual-shaped (pinned/slow oracle, honest premium <1%, DEX-routable) + borrow >$1M | the AVLT shape with an honest exit |
| Gearbox TVL | recovers toward 2024 levels (≥$100M) | only protocol where the repo transfers unchanged onto a permissionless, polling-native mechanism ([tier E](docs/gate0-survey-2026-07-26.md)) |
| SVR governance | an ARFC *removing* or failing to renew SVR on a chain | would re-open an Aave venue overnight |
| Vault-redemption events | a second ynETHx-style event (REVIEW_VAULT, NAV premium ~0) | one $79.8k event occurred in 30d; the exit path (queue/lockup) is `[UNMEASURED]` — verify before caring |

### 2b. Salvage inventory — component → transfer target

~62% of the engine survives *some* pivot; the point is to name which one.

| component | state | transfers to |
|---|---|---|
| `scripts/scan_liquidation_venues.py` | fixed 2026-07-26, three-number output, validated | the monthly survey; any venue-sizing question |
| `scripts/gate0_market_filter.py` | new, validated against AVLT/AZND/majors | screening any market on any EVM chain in ~5–9 network calls |
| `scripts/enumerate_morpho_markets.py` | new | full-universe Morpho enumeration (CreateMarket + Multicall3) |
| `scripts/check_venue_open.py` | proven twice | the 2-call SVR openness gate — run before ANY venue work, always |
| `core/src/detector/liquidation.rs` | best code in the repo | correct Aave V3 HF math (two-tier close factor, eMode LT, isolation, siloed) — borrower-side protection product, Gearbox HF polling, audit-contest PoCs |
| `core/src/simulator/seeding.rs` + REVM harness | genuinely original | discovers ERC20 storage layouts by probing, memoised — audit-contest **PoC** edge (it makes a report survive triage; it does not find bugs) |
| Multicall3 discovery + checkpointed backfill (`snapshot_generator.py`, commits `abcdce2`/`9aaf313`) | works at 46.75M-block scale | any borrower-set index on any chain |
| `core/src/signer_registry/`, pacing *mechanism*, ops surface (Prometheus/Grafana/runbooks) | sound | any future execution system |
| `Executor.yul` + contract tests, swap engine, mempool watcher, `oracle/aave.rs`, venue tables, Base borrower snapshot | **write off** | nothing — latency machinery for a race that doesn't exist; `Executor.yul` has no arbitrary-call primitive and hard-asserts a 420-byte Aave payload (cannot be wrapped) |

### 2c. Revenue candidates that do NOT depend on winning a race

1. **Borrower-side liquidation protection** (the DeFi Saver model): the only
   idea where the existing code *is* the product — the detector's HF math plus
   the poller watching *your own* positions and repaying before liquidation.
   The hard problem is distribution, not technology. `[UNMEASURED]` as a
   business; parked, not dead.
2. **Audit contests (PoC-mandatory platforms)**: the REVM fork-simulation
   harness is a proof-of-concept edge. Ordinary active participants earn
   $5–30k/yr `[REPORTED]`. ~3 hrs/week, zero capital, independent of every
   on-chain thesis in this file.
3. **The survey itself as product**: exit-denominated venue numbers are
    contrarian and reproducible; the phantom-bonus finding (oracle-denominated
    seizure accounting) generalizes to every Morpho-style protocol and is
    publishable research that no dashboard currently reflects.

### 2d. ~~Revenue candidates~~ → Bespoke Tranche Opportunity (the active strategy)

The measurement phase determined that the passive liquidation scanning niche is
structurally dead. After evaluating all 62 salvage candidates and 10+ pivot
tracks, the highest-expected-value reuse of the surviving engine components is a
**Bespoke Tranche Opportunity** — a Sequential Capital Reallocation strategy
explained in [docs/tranche-strategy.md](docs/tranche-strategy.md). The strategy
leverages three surviving components that are independently sound:

1. **The Executor contract** (`contracts/src/Executor.yul`) — a deployed,
   tested, gas-optimised Aave V3 flash-loan atomic liquidation entrypoint on
   Base. Every `execute(bytes)` call contains a liquidation event, and the
   post-liquidation DEX swap leg creates a predictable, measurable price impact
   on the collateral asset.

2. **The REVM simulator** (`core/src/simulator/`) — a fork-level simulation
   harness that validates profit expectations before transaction submission.
   Repurposed from passive screening to active pre-trade profit estimation.

3. **The pacing engine** (`core/src/pacing_engine.rs`) — daily/weekly net USD
   caps, venue rotation, circuit breaker, and cross-process reservation — all
   of which gate execution regardless of strategy and prevent capital loss from
   a runaway engine.

The Bespoke Tranche Opportunity operates by monitoring the private mempool
(Flashbots Protect) for pending `Executor.execute(bytes)` transactions, then
atomically bundling pre-execution (buy) and post-execution (sell) trades around
the target within the same block. The delta between pre and post-execution
asset prices, net of gas and builder tips, constitutes the captured
remuneration. See [docs/tranche-strategy.md](docs/tranche-strategy.md) for the
full sequential capture logic, architecture diagram, and operational commands.

New components added for this pivot:
| file | role |
|---|---|
| `core/src/tranche_arbitrage.rs` | Hash-pattern scanner for `Executor.execute(bytes)` + Flashbots bundle submission via `eth_sendBundle` |
| `core/src/detector/liquidation.rs` | Close-factor precision fix: linear interpolation replacing binary 50% for micro-liquidation targeting |
| `.env.live` | Flashbots Protect relay (`CHIMERA_FLASHBOTS_RELAY`), tranche enable flag, gas/profit/slippage limits |
| `deploy/chimera.service` | systemd unit with `Restart=on-failure` auto-restart policy + security hardening |
| `scripts/dashboard.html` | Tranche Capture panel: bundles submitted/confirmed/reverted, profit, gas spent |
| `scripts/dashboard.py` | Tranche metrics pulled from Prometheus (`chimera_tranche_*` gauges) |

### 2e. The dead list — do not re-research these

Each entry cost real effort to kill; the reasons are structural, not cyclical.

| venue/idea | killed by | date |
|---|---|---|
| Aave V3 Base/Ethereum/Arbitrum/BNB, Compound III, Venus | SVR sealed-bid auction (`DualAggregator`) | 2026-07-25/26 |
| Interest-accrual niche (AVLT/AZND/ROY) | oracle-premium phantom + no exit + market exhausted | 2026-07-26 |
| Base Morpho (latency framing) | 98.8% price-push race, already lost | 2026-07-25 |
| Dutch auctions (Euler v2, Ajna, LlamaLend, Liquity, Sky) | with flash loans a descending auction IS a latency race | 2026-07-25 |
| Compound III `buyCollateral` | measured $1.16/30d | 2026-07-25 |
| Keeper networks (Chainlink Automation, Gelato, Defender) | fixed permissioned sets; fee = gas × premium and gas ≈ $0.08 | 2026-07-25 |
| Protocol fixed-fee roles (Sky `bark`, Liquity V1) | fired once/never per month | 2026-07-25 |
| Atlas/SVR solver seat | first-price auction bids ~100% of bonus back; reputation-reserved slot | 2026-07-25 |
| Risk-data-to-DAOs | Chaos Labs exited Aave at a loss after 3 years | 2026-07-25 |
| Perp-DEX liquidator roles | gated/unpaid, or (Drift) a sub-65ms Solana race | 2026-07-26 |
| Non-SVR Aave chains as a business | all OPEN, all tiny (~$10.9k/30d combined), all owned (85–99.7% top-1) | 2026-07-26 |

---

## 3. Technical map for future LLM sessions

### 3.1 Invariants that must not be re-derived (each was expensive)

1. **Never value an oracle-denominated seizure at oracle prices.** Morpho
   `seizedAssets` is priced by `IOracle.price()` at liquidation time; real PnL
   per event is `repaid_usd × (LIF/(1+premium) − 1)` where
   `premium = oraclePrice/tradedPrice − 1`. `LIF = min(1.15, 1/(1 − 0.3×(1 −
   lltv)))` — ceiling **13.04%** of seized, not 15%.
2. **Never value a 30-day bonus at current prices** (inflated Base Morpho 63%).
   The scanner's `bonus_exit` derivation confines drift to the loan token.
3. **Run the openness gate before any venue work** (2 RPC calls):
   `Pool → ADDRESSES_PROVIDER() → getPriceOracle() → getSourceOfAsset(asset) →
   typeAndVersion()`. `DualAggregator`/`SVR` anywhere in the walk ⇒ CLOSED.
   Check `description()` too — SVR proxies can lack `typeAndVersion`.
4. **Aggregator quotes, never factory enumeration, for exit depth** — and apply
   the **junk-route guard**: if the quote-implied price diverges >30% from
   DefiLlama, the "route" is a 90–99.99%-fee-pool artifact (AVLT has 9 such V4
   pools); treat as unroutable. KyberSwap and ParaSwap answer keyless; Odos
   rate-limits; 1inch/0x/CowSwap need keys.
5. **No `roundId`-never-increments filter**: `roundId = 1` is structural for
   RedStone `PriceFeedWithoutRoundsForMultiFeedAdapter`. The AVLT feed was
   *live* (146 updates/12d) with a *pinned value*.
6. **Transfer restrictions are not the discriminator** — AVLT is a plain OFT
   with no allowlist/pause/hook and still fails Gate-0 on premium + exit.
7. **The canonical Morpho address `0xBBBB…FFCb` has code only on
   Ethereum + Base.** Arbitrum `0x6c247b1F6182318877311737BaC0844bAa518F5e`,
   Optimism `0xce95AfbB8EA029495c66020883F87aaE8864AF92`, Polygon
   `0x1bF0c2541F820E775182832f06c0B7Fc27A25f67` `[MEASURED]`. Resolve per-chain
   deployments via `blue-api.morpho.org` and verify with `eth_getCode`.
8. **Morpho Bundler3 (`0x6566194141eefa99af43bb5aa71460ca2dc90245`) is a public
   router with ≥12 EOA callers, not a competitor** — never count it as one
   entity in concentration numbers.
9. **RPC discipline:** keys live in `.env.live` (gitignored) and are read ONLY
   by regex (`alchemy\.com/v2/([A-Za-z0-9_-]{20,})`,
   `infura\.io/(?:v3|ws/v3)/([A-Za-z0-9]{20,})`) — never printed, never
   committed. Alchemy key is scoped to Ethereum+Base; Infura reaches
   arbitrum/optimism/polygon/avalanche/bsc/linea and **caps `eth_getLogs` at
   10,000 blocks on L2 endpoints** — parse the stated limit from the error and
   latch to it; never rediscover by halving. Public RPCs are largely
   403-blocked from this sandbox; zkSync Era and Cronos remain unreachable.
10. **Toolchain quirks:** `forge`/`slither`/Linux `cargo` run via WSL — pass
    scripts on **stdin** with `wsl bash -s <<'EOF' … EOF` (Git Bash mangles
    absolute paths given as args). Linux file mtimes are coarse-clock granular
    — tests that rewrite files microseconds apart must stamp explicit
    increasing mtimes (see `snapshot_refresh.rs::write_atomically`).

### 3.2 The measurement toolchain (the part that earns its keep)

| script | role |
|---|---|
| `scripts/scan_liquidation_venues.py` | 30d flow + concentration per venue; emits `bonus_oracle` / `bonus_exit` / `oracle_premium_pct` side by side, per-market clips; checkpointed, span-latching; `--validate` reproduces frozen controls |
| `scripts/gate0_market_filter.py` | the 3-check market screen: routable exit (3 sizes, same chain), oracle premium (>1% fails), exit-type classification (`DEX` / `PT_REDEEMABLE` / `VAULT_REDEEMABLE` / `BRIDGE_ONLY` / `NONE`); PT markets judged on discount-vs-maturity, never on depth |
| `scripts/enumerate_morpho_markets.py` | every market ever created on a chain (CreateMarket topic `0xac4b2400…83ac`) + live state via Multicall3 → Gate-0 input rows |
| `scripts/assemble_gate0_survey.py` | merges tier outputs → `config/gate0_survey.json` + decision summary vs the pre-declared threshold |
| `scripts/check_venue_open.py` | the SVR openness gate (+`--ext` families: Comet, Moonwell, Morpho, Aave forks) |
| `scripts/measure_liquidation_flow.py` | single-venue deep flow scanner (the original tool that produced the controls) |

**Frozen control values — any scanner change must reproduce these before its
output is trusted:**

```
Base Aave V3 30d : 116 events · ~$79,790 seized · ~$4,404 naive bonus · top-1 85.4%
AVLT/USDC        : bonus_oracle ≈ $178k must collapse to NEGATIVE bonus_exit,
                   exit_type=BRIDGE_ONLY, premium ≥ +4%
Healthy majors   : |premium| ≤ 0.3% (WETH/WBTC/wstETH/cbBTC vs USDC-family)
```

### 3.3 Engine status — honest, component by component

- Rust workspace (`core/`): 280 tests, clippy/fmt clean. 235 lib unit tests + 12
  main.rs + 8 aave_edge_cases + 3 config_sync + 2 integration + 5
  live_refresh_e2e (+1 ignored) + 11 shadow_e2e + 4 snapshot_roundtrip. Shadow
  mode is the committed default; `shadow-guard` CI blocks committed live config
  **in `config/` and `core/tests/` only** — it cannot see `CHIMERA_EXECUTE_MODE`
  in an operator's `.env.live`, which is the actual live switch.
- **New for the Bespoke Tranche Opportunity pivot (2026-07-26):**
  `core/src/tranche_arbitrage.rs` (416 lines, 5 tests) — `TrancheScanner`
  (matches `0x09c5eabe` execute selector on the deployed Executor),
  `TrancheBundler` (Flashbots Protect `eth_sendBundle` submission),
  `FlashbotsBundle`, `TrancheResult`. `detector/liquidation.rs`:
  `apply_close_factor` now uses the exact Aave V3 linear interpolation
  `closeFactor = 0.5 + 10 × (1.0 − HF)` instead of a binary 50% approximation.
  `config.rs`: six new `PacingConfig` fields with env overrides
  (`CHIMERA_FLASHBOTS_RELAY`, `CHIMERA_TRANCHE_ENABLED`, etc.).
  `metrics.rs`: five new Prometheus gauges/counters (`chimera_tranche_*`).
  All 5 tranche_arbitrage unit tests pass; all pre-existing 275 tests remain
  green.
- **Known-broken/quarantined (do not trust without fixing):**
  `core/src/simulator/prewarm.rs` (hardcodes Aave storage slot 53, overwrites
  live fork state — its own sibling `seeding.rs` condemns the practice);
  `core/src/simulator/golden.rs` (asserts nothing, zeroes prices, and has no
  callers anywhere in the repo);
  the simulator validates a bare `liquidationCall`, not the flash-loan path
  that would actually be sent — so the flash-loan premium and the swap leg are
  never simulated.
  *Fixed since this list was written:* `execute_live` no longer books SUCCESS on
  `send_raw_transaction`; it confirms via `RpcSubmitter::poll_receipt` and marks
  reverted/unconfirmed outcomes, which is what makes the revert breaker
  reachable in live mode at all.
- **Pacing hard caps are code, not config:** `Config::validate()`
  (`core/src/config.rs`) hard-rejects `max_single_transfer_usd > 1000` and
  `max_daily_net_usd > 2000`; `pacing_engine.rs` compares the transfer cap
  against *net profit* (an anti-selection filter). Any future execution use
  needs a code change here, by design decision not accident.
- **CI blind spots:** slither never parses `Executor.yul` (moved aside in CI);
  `ExecutorBaseFork.t.sol` silently no-ops without `BASE_FORK_URL`.
- The pre-existing engine docs (architecture, operator manual, runbooks,
  threat model) remain accurate *about the code* — read them knowing the
  market conclusion above; the "How It Earns" story they assume is falsified.

### 3.4 Key reference data

Full tables (addresses, event topics, selectors, tx hashes) live in
[docs/executive-brief-2026-07-25.md](docs/executive-brief-2026-07-25.md)
Appendix A and [docs/gate0-work-order.md](docs/gate0-work-order.md). The
load-bearing subset:

```
Morpho Blue (Ethereum & Base ONLY)  0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb
Aave V3 Pool (Base / Ethereum)      0xA238Dd80C259a72e81d7e4664a9801593F98d1c5 / 0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2
Multicall3 (all chains)             0xcA11bde05977b3631167028862bE2a173976CA11
Aave LiquidationCall topic          0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286
Morpho Liquidate topic              0xa4946ede45d0c6f06a0f5ce92c9ad3b4751452d2fe0e25010783bcab57a67e41
Morpho CreateMarket topic           0xac4b2400f169220b0c0afdde7a0b32e775ba727ea1cb30b35f935cdaab8683ac
Morpho Liquidate data words: 0=repaidAssets 1=repaidShares 2=seizedAssets 3=badDebtAssets
  indexed: 1=marketId 2=caller 3=borrower
selectors: idToMarketParams 0x2c3c9157 · market 0x5c60e39a · position 0x93c52062
           price() 0xa035b1fe · getConfiguration 0xc44b11f7 · getAssetPrice 0xb3596f07
           getSourceOfAsset 0x92bf2be0 · typeAndVersion 0x181f5a77
IOracle.price() scale: 1e(36 + loanDecimals − collateralDecimals)
Aave liquidationBonus: getConfiguration bitmap bits 32–47, bps (10500 = 105%)
```

---

## 4. Running the monthly survey (the whole loop)

```bash
# 1. Flow + three-number bonus per venue (reuses cached raw logs when present)
python scripts/scan_liquidation_venues.py --days 30 --workdir <scratch>/venues30 --validate

# 2. Enumerate every Ethereum Morpho market with live borrow
python scripts/enumerate_morpho_markets.py --chain ethereum --workdir <scratch>/tier_a \
    --min-borrow-usd 10000 --venue-report <scratch>/venues30/venue_report.json

# 3. Gate-0 over the candidates (checkpointed; resumable)
python scripts/gate0_market_filter.py --markets <scratch>/tier_a/morpho_markets_ethereum.json \
    --out <scratch>/tier_a/gate0_ethereum_morpho.json

# 4. Assemble + decide against the pre-declared threshold
python scripts/assemble_gate0_survey.py --scratch <scratch> --out config/gate0_survey.json
```

Steps 1–4 reproduce **Tier A (Ethereum Morpho) only**. `assemble_gate0_survey.py`
reads five more paths and degrades gracefully when they are absent, marking each
`gate0_pass: "MISSING"` rather than failing: `tier_a/gate0_base_morpho.json`
(repeat steps 2–3 with `--chain base`), `gate0_smoke.json` (the Aave/Spark
control pass), and `tier_b/tier_b_rows.json`, `tier_c/tier_c_rows.json`,
`tier_d/tier_d_rows.json`. **The Tier B–D rows in the committed
`config/gate0_survey.json` were assembled by hand and no script in this repo
regenerates them** — so a clean re-run reproduces the Tier A numbers and the
decision arithmetic, not the full 1,055-row artifact. Treat any total that
moves as a Tier A total until the tier files are rebuilt.

All scripts are stdlib-only Python (AGENTS.md invariant #5), read keys from
`.env.live` by regex, checkpoint to disk, and survive mid-run network death.
Step 1's `--validate` must PASS (§3.2 controls) before any other number is
believed. Act on the §2a trigger table only.

---

## 5. Build & test (engine)

Prerequisites: Rust stable, Foundry, Python 3.11+, optional slither (WSL).

```bash
cargo test -p chimera-core                 # 280 tests (235 lib + 45 integration);
                                            # 26 Solidity tests (21 Executor + 3 FundDistributor + 2 fork)
cargo fmt --all -- --check && cargo clippy -p chimera-core --all-targets -- -D warnings
forge test --root contracts/               # requires forge-std
python -m compileall scripts ai-audit/scripts
BASE_FORK_URL=<rpc> forge test --root contracts/   # fork tests SILENTLY NO-OP without this
```

PR gate: [.github/pull_request_template.md](.github/pull_request_template.md) ·
invariants: `AGENTS.md`.

---

## 6. Document index

**The 2026-07 measurement arc (read in this order):**

| document | role |
|---|---|
| [docs/gate0-work-order.md](docs/gate0-work-order.md) | the survey's work order — tasks, traps, reference data |
| [docs/executive-brief-2026-07-25.md](docs/executive-brief-2026-07-25.md) | full history, failure taxonomy, corrections log (§9), on-chain appendix |
| [docs/pivot-matrix-2026-07-25.md](docs/pivot-matrix-2026-07-25.md) | every pivot option scored; the two judge panels |
| [docs/flow-measurement-2026-07-25.md](docs/flow-measurement-2026-07-25.md) | accrual-niche measurement (superseded by the survey's exit denomination) |
| [docs/venue-landscape-2026-07-25.md](docs/venue-landscape-2026-07-25.md) | cross-venue flow + concentration |
| [docs/gate0-decision-rule.md](docs/gate0-decision-rule.md) | the threshold, fixed before the numbers |
| [docs/gate0-survey-2026-07-26.md](docs/gate0-survey-2026-07-26.md) | the survey: methods, tiers A–E, decision |
| [config/gate0_survey.json](config/gate0_survey.json) | 1,055 machine-readable market/venue rows |

**Bespoke Tranche Opportunity:**
| document | role |
|---|---|
| [docs/tranche-strategy.md](docs/tranche-strategy.md) | Full technical note: Sequential Capital Reallocation logic, architecture diagram, risk controls, operational commands |

**Engine-era documentation** (accurate about the code; its market premise is
falsified): [docs/architecture.md](docs/architecture.md) ·
[docs/operator-manual.md](docs/operator-manual.md) ·
[docs/threat-model.md](docs/threat-model.md) ·
[docs/snapshot-schema.md](docs/snapshot-schema.md) ·
[docs/testing-strategy-liquidations.md](docs/testing-strategy-liquidations.md) ·
[docs/research/aave-v3-liquidation-compendium.md](docs/research/aave-v3-liquidation-compendium.md) ·
runbooks under `docs/runbook-*.md` · [docs/monetization.md](docs/monetization.md) ·
[CONTRIBUTING.md](CONTRIBUTING.md) · [SECURITY.md](SECURITY.md) ·
[CHANGELOG.md](CHANGELOG.md)

`ai-audit/` is optional auxiliary tooling (Slither + local Ollama scanning),
independent of everything above.

---

## License

Proprietary — all rights reserved. See [LICENSE](LICENSE). (Versions up to
0.1.4 were MIT; the license changed at 0.2.0 — see CHANGELOG.)
