# Project Chimera — archived

**Status: ARCHIVED (2026-07-26). The engine works. The business does not exist.**

Chimera was a Rust + Yul atomic flash-loan execution engine for on-chain lending
liquidations and MEV, built to earn revenue comparable to a prior retail-forex
bot. It reached production quality — 318 passing tests, a deployed Yul contract,
a full risk and pacing layer — and was run live for six days.

**Revenue: $0. Liquidations executed: 0. Direct measurement spend: ~$350.**

Five independent strategy investigations were run against it. All five returned
negative, and all five returned negative *for the same structural reason* (§3).
This repository is preserved as the evidence chain for that conclusion and as a
reusable measurement toolkit — not as a system to restart.

**If you are a future reader (human or LLM) considering reviving this: read §3
first.** The engineering is not what failed. Re-reading the code will not
surface the problem, because the problem is not in the code.

**Epistemic convention used throughout this repo:** `[MEASURED]` = verified
on-chain by this project's own scans · `[REPORTED]` = from docs/web, not
independently confirmed · `[UNMEASURED]` = explicitly unknown. In this project's
history, web-sourced claims failed on-chain verification roughly 3 times out of
4. Trust the tags.

---

## 1. What was built

| component | state |
|---|---|
| `core/` — Rust engine, ~18,465 lines | 318 tests, `fmt` + `clippy -D warnings` clean |
| `contracts/src/Executor.yul` — 387 lines | Deployed to Base mainnet at `0x98Fc3F5c95b34BF3197e1349a2932F6177D336Ef` |
| `contracts/src/FundDistributor.sol` | Deployed; ETH-only payout splitter |
| `scripts/*.py` — measurement toolchain | stdlib-only, checkpointed, resumable (§5) |
| 30+ design docs, 7 runbooks | Accurate about the code; their market premise is falsified |

The Executor performs, atomically in one transaction: Aave `flashLoanSimple` →
`liquidationCall` → collateral→debt DEX swap → repay → profit gate. It is
owner/worker gated (`Executor.yul:35-38`) and has a fixed dispatch table.

The engine provides opportunity detection, REVM fork simulation (`AlloyDB` +
`CacheDB`), pacing and circuit-breaker risk controls, a multi-EOA signer
registry with nonce lanes, Prometheus metrics, and JSONL audit persistence.

---

## 2. The five investigations — all negative

### 2.1 Base Aave V3 liquidations `[MEASURED]`

The original thesis. The entire 30-day liquidation bonus pool on Base Aave V3
was **$4,404 across 22 liquidators, top-1 taking 85.4%** — leaving roughly
**$650/month gross** for perfect execution against the incumbent, before gas,
slippage, and infrastructure.

Worse, it is structurally closed: 99.5% of that value routes through a Chainlink
SVR sealed-bid oracle auction (FastLane Atlas). The price update that makes a
position liquidatable arrives *inside the winning solver's own bundle*. A
public-RPC bot sees the opportunity only after it is gone. **Not a latency
problem — unfixable with a faster node.** Ethereum, Arbitrum and BNB Aave, plus
Compound and Venus, are closed the same way.

Coverage was never the constraint: standing liquidatable stock was **$9.15 of
dust across 214,180 borrowers**. Market *size* was the binding constraint.

### 2.2 The interest-accrual niche `[MEASURED]`

The proposed escape hatch — liquidations triggered by debt accrual rather than
price moves, so no oracle update, no auction, no race. Measured $283.6k/30d and
collapsed on inspection.

Morpho's `seizedAssets` is **denominated at the market oracle price by
construction** (`Morpho.sol::liquidate`: `seizedAssets = repaid × LIF × SCALE /
oracle.price()`), so any oracle-above-market premium reads as bonus while
actually *reducing* what an exit realises. Re-denominated at achievable exit
prices:

| claim | oracle-denominated | exit-denominated |
|---|---:|---:|
| AVLT/USDC (59% of the niche) | $178,004/30d | **−$67,394/30d** — oracle pinned +6.04% above market; only venue is HyperEVM behind a non-atomic bridge; borrow collapsed $4.5M → $43.9k in 30d |
| AZND/USDC | $13,747/30d | **phantom** — price hardcoded 1.0; best route 50,000 AZND → 470 USDC (−99%); one borrower holds 58% of supply |
| ROY-ST-apyUSD | $13,233/30d | **phantom** — oracle +5,118% vs traded |
| All 241 live Ethereum Morpho markets | $277,335/30d | **$2,892/30d** in Gate-0-PASS markets † |

† A floor measured on ~14% of the qualifying set (19 of 137 rows carry a
non-null `bonus_exit`), not a complete accounting. Every market that *was*
priced came in at or below its oracle figure, three catastrophically.

### 2.3 The landscape survey `[MEASURED]`

Against a threshold fixed *before* any number existed ($100k/30d of
exit-denominated, Gate-0-surviving bonus —
[docs/gate0-decision-rule.md](docs/gate0-decision-rule.md)), the whole landscape
yields **≈$69–79k/30d**, of which $50.2k is Base Morpho price-push flow — a
latency race already lost. The pollable remainder is ≈$19–29k/30d fragmented
across ~8 venues, each with an 85–99.7% incumbent. Every non-SVR Aave chain
(Optimism, Polygon, Avalanche, Gnosis, Linea, Scroll, Metis, Sonic, Celo) is
oracle-OPEN and collectively worth ~$10.9k/30d — open only because it is not
worth auctioning.

### 2.4 The "Bespoke Tranche Opportunity" — refuted analytically

A three-leg atomic bundle (pre-trade buy → own `Executor.execute` liquidation →
post-trade sell) intended to capture the price impact of its own liquidation.
Originally named `Sandwich*` and renamed to `Tranche*` in `3adc7ca`; the rename
did not change the mechanism.

**It has no profit source.** Under a constant-product invariant the collateral
reserve traverses `x0 → x0−dq → x0−dq+liq → x0+liq`. The endpoints are identical
to the liquidation alone, so a round trip returning inventory to flat contributes
**exactly zero gross** and is strictly negative once fees are paid. Price impact
is the wedge between marginal and average execution price, paid *by* the crossing
trader *to* the LPs — it is not a harvestable resource. It pays only when a third
party is forced to trade at the worsened price.

Reproduced offline in [`core/tests/tranche_falsification_test.rs`](core/tests/tranche_falsification_test.rs)
(no chain, no keys, no capital) and independently by a 6,125-parameter sweep
across pool sizes, fee tiers, V3 concentrated liquidity, and reversed leg order:
**0 strictly positive results.**

| fee tier | bundle − baseline |
|---|---|
| 0 bps | `0.00000000000000000000000000` |
| 30 bps | `−0.29949420357655465613718168` |
| 100 bps | `−0.99481630960524742017800586` |

**Two traps this structure sets, both now pinned by tests:**

1. **Telemetry rises while the account drains.** The pre-buy genuinely *does*
   raise the price the Executor sells into — leg B output climbs
   51,592 → 61,335 USDC as the pre-trade grows 0 → 100 WETH, while owner P&L
   *falls* +1,567 → −211. An operator instrumenting only `Executor.yul:204-206`
   watches the number go up. The loss lives in the outer legs, outside the
   contract's telemetry.
2. **The `minProfit` gate is not a defence.** The pre-buy inflates
   `balanceAfter`, so wrapping a marginal liquidation can push it *through*
   `ProfitGateFailed` (`Executor.yul:193-202`) while making it worse for the
   owner.

Full analysis: [docs/decision-2026-07-26-tranche-venue.md](docs/decision-2026-07-26-tranche-venue.md).
The path is refused at startup by `PacingConfig::validate_tranche_preconditions`,
keyed off the code constant `TRANCHE_EXECUTION_SUPPORTED = false`.

### 2.5 Cross-chain venue and atomic-arbitrage surveys `[REPORTED]`

Two final surveys asked whether *any* chain hosts the structure profitably.

**For sandwiching:** only Ethereum L1 has both a public mempool and unrestricted
bundles, and it is economically picked over. BSC has the best rails of any chain
*and deliberately filters this exact pattern* — 48Club's open-sourced
`bscexorcist` pattern-matches Buy-Buy-Sell / Sell-Sell-Buy, and the BNB Good Will
Alliance has validators accept bids only from filtering builders (>96% of
blocks); daily sandwiches fell ~140k → <1k. Base, Arbitrum, Polygon, Avalanche
lack a public mempool, a bundle relay, or both.

**For legitimate atomic (cyclic) arbitrage,** ranked by the residual actually
reachable by a *new* entrant after incumbents and builders take their cut:

| chain | new-entrant residual | verdict |
|---|---|---|
| Polygon PoS (137) | ~$17k/mo of $335k gross | MARGINAL |
| Avalanche (43114) | ~$10–40k/mo, unmeasured | MARGINAL |
| Arbitrum One (42161) | $145k/mo gross chain-wide; mean $0.78–1.11/arb | MARGINAL → no |
| Ethereum (1) | ~$0 — builder auction takes 90–99% of gross | CLOSED |
| Base (8453) | ~$0 non-colocated | CLOSED |
| BNB Chain (56) | ~$35k/mo; mean $0.23/arb | CLOSED |
| Optimism (10) | $25–60k/mo gross chain-wide | CLOSED |
| Solana | non-EVM rewrite | CLOSED |

A migration to the least-bad candidate was priced at **3–4 months, $5–25k of
inventory at price risk, for a ceiling of $0–5k/month that decays** — including a
total Executor rewrite (a cycle cannot be expressed by the current contract at
all) and a 6–10 week pool indexer, since the repo contains **zero lines that read
AMM pool state**.

---

## 3. Why — the one structural reason

On every venue measured, the surplus is captured by whoever holds one of exactly
three things:

1. **Exclusive or private order flow** — 70.5% of Ethereum's trading-related
   builder revenue sits behind it; Base has no public mempool at all.
2. **The block-building or sequencing seat** — BSC's two dominant builders run
   their own arbitrage contracts and take 90–92% of MEV profit; Base is one
   Coinbase sequencer.
3. **A colocation and private-feed budget** — Arbitrum's FCFS ordering,
   Solana's 400ms slots, Base's 200ms Flashblocks.

**None of those three is code.** This project's entire asset was code quality,
and there was no venue where code quality was the binding constraint. That is the
whole finding. It held for liquidations, for the accrual niche, for the tranche
structure, for cross-chain sandwiching, and for atomic arbitrage — five times,
independently.

The corollary for any future project: **before writing anything, identify which
of those three the incumbents hold, and whether you can hold one too.** If the
answer is no, the quality of your implementation is irrelevant.

---

## 4. Invariants that must not be re-derived (each was expensive)

1. **Never value an oracle-denominated seizure at oracle prices.** Morpho
   `seizedAssets` is priced by `IOracle.price()` at liquidation time; real PnL
   per event is `repaid_usd × (LIF/(1+premium) − 1)` where
   `premium = oraclePrice/tradedPrice − 1`. `LIF = min(1.15, 1/(1 − 0.3×(1 −
   lltv)))` — ceiling **13.04%** of seized, not 15%.
2. **Never value a 30-day bonus at current prices** (inflated Base Morpho 63%).
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
7. **The canonical Morpho address `0xBBBB…FFCb` has code only on Ethereum +
   Base.** Arbitrum `0x6c247b1F6182318877311737BaC0844bAa518F5e`, Optimism
   `0xce95AfbB8EA029495c66020883F87aaE8864AF92`, Polygon
   `0x1bF0c2541F820E775182832f06c0B7Fc27A25f67` `[MEASURED]`.
8. **Morpho Bundler3 (`0x6566194141eefa99af43bb5aa71460ca2dc90245`) is a public
   router with ≥12 EOA callers, not a competitor** — never count it as one entity
   in concentration numbers.
9. **You cannot sandwich yourself.** Constant-product paths are
   endpoint-determined; a round trip returning inventory to flat nets exactly
   zero gross, strictly negative with fees. See §2.4.
10. **The Aave liquidation bonus is oracle-priced**, therefore an additive
    constant invariant to any AMM games played around it. It cannot be inflated
    by moving DEX prices. (The one exception that would break this: a protocol
    whose liquidation math read AMM spot or a short TWAP — that is an
    oracle-manipulation attack, and does not apply to Aave V3.)
11. **RPC discipline:** keys live in `.env.live` (gitignored), read ONLY by regex
    — never printed, never committed. Infura **caps `eth_getLogs` at 10,000
    blocks on L2 endpoints** — parse the stated limit from the error and latch to
    it; never rediscover by halving.
12. **Toolchain quirks:** `forge`/`slither`/Linux `cargo` run via WSL — pass
    scripts on **stdin** with `wsl bash -s <<'EOF' … EOF` (Git Bash mangles
    absolute paths given as args). Linux file mtimes are coarse-clock granular —
    tests that rewrite files microseconds apart must stamp explicit increasing
    mtimes (see `snapshot_refresh.rs::write_atomically`).
13. **A conditionally-skipped test that `return`s early passes vacuously.**
    `contracts/test/ExecutorBaseFork.t.sol:23-24` is
    `if (bytes(forkUrl).length == 0) return;` — without `BASE_FORK_URL` a green
    `forge test` proves nothing about Base behaviour. Make skips visible.

---

## 5. The measurement toolchain (the part that earns its keep)

This is the genuinely reusable output of the project. It generalizes to any
lending venue with a pool address and an event signature.

| script | role |
|---|---|
| `scripts/scan_liquidation_venues.py` | 30d flow + concentration per venue; emits `bonus_oracle` / `bonus_exit` / `oracle_premium_pct` side by side; checkpointed, span-latching; `--validate` reproduces frozen controls |
| `scripts/gate0_market_filter.py` | the 3-check market screen: routable exit, oracle premium (>1% fails), exit-type classification |
| `scripts/enumerate_morpho_markets.py` | every market ever created on a chain + live state via Multicall3 |
| `scripts/assemble_gate0_survey.py` | merges tier outputs → decision summary vs the pre-declared threshold |
| `scripts/check_venue_open.py` | the SVR openness gate (+`--ext` families: Comet, Moonwell, Morpho, Aave forks) |
| `scripts/measure_liquidation_flow.py` | single-venue deep flow scanner |

**Frozen control values — any scanner change must reproduce these before its
output is trusted:**

```
Base Aave V3 30d : 116 events · ~$79,790 seized · ~$4,404 naive bonus · top-1 85.4%
AVLT/USDC        : bonus_oracle ≈ $178k must collapse to NEGATIVE bonus_exit,
                   exit_type=BRIDGE_ONLY, premium ≥ +4%
Healthy majors   : |premium| ≤ 0.3% (WETH/WBTC/wstETH/cbBTC vs USDC-family)
```

### Key reference data

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
           Executor.execute(bytes) 0x09c5eabe · swapExactTokensForTokens 0x38ed1739
IOracle.price() scale: 1e(36 + loanDecimals − collateralDecimals)
Aave liquidationBonus: getConfiguration bitmap bits 32–47, bps (10500 = 105%)
```

---

## 6. Engine status — honest, component by component

**Sound and reusable:**

- `core/src/simulator/mod.rs` — REVM fork harness (`AlloyDB` + `CacheDB` at
  latest block, prewarming, revert decoding, L2 chain types). Strategy-agnostic.
- `core/src/simulator/seeding.rs` — empirical storage-layout probing.
- `core/src/signer_registry/` — multi-EOA nonce lanes.
- `core/src/pacing_engine.rs` — execution gating and circuit breakers
  (mechanism is sound; the `config/pacing.yaml` numbers are venue-specific).
- `core/src/state/` — JSONL persistence and recovery.
- `core/src/routing/split.rs` — multi-venue split-routing optimizer (greedy
  marginal allocation, `Decimal` throughout). Never wired to execution: the
  Executor takes one router and has no loop.
- `core/src/metrics.rs`, `monitoring/`.

**Known-broken / quarantined — do not trust without fixing:**

- `core/src/simulator/prewarm.rs` — hardcodes Aave storage slot 53 and
  overwrites live fork state; its own sibling `seeding.rs` condemns the practice.
- `core/src/simulator/golden.rs` — asserts nothing, zeroes prices, no callers.
- The simulator validates a bare `liquidationCall`, not the flash-loan path that
  would actually be sent — the flash-loan premium and swap leg are never
  simulated.
- `core/src/tranche_arbitrage.rs`, `tranche_orchestrator.rs`,
  `mempool_predator.rs` — **refuted (§2.4)**, unreachable, retained only as the
  record. `AtomicPacketBundler::verify_inclusion` deliberately errors rather than
  returning a result.

**Safety state at archival:** `config/pacing.yaml` is `execute_mode: shadow`.
`Config::validate()` hard-rejects `max_single_transfer_usd > 1000` and
`max_daily_net_usd > 2000` in *code*, not config. `TRANCHE_EXECUTION_SUPPORTED
= false` blocks the tranche path at startup and on every SIGHUP reload.

> **Operator note before archiving.** `.env.live` is gitignored but contains
> cleartext secrets (`CHIMERA_KEYSTORE_PASSWORD`, `CHIMERA_OPERATOR_TOKEN`, and
> live API keys embedded in RPC URLs), and `deploy/chimera.service` loads it
> wholesale via `EnvironmentFile` with `Restart=on-failure`. **Rotate those keys
> and sweep any remaining funds from the Executor and treasury before walking
> away.** The Executor's `withdraw(address,uint256)` path (`0xf3fef3a3`,
> `Executor.yul:258-284`) is `requireOwner()`-gated and functional: pass
> `token = address(0)` for ETH or an ERC20 address for tokens, and
> **`amount = 0` sweeps the full balance**. Funds go to `caller()`, so call it
> from the owner key.

**CI blind spots:** slither never parses `Executor.yul` (moved aside in CI);
`ExecutorBaseFork.t.sol` silently no-ops without `BASE_FORK_URL`.

---

## 7. Build & test

Prerequisites: Rust stable, Foundry, Python 3.11+, optional slither (WSL).

```bash
cargo test -p chimera-core
```

```bash
cargo fmt --all -- --check && cargo clippy -p chimera-core --all-targets -- -D warnings
```

```bash
BASE_FORK_URL=<rpc> forge test --root contracts/
```

318 Rust tests pass; 26 Solidity tests (21 Executor + 3 FundDistributor + 2
fork). **The fork tests silently no-op without `BASE_FORK_URL`** — see invariant
13.

---

## 8. Document index

**The measurement arc (read in this order):**

| document | role |
|---|---|
| [docs/gate0-decision-rule.md](docs/gate0-decision-rule.md) | the threshold, fixed before the numbers — **the single most valuable artifact here** |
| [docs/gate0-work-order.md](docs/gate0-work-order.md) | the survey's work order — tasks, traps, reference data |
| [docs/executive-brief-2026-07-25.md](docs/executive-brief-2026-07-25.md) | full history, failure taxonomy, corrections log (§9) |
| [docs/flow-measurement-2026-07-25.md](docs/flow-measurement-2026-07-25.md) | the $4,450/mo Base Aave measurement and the incumbent recon |
| [docs/venue-landscape-2026-07-25.md](docs/venue-landscape-2026-07-25.md) | cross-venue flow + concentration |
| [docs/pivot-matrix-2026-07-25.md](docs/pivot-matrix-2026-07-25.md) | every pivot option scored |
| [docs/gate0-survey-2026-07-26.md](docs/gate0-survey-2026-07-26.md) | the survey: methods, tiers A–E, decision |
| [config/gate0_survey.json](config/gate0_survey.json) | 1,055 machine-readable market/venue rows |
| [docs/decision-2026-07-26-tranche-venue.md](docs/decision-2026-07-26-tranche-venue.md) | the tranche refutation, its two self-corrections, and the trap analysis |

**Falsification tests** — offline, no chain access, no keys:
[core/tests/tranche_falsification_test.rs](core/tests/tranche_falsification_test.rs) ·
[core/tests/split_vs_arb_test.rs](core/tests/split_vs_arb_test.rs)

**Handoff — written for life after this repo:**

| document | role |
|---|---|
| [docs/handoff/crypto-corner.md](docs/handoff/crypto-corner.md) | ~16k-word standing reference: market-structure evaluation framework, per-chain cheat sheet, DEX/contract/lending/MEV/ops sections. Destined for Notion. **§1 is the gate to re-read before any future crypto project.** |
| [docs/handoff/cowork-prompts.md](docs/handoff/cowork-prompts.md) | Two Claude Cowork prompts: the archival status update, and the Notion build-out of Crypto Corner |

**Superseded — read only for history:**
[docs/tranche-strategy.md](docs/tranche-strategy.md) describes a strategy that
does not work; its price-direction claim (§:53-54) is inverted and its Flashbots
claims are false on three counts. Retained unedited as the record of what was
believed.

**Engine-era documentation** (accurate about the code; market premise falsified):
[docs/architecture.md](docs/architecture.md) ·
[docs/operator-manual.md](docs/operator-manual.md) ·
[docs/threat-model.md](docs/threat-model.md) ·
[docs/snapshot-schema.md](docs/snapshot-schema.md) ·
[docs/security-research.md](docs/security-research.md) ·
runbooks under `docs/runbook-*.md` · [docs/monetization.md](docs/monetization.md) ·
[AGENTS.md](AGENTS.md) · [CONTRIBUTING.md](CONTRIBUTING.md) ·
[SECURITY.md](SECURITY.md) · [CHANGELOG.md](CHANGELOG.md)

---

## 9. If you are considering restarting this

Do not, unless you can answer **yes** to at least one:

- Do you hold exclusive or private order flow?
- Do you hold a block-building or sequencing seat?
- Can you fund colocation and private feeds against incumbents who already do?

If all three are no, the outcome is already known — it was measured five times.

What *would* change the picture: a new lending venue with meaningful borrow
volume whose liquidations are **not** routed through an OEV/SVR auction, caught
early enough that no incumbent has automated it. The toolchain in §5 detects
exactly that, in about a day, for under $100. Run it before writing code, not
after — that inversion is the entire lesson of this repository.

---

## License

Proprietary — all rights reserved. See [LICENSE](LICENSE). (Versions up to 0.1.4
were MIT; the license changed at 0.2.0 — see CHANGELOG.)
