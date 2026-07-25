# Chimera Swap Engine — Post-Liquidation Collateral→Debt Swap as a First-Class Profit Engine

> **Date:** 2026-07-20 · **Author:** Architect session · **Status:** AWAITING APPROVAL — no code changes made
> **Doctrine:** the flash-loan core captures *paper* profit (the Aave bonus). The swap converts it to *realized* profit. Realized = bonus − (slippage + price impact + DEX fees + gas + flash premium). Venue depth also *caps* position size. This plan treats the swap layer as co-equal to the execution engine.

---

## 1. Current-State Assessment (verified file:line, 2026-07-20)

### 1.1 On-chain swap leg — `contracts/src/Executor.yul`

| Claim | Verdict | Evidence |
|---|---|---|
| Swap is V2-single-hop only | **CONFIRMED** | `callSwapExactTokens` encodes `swapExactTokensForTokens(amountIn, amountOutMin, path[2], to, deadline)`, selector `0x38ed1739`, fixed 260-byte calldata, path length hardcoded `2` (`Executor.yul:368-379`). `IDexRouter.sol:21-38` is V2-shaped only. |
| Swap runs inside flash callback after `liquidationCall` | Confirmed | `Executor.yul:156-183` |
| Router validation = no high bits, nonzero when swap needed, extcodesize | Confirmed | `Executor.yul:61-74`, `132-148`. **No allowlist** — any contract address supplied via calldata is accepted. |
| `amountOutMin`, `minProfit`, `tip`, `deadline` enforced on-chain | Confirmed | `Executor.yul:185-202` (profit gate: `balanceAfter > balanceBefore + premium + minProfit + tip`), `70-74`. |
| Approval hygiene | Exact-balance approve to calldata-supplied router (`Executor.yul:169`), then full debt-asset repay approve to pool (`Executor.yul:187`). Allowance is consumed atomically; no standing approvals persist post-tx. Residual profit/dust sits in contract until `withdraw` (owner-only, `Executor.yul:258-284`). |

**Gaps:** (a) single-hop only — no multi-hop path array; (b) V2 ABI only — no V3 `exactInputSingle`/`exactInput`; (c) no router allowlist — relies on off-chain config correctness (see D2, currently broken); (d) no split routes.

### 1.2 Engine route selection — `core/src/routing/resolver.rs`, `orchestrator.rs`

| Claim | Verdict | Evidence |
|---|---|---|
| `resolve_v2` skips non-`v2` venues | **CONFIRMED** | `resolver.rs:68-72` — `router_compatibility != "v2"` ⇒ `continue`. So the two `v3` venues in routing.yaml are *dead config*, never selected. |
| `amountOutMin` derived from **debt**, not from sim output or pool price | **CONFIRMED (doctrine violation)** | `resolver.rs:150-157`: `debt_to_cover * (10000 + 5 − slippage_bps) / 10000`. No quote, no pool state, no REVM. It assumes the seized collateral sells for ≥ debt × (1+premium−slippage) — true only because Aave over-collateralizes seizure; it is **blind to actual liquidity**. |
| First-match venue wins; no scoring/depth comparison | Confirmed | `resolver.rs:66-101` iterates `venues_for_chain` in file order, returns first pair match. |
| Venue feeds pacing rotation gate | Confirmed | `orchestrator.rs:481` (`venue: route.venue_name`), gate at `pacing_engine.rs:104-109`, deque update `pacing_engine.rs:243-249` (keeps `venue_rotation_count−1 = 4` entries). |
| Simulator never executes the swap leg | **CONFIRMED (critical)** | `simulator/mod.rs:172-245` simulates a **bare `liquidationCall` against the Pool** (`TxKind::Call(self.aave_pool)`, `:275`), not the Executor flash+liquidate+swap path. Profit extraction (`:316-379`) reads the `LiquidationCall` event and computes bonus in **collateral units**, then subtracts `actual_debt_covered` in **debt units** (`:358-368`) — a unit mismatch that only cancels when collateral≈debt in value per unit, and converts at `:376` assuming debt is 18-decimal ETH (`profit_wei/1e18 * eth_price`). For 6-decimal USDC debt the USD figure is wrong by 10¹² unless the heuristic path (`:383-410`) rescues it. **The REVM sim today cannot see swap slippage at all.** |
| Route resolution runs after sim, before pacing | Confirmed | `orchestrator.rs:423-442`; no-route ⇒ skip. |
| minProfit: WETH-debt via Chainlink ETH/USD; stable-debt via $1 assumed; else disabled | Confirmed | `orchestrator.rs:656-704`; live submission refused when `min_profit == 0` and EV > 0 (`:706-721`). |
| Live gas limit = sim gas × 1.2 (floor 200k) | Confirmed | `orchestrator.rs:636,741`. But sim excludes flash+swap ⇒ **live gas limit is underestimated** for the real Executor path. |

### 1.3 routing.yaml — on-chain verification (Base, head ~48904xxx, 2026-07-20)

| Venue | `router_address` in yaml | Probe result | Verdict |
|---|---|---|---|
| aerodrome-base | `0x71524B4f…62859` | 11 264 bytes code; answers `getPair(WETH,USDC)` ⇒ **it is a V2 FACTORY (SushiSwap's)**, not a router, and not Aerodrome | **D2 CONFIRMED and worse than reported** |
| sushi-base | `0x71524B4f…62859` | same address — Sushi **factory**; the actual Sushi router `0x6BDED42c…D7891` (17 762 bytes) returns `factory() = 0x71524B4f…` | Misconfigured: factory in router slot |
| uniswap-v3-base | `0x33128a8f…FdfD` | 24 535 bytes — this is the **Uni V3 FACTORY**; the Base SwapRouter02 is `0x2626664c…e481` (24 497 bytes) | Misconfigured: factory in router slot |
| camelot-arbitrum | `0x6EcCab42…a652` | (not probed — no ARB RPC) pattern matches Camelot **factory** `0x6EcCab42…` | Presumed misconfigured; verify in Phase 0 |
| uniswap-v3-arbitrum | `0x1F98431c…F984` | (not probed) Uni V3 **factory** (same address on all chains) | Presumed misconfigured; verify in Phase 0 |

**D2 verdict: every one of the 5 venues has a factory (or wrong) address in the router slot.** The two Base "v2" venues point at the Sushi *factory*: `swapExactTokensForTokens` on it reverts (no such function) ⇒ every live swap through the current yaml would revert. In shadow mode the tx is never broadcast, so this is latent, not burning gas — but the moment live flips, `consecutive_reverts` hits 3 and the breaker trips (`pacing_engine.rs:252-257`). **This is the single most urgent config defect.**
Note: prior handoff (`.kilo/plans/kilo-code-handoff-2026-07-09.md` defect 9) flagged aerodrome-base as the Aerodrome *PoolFactory* `0x420DD381…`; the yaml has since drifted to the Sushi factory — same class of bug, new wrong value. Canonical Aerodrome router probe returned **0 code** on this node's view (`0xcF77a3Ba…874E43`) — flagged as an open item to re-verify from a second provider in Phase 0 before trusting any address.

### 1.4 Pair coverage — `config/routing.yaml:23-69` vs 15 snapshot reserves

Configured Base pairs: **WETH→USDC, WETH→USDbC only.** Snapshot reserves (15): WETH, cbETH, USDbC, wstETH, USDC, weETH, cbBTC, ezETH, GHO, wrsETH, LBTC, EURC, AAVE, tBTC, syrupUSDC.
- GHO and syrupUSDC have `liquidation_bonus: 0` in the snapshot — unprofitable to seize; excluded from coverage target.
- **D3 CONFIRMED:** 13 seizable collateral types; 12 of 13 have **no exit route** (only WETH covered). Collateral→debt coverage = 1/13 ≈ 8%.

### 1.5 Venue tooling — `scripts/update_venues.py`

- **D4 partially CONFIRMED, reframed:** `FACTORIES["base"]["sushiswap"] = 0x71524B4f…` is **correct as a factory** (probe: answers `getPair`). The defect is that this factory address was then copied into routing.yaml's *router* slot — the script's address book is fine; the hand-off to config is broken.
- **Script is runtime-dead:** `discover_venues` constructs `Venue(address=…, pair=…, verified=…)` (`update_venues.py:438-449`) but the `Venue` dataclass (`:176-183`) has `router_address`/`pairs` fields → `TypeError` on the first qualifying venue. It has never successfully written yaml in this form.
- Hardcoded June-2026 prices (`:102-111`) — stale fallback, no oracle.
- Depth = ERC20 `balanceOf` on the pool (`:283-309`) — reserves, fine for V2 TVL, but meaningless for V3 (ignores active-tick liquidity) and for *executable depth at size* (no price-impact math).
- Only probes WETH/USDC (`:432`) — cannot close D3.
- On RPC failure it **falls back to stub liquidity constants** (`:315-320,331-344`) and marks the venue "verified" — silent fabrication of the venue list. Unacceptable for a profit-critical input.

### 1.6 Config validation — `core/src/config.rs:963-1068`

`RoutingConfig::validate` checks kyc, ≥$50k, `router_compatibility ∈ {v2, v3, custom, ""}` (`:1030-1062`). It does **not** check: router vs factory confusion, pair-token sanity, chain-specific address formats, per-kind required fields (V3 fee tier), or that a `v2` venue's router actually speaks V2 ABI. `venues_for_chain` (`:1064-1067`) confirmed.

### 1.7 Pacing interplay

- Rotation gate confirmed (`pacing_engine.rs:104-109`); window = last `venue_rotation_count−1` = **4** venues. With only 3 Base venues configured (2 after v3 pruning), the rotation window can cover **every** venue ⇒ if route scoring ever has < window+1 executable venues, *all* candidates get denied. Not a bug today (scoring is first-match), but a **hard constraint on the route model**: executable-venue count per chain must stay ≥ `venue_rotation_count` (5) or the rotation window must shrink. Current executable count after D1/D2 fixes: 1–2. **This is a latent liveness bug for the swap engine** — venue diversity is a pacing invariant, so the plan must either grow the executable venue set to ≥5 per chain or make the rotation window venue-count-aware.
- `ESTIMATED_GAS_UNITS = 150_000` (`pacing_engine.rs:183`) is below the real Executor path cost (flash+liquidate+swap ≈ 350–600k) ⇒ profit-multiplier gate under-charges gas. Must be route-aware.

### 1.8 Observability today

Metrics (`metrics.rs:17-31`): candidates, sims, latency, profit histogram, reverts, breaker, gas, L1 fee, daily/weekly net, sweeps. **Nothing swap-specific**: no per-venue series, no expected-vs-realized out, no slippage bps. Dashboard (`dashboard.py:451-582`) synthesizes `/api/state` from Prom text + log tail + snapshot + ticker; extend via new Prom series + a routing section — no rebuild needed.

---

## 2. Target Architecture

### 2.1 Route model

```rust
// core/src/routing/model.rs (new)
enum SwapKind { V2, V3 { fee_tier: u32 }, Aerodrome { stable: bool }, Aggregator }
struct SwapLeg { venue: String, kind: SwapKind, router: Address,
                 token_in: Address, token_out: Address, pool: Option<Address> }
struct Route {
    legs: Vec<SwapLeg>,            // 1..=3 legs; v1 ships 1–2
    expected_out: U256,            // debt units, from quote/sim at given size
    amount_out_min: U256,          // expected_out × (1 − slippage_max_bps)
    max_size: U256,                // sizing cap from depth (see 2.3)
    calldata: Vec<u8>,             // per-leg router calldata, engine-encoded
}
```

- **Ownership:** a new `RouteScorer` (engine) owns selection; the simulator owns *validation* (full-path REVM); the assembler owns *encoding*. Selection ≠ validation ≠ encoding — three separate components, single direction of data flow: `Scorer.rank()` → pick best pacing-eligible → `Simulator.simulate_full_path(route)` → on success `Assembler.build(route)` → submit. This replaces today's "config first-match" with score-then-simulate.
- **Single atomic tx preserved:** all legs execute inside the one flash callback in the Executor (multi-leg encoding on-chain, §2.2). No bundles (Base model unchanged).
- **`amountOutMin` provenance (hard rule):** derived from the **REVM full-path sim's measured output at the chosen size**, discounted by `slippage_max_bps` — never from `debt_to_cover` arithmetic. Sim output is the quote of record; the on-chain gate enforces it.
- **`minProfit`:** unchanged on-chain gate; off-chain computed from sim's post-swap, post-gas, post-L1-fee USD surplus × `min_profit_fraction`, denominated in debt units via the debt asset's oracle price (extends today's WETH/stable special-casing to any reserve with an Aave/Chainlink feed).

### 2.2 Executor contract evolution — recommended: generic passthrough with strict allowlist

Evaluated:
- **(a) Per-type encodings (V2/V3/Aerodrome) in Yul:** smallest conceptual diff but每 new venue kind = new Yul + new audit surface + redeploy; V3 path encoding (packed bytes) is fiddly in raw Yul; aggregator support impossible without yet another selector.
- **(b) Generic approved-target calldata passthrough (RECOMMENDED):** Executor stores an **owner-managed router allowlist** (mapping, slot 3). New `execute(bytes)` v2 payload: the 11 words **plus** a dynamic tail: `leg_count`, then per leg `{target, callData_length, callData}`. On-chain checks: `target ∈ allowlist`, `callData.length ≤ 1024`, `leg_count ≤ 3`; per leg: exact-balance approve of leg input token → `call(target, callData)` → require success; after all legs: existing premium+minProfit+tip gate unchanged, plus **final debt-balance ≥ debt before + amountOutMin** (explicit, since a passthrough target can't be trusted to enforce our min-out). Engine encodes V2 `swapExactTokensForTokens`, V3 `exactInputSingle`/`exactInput`, Aerodrome `swapExactTokensForTokens(Route[])` (note: Aerodrome router ABI is **not** V2-compatible — routes carry a `stable` flag; another reason per-type Yul (a) is inferior), or aggregator calldata off-chain where encoders are testable Rust, not Yul.
- Migration: **new Executor deployment** (bytecode changes), then a config flag `executor_abi: v1|v2` so the engine can drive either; old workers re-authorized on the new contract; ownership to Safe as today (`vm.addr(pk)` constructor pattern per project decision). Rollback = point `executor_address` back at the v1 contract.
- Foundry tests required (invariant): allowlist enforcement, per-leg approval scoping, multi-leg V2 path happy/sad, V3 exactInputSingle against a fork mock, calldata-size cap, profit-gate parity with v1, dust/no-residual-allowance assertions, selector/length exactness tests mirroring the existing 11-word suite, Base-fork test against real Sushi router + real Uni V3 SwapRouter02.

### 2.3 Route scoring & liquidity intelligence — `core/src/routing/scorer.rs` (new)

- **Expected-out model per venue** (all Decimal at USD boundary; U256 token math):
  - V2: quote from on-chain `getAmountsOut` at candidate size (batched via one RPC round per scan, or read reserves from a cached pool-state snapshot refreshed with the block scan) → fee 0.3% (Sushi) / Aerodrome variable+stable fee tiers.
  - V3: quote via `QuoterV2.quoteExactInputSingle` (off-chain `eth_call`, no tx) per fee tier.
  - Convert out to USD via debt-asset oracle price; score = `expected_out_usd − gas_cost_usd(route)` where gas is **route-aware** (base Executor path + per-leg cost table, calibrated from fork tests).
- **Sizing cap from depth:** for V2-style pools, impact ≈ `x / (x + reserve_in)`; cap `amount_in` at `reserve_in × max_impact_bps/10_000` (config `route_max_impact_bps`, default 100). For V3, cap at quoted size where marginal price moves > impact budget (Quoter at ladder sizes). `Route.max_size` feeds back to `debt_to_cover` clamping pre-sim: `debt_to_cover_final = min(detector_amount, max_size_debt_equivalent)` — a $500k position with $80k depth becomes an $80k liquidation by construction.
- **Tie-break:** (1) higher expected_out_usd − gas; (2) venue **not** in the pacing rotation window (soft preference — hard denial stays in pacing); (3) lower leg count; (4) config file order (deterministic).
- **Rotation interplay:** scorer receives the current rotation deque snapshot (read-only) and ranks; orchestrator walks the ranked list and picks the first pacing-`Allow`. This removes today's structural conflict where first-match could pick a venue the pacing gate then denies.
- **Dead-venue failover mid-assembly:** if the full-path REVM sim reverts or the quote moved > `requote_drift_bps` between rank time and build time, the scorer excludes that venue *for this candidate* and re-ranks (bounded: one re-rank per candidate, then skip).
- **Decimal invariant:** USD math in `rust_decimal::Decimal`; token-unit math in `U256`; single conversion boundary mirroring `SimulationResult` (`simulator/mod.rs:99-101`).

### 2.4 Venue lifecycle

- **routing.yaml schema v2** (backward-compatible via serde defaults):
  ```yaml
  - name: "sushi-base"
    chain: "base"
    type: "dex"
    kyc: false
    router_compatibility: "v2"        # v2 | v3 | aerodrome | aggregator
    router_address: "0x6BDED42c…"     # ROUTER (verified on-chain)
    factory_address: "0x71524B4f…"    # NEW: for discovery/verification
    fee_tiers: [500, 3000, 10000]     # v3 only
    pairs:
      - token_in: "0x…"               # collateral
        token_out: "0x…"              # debt
        pool: "0x…"                   # NEW: pinned pool address
        depth_usd: 123456             # NEW: measured at refresh
        depth_as_of: 2026-07-20T00:00:00Z
  ```
  `config.rs` validation additions: router ≠ factory; per-kind required fields (v3 ⇒ fee_tiers non-empty; aerodrome ⇒ stable flag present); address parse checks; **`router_compatibility` ↔ executor ABI capability gate** (engine refuses venues its Executor can't drive — closes D1 at config level); chain allowlist. Fixture + disk-parse tests updated (existing pattern at `config.rs:1070-1327`).
- **`update_venues.py` rewrite (Phase 1):**
  - Fix the `Venue` ctor crash (align fields); make it emit schema v2.
  - **Kill stub liquidity fallbacks** — RPC unreachable ⇒ venue marked `verified: false` and excluded (fail-closed).
  - Live USD prices: Chainlink feeds on-chain (Base ETH/USD `0x71041ddd…Bb70` already in pacing.yaml; extend a small `config/chainlink_feeds.yaml` for the 13 reserves; fall back to Aave oracle `getAssetPrice`) — no hardcoded prices.
  - Depth: V2 reserves via `getReserves()` on the pinned pool (not `balanceOf`, which includes fees/skew); V3 via `liquidity()` + slot0 tick → active-depth estimate; record `depth_as_of`.
  - **Router verification step:** for each candidate venue, `eth_getCode` + ABI probe (`factory()` for V2, `factory()`+`WETH9()`/`quoter` presence for V3 routers) and cross-check factory-derived pool == pinned pool. Refuse to write yaml on mismatch. (This single step would have caught today's all-factories config.)
  - RPC-credit hygiene: all discovery batched, one-shot, manual/cron invocation; never a loop.
- **Cadence:** weekly scheduled run + on-demand after any config validator alert; `depth_as_of` older than 48h ⇒ scorer treats venue as unranked (stale-depth guard).

### 2.5 Collateral exit coverage — `scripts/route_inventory.py` (new, Phase 1)

- For each of the 13 seizable Base reserves: enumerate candidate exits (direct collateral→debt-asset pools; collateral→WETH→debt 2-hop via configured venues) with measured per-leg depth and computed max size; write `config/route_inventory.json` (collateral → best routes + depth + cap).
- Coverage gate: `covered_collaterals / 13`; emit metric `chimera_route_coverage_ratio`; dashboard banner + log WARN when < 1.0 naming the uncovered symbols. Re-run on the venue-refresh cadence.
- Anticipates the position-discovery workstream: when `users` land in snapshot.json, the detector will emit exactly these collateral types — coverage must already be 13/13.

### 2.6 Observability (extend, don't rebuild)

- New Prom series (`metrics.rs`): `chimera_route_selected_total{venue,kind}`, `chimera_route_expected_out_usd{venue}`, `chimera_swap_realized_out_usd{venue}` (from tx receipt balance delta, live mode), `chimera_swap_slippage_bps{venue}` (realized vs sim'd), `chimera_route_coverage_ratio`, `chimera_route_failover_total{venue}`. Label cardinality bounded (≤ 8 venues × 4 kinds).
- `/api/state` additions under `routing:` { coverage, venues: [{name, kind, depth_usd, depth_as_of, in_rotation_window}], recent: [{route, expected, realized, slippage_bps}] } — sourced from Prom text + `route_inventory.json`; `dashboard.py` gains a small parser for the new series + one panel section. Log-line additions (engine `info!` on select/failover/realized) flow into the existing event tail with a new `kind: "route"`.

### 2.7 Risk posture (summary — full register in §5)

- Sandwich/MEV: Base has a single sequencer + private-orderflow options; primary mitigation is **atomicity** (sandwich can't separate flash from swap) + `amountOutMin` from REVM + `minProfit` gate + short deadline (300s today — reduce to ~60s) + private RPC submission (`routing.yaml primary` already "alchemy-base-private"). Residual: cross-tx sandwich of the swap leg — bounded by min-out, priced into `slippage_max_bps` (50 bps cap, `risk.yaml:8`, validation ceiling 200 bps `config.rs:898-902`).
- Stale-depth reverts: full-path sim per candidate + `depth_as_of` freshness gate + one failover re-rank.
- Dust: per-leg exact-amount approvals (no unlimited), post-tx residual sweep via existing owner `withdraw`; add optional auto-sweep of profit token to treasury post-success (later phase, owner call only).
- Approval hygiene: allowlisted routers only; exact-balance approve per leg; no standing approvals.
- Oracle-vs-DEX divergence: scorer compares oracle-implied out vs quoted out; divergence > `oracle_divergence_bps` (default 300) ⇒ venue excluded (manipulation or dead pool guard).

---

## 3. Phased Implementation Plan

> Every phase is shadow-testable first; committed config never leaves `execute_mode: shadow`; validation gate per AGENTS.md before merge: `cargo test -p chimera-core` · `forge test --root contracts/` · `python -m compileall scripts ai-audit/scripts` · `slither contracts --config-file slither.config.json`.

### Phase 0 — Config triage + verification harness (no behavior change; ~1 day)
1. Fix routing.yaml Base routers to verified values (Sushi router `0x6BDED42c…D7891`; Uni V3 SwapRouter02 `0x2626664c…e481`); re-verify canonical Aerodrome router from a **second** RPC provider (this node returned 0 code — treat as suspect until cross-checked); verify Arbitrum addresses with an ARB endpoint.
2. Remove/neutralize the dead `v3` duplicate-factory entries; keep `router_compatibility` honest per actual capability (v3 venues stay config-present but engine-ineligible until Phase 3).
3. Add a tiny `scripts/verify_venues.py` (offline-importable, stdlib+optional web3): getCode + ABI probes + factory/pool cross-check + report. Run once, commit its output table into this doc's follow-up.
- **Tests:** `verify_venues.py` import test w/o web3; routing.yaml still parses (existing disk tests). **Rollback:** git revert of the yaml edit.
- **Exit:** 2/2 Base v2 venues executable-in-shadow; D2 closed with probe evidence.

### Phase 1 — Scoring + coverage + tooling, engine-only (no contract change; ~1 week)
1. `routing/model.rs` + `routing/scorer.rs` (V2-only kinds initially): expected-out via batched `getAmountsOut` eth_calls at candidate size; sizing cap (`route_max_impact_bps`); ranked list; rotation-aware pick; one re-rank failover. Orchestrator swaps `resolve_v2` first-match for scorer (behind `routing_scoring: true` config flag, default true with legacy fallback).
2. **Full-path REVM sim**: add `simulate_executor_path` that replays flash+liquidate+**swap** in REVM (Executor bytecode + mock pool or fork state), producing swap-measured `expected_out` ⇒ `amountOutMin = sim_out × (1 − slippage_max_bps)`, and route-aware gas. Fixes the unit-mismatch in `extract_profit_from_state` as part of the new path (per-asset oracle prices, correct decimals).
3. `scripts/route_inventory.py` + `config/route_inventory.json` + coverage metric/alert (13/13 target: WETH pivot routes collateral→WETH→{USDC,USDbC} initially).
4. `update_venues.py` rewrite per §2.4 (ctor fix, fail-closed, live oracle prices, pinned pools, verification step).
5. routing.yaml schema v2 + config.rs validation + fixtures.
6. Metrics + dashboard extension per §2.6.
- **Tests:** scorer unit tests (quote math, impact cap, tie-breaks, failover), sim-vs-fork golden (V2 Sushi WETH→USDC at several sizes vs actual pool state), config validation tests (router≠factory, v3-requires-fee-tiers), inventory script tests w/o web3, dashboard parser test. **Rollback:** `routing_scoring: false` → legacy first-match path; all new code is additive.
- **Exit:** shadow logs show per-candidate ranked routes, sim-derived `amountOutMin`, coverage = 13/13 via WETH pivot, zero stub-liquidity venues.

### Phase 2 — Executor v2 (passthrough + allowlist) (~1–2 weeks, contract change)
1. Yul: router allowlist (slot 3, owner `setRouter`), `execute` v2 payload with leg array (≤3 legs, ≤1024 B calldata/leg), per-leg exact approve + call, final debt-min-out + existing profit gate; keep v1 selector functional for rollback.
2. Rust encoders: V2 path, (V3 `exactInputSingle` behind kind flag — enabled Phase 3), leg-array ABI; assembler emits v2 payloads; `executor_abi` config flag.
3. Deploy new Executor via `Deploy.s.sol` (constructor owner = deployer EOA per project decision), re-authorize workers, transfer ownership to Safe; `.env.live` points at new address only after fork tests pass.
- **Tests (hard gate):** Foundry unit suite mirroring today's 638-line file extended for v2 (allowlist deny, leg-cap, calldata-cap, multi-leg happy path, no residual allowance, profit-gate parity), **Base-fork tests** executing real Sushi V2 swap + (dry) V3 SwapRouter02 swap through the new contract; slither clean; shadow e2e drives v2 path against fork. **Rollback:** `executor_abi: v1` + old `executor_address`; new contract sits idle (no value at risk — it holds nothing between txs).
- **Exit:** fork-proven v2 contract; engine shadow-drives multi-leg V2 routes through it.

### Phase 3 — V3 + Aerodrome activation (~1 week)
1. Enable `v3` kind in scorer (QuoterV2 quotes, fee-tier ladder) and assembler (`exactInputSingle`/`exactInput` encoders); activate Uni V3 Base venue (SwapRouter02, verified Phase 0).
2. Aerodrome kind: `Route[]` encoder with `stable` flag; activate if depth beats Sushi/V3 for any pair (scorer decides per candidate).
3. Raise executable Base venue count to ≥ 5 (Sushi, Uni V3, Aerodrome volatile+stable, + 1 aggregator or second V2) to satisfy the pacing rotation invariant (§1.7) — or make rotation window venue-count-aware in the same PR (config + pacing + fixture sync).
- **Tests:** V3 quote-vs-fork accuracy (±1 bip), Aerodrome stable/volatile fork swaps, rotation-liveness test (≥5 venues ⇒ no global denial), validation gate. **Rollback:** disable kinds via config (`enabled_kinds: [v2]`).
- **Exit:** per-candidate best-of-3+-venue selection live in shadow; rotation invariant satisfiable.

### Phase 4 — Live-readiness hardening (~1 week, operator-gated)
1. Split routes (two venues, two legs) where depth is fragmented — scorer already supports legs; enable after Phase 2/3 data shows benefit.
2. Aggregator kind evaluation (quote-only initially; passthrough target only after manual review of their router) — optional.
3. Go-live pack addendum: gas table re-calibration (`ESTIMATED_GAS_UNITS` per route kind), runbook for router allowlist ops, slippage-bps review from realized-vs-sim dashboard data, oracle-divergence alert tuning.
4. Position-discovery handoff checklist: coverage gate 13/13 enforced before first user-emitting snapshot is accepted.
- **Exit:** operator sign-off; live flip remains a separate, explicitly-approved step with env-local config.

---

## 4. Test & Rollback Matrix (per phase)

| Phase | New tests | Rollback |
|---|---|---|
| 0 | venue verifier import test; yaml parse | git revert yaml |
| 1 | scorer units; fork-golden swap quotes; config validation; inventory/dashboard tests | `routing_scoring: false` |
| 2 | full Foundry v2 suite; Base-fork real swaps; slither | `executor_abi: v1` + old address |
| 3 | V3/Aerodrome fork quotes+swaps; rotation liveness | `enabled_kinds: [v2]` |
| 4 | calibration regression vs captured shadow data | n/a (ops docs) |

Global invariants maintained: pacing.yaml↔config.rs↔fixture 3-way sync (any pacing change ships all three); snapshot-schema.md↔prewarm.rs (untouched by this plan); Decimal money; Cancun EVM; web3-less Python imports; Foundry test per contract change.

## 5. Risk Register

| # | Risk | Likelihood | Impact | Mitigation (phase) |
|---|---|---|---|---|
| R1 | Another factory/router config mistake | Med | Swap reverts, breaker trips | Phase 0 verification script + config validation router≠factory + ABI capability gate (1) |
| R2 | Sandwich on public mempool | Low-Med (Base) | Slippage up to `amountOutMin` band | Atomicity; private RPC; REVM-derived min-out; 60s deadline; profit gate (2,4) |
| R3 | Stale depth → revert at execution | Med | Lost gas, revert counter | Full-path sim per candidate; depth_as_of freshness gate; failover re-rank (1) |
| R4 | Passthrough target abuse (compromised worker) | Low | Bounded by per-tx exact approvals + min-out + profit gate | On-chain allowlist; calldata caps; worker rotation; monitoring (2) |
| R5 | Oracle/DEX divergence (manipulated or dead pool) | Low | Bad quote → revert or dust profit | Divergence guard `oracle_divergence_bps`; venue exclusion (1) |
| R6 | Rotation window ≥ executable venues ⇒ all candidates denied | **High today** | Liveness failure | ≥5 executable Base venues (3) or venue-count-aware window (3) |
| R7 | Gas underestimate (150k vs ~500k) | High | Profit-multiplier gate mispriced | Route-aware gas table from fork calibration (1,4) |
| R8 | Sim/reality drift on V3 (tick-crossing, multi-hop) | Med | amountOutMin too tight → reverts; too loose → MEV room | Quote ladder + fork goldens + slippage-bps dashboard feedback (3,4) |
| R9 | RPC credit burn from quoting | Med | Cost | Batched eth_call per scan; quoter only for shortlisted venues; cached pool snapshot (1) |
| R10 | Aerodrome canonical-address uncertainty (0-code probe) | ? | Wrong config | Second-provider verification in Phase 0 before enabling (0) |
| R11 | Aggregator dependency creep | Low | New trust/API surface | Quote-only first; passthrough only after manual router review (4) |
| R12 | Dust accumulation in Executor | Low | Stranded cents | Existing owner `withdraw`; optional auto-sweep later (4) |

## 6. Explicit Assumptions & Open Items

1. **A1:** Sushi router on Base = `0x6BDED42c6DA8FBf0d2bA55B2fa120C5e0c8D7891` — **verified on-chain** (code + `factory()` back-reference). Uni V3 SwapRouter02 = `0x2626664c2603336E57B271c5C0b26F421741e481` — code verified; calldata shape to be fork-proven in Phase 2.
2. **A2:** Canonical Aerodrome router `0xcF77a3Ba…874E43` returned **0 code** from this node's view — unresolved; requires second-provider check (Phase 0). If truly absent, Aerodrome address book needs re-sourcing from Aerodrome docs before Phase 3.
3. **A3:** Arbitrum venue addresses not probed (no ARB RPC in env); presumed same factory-in-router-slot defect class; Phase 0 verifies before any arb use.
4. **A4:** Aave V3 flash premium 5 bps (already assumed in `resolver.rs:151`) — reconfirm against live Pool `FLASHLOAN_PREMIUM_TOTAL` in Phase 1 (cheap view call).
5. **A5:** Camelot is deprioritized (Arbitrum chain process is out of the live scope per current ops); Base venues carry the P&L plan.
6. **A6:** The position-discovery workstream will emit the 13 collateral types identified here; if it adds reserves, the inventory script re-runs and coverage gate re-evaluates automatically.
