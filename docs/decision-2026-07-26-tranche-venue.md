# Decision — Bespoke Tranche Opportunity: venue, and whether to build it

**Date:** 2026-07-26
**Status:** Decided — do not wire. Venue selection is moot.
**Supersedes:** the venue-selection action item in the 2026-07-26 Defensive Architectural Audit
**Scope:** `core/src/tranche_arbitrage.rs`, `docs/tranche-strategy.md`, PR #26 (`8518108`)

---

## 1. Decision

1. **There is no Base-side private orderflow venue that provides what the strategy requires.** Not a
   preference between candidates — the category is empty on Base today.
2. **The strategy has no gross edge in the form it is documented,** independent of venue. Venue was
   never the blocker, so choosing one would not unblock anything.
3. **The tranche path is refused at startup** (§5). Enabling it now requires a source change, not an
   environment variable.
4. **The end-to-end wiring, the collateral-inventory provisioning, and the four documentation
   rewrites are not being built.** Reasons in §6.

An architectural audit asked for this memo before any relay-facing code changed. That sequencing was
right, and it is what surfaced the finding in §3 — which is the reason the answer is "don't build it"
rather than "build it against venue X."

---

## 2. Why no venue exists

The module hardcodes `https://rpc.flashbots.net` and documents it as the "Default Flashbots Protect
relay for Base mainnet" (`core/src/tranche_arbitrage.rs:31-32`). Every clause of that comment is
wrong.

| Check | Result |
|---|---|
| `eth_chainId` at `rpc.flashbots.net` | `0x1` — Ethereum L1 |
| `eth_chainId` at `mainnet.base.org` | `0x2105` (8453) |
| `eth_sendBundle` on Base | HTTP 403 / `-32601 rpc method is unsupported` |
| `eth_sendBundle` unsigned | `-32600 "signature is required"` |

Probes used an empty `txs` array; nothing was broadcast.

Three further defects the audit did not reach:

- The code posts to the Protect **user RPC**, not the bundle relay (`relay.flashbots.net`).
- Flashbots documents `eth_sendBundle` for Mainnet and Sepolia only.
- **No substitute exists.** Base is one Coinbase-run sequencer with a private mempool, a single
  operator-run builder, and priority-fee ordering in ~200ms Flashblocks. There is no competitive
  builder market for a bundle to be auctioned into. BlockRazor, the nearest commercial option, lists
  bundle methods for Ethereum and BSC only and offers plain `eth_sendRawTransaction` for Base.

`TrancheBundler` carries a `chain_id` field (`tranche_arbitrage.rs:236`) and never reads it, so
nothing in the code catches the mismatch.

**This repository already reached this conclusion twice and the tranche work contradicted both.**
`config/routing.yaml:2` reads `# L2-focused: no Flashbots eth_sendBundle assumption.` and pins
`submission_style: "single_atomic_tx"  # NOT bundle`, validated at `core/src/config.rs:1157-1161` and
locked by `test_routing_rejects_invalid_submission_style`. And
`docs/venue-landscape-2026-07-25.md:169-172` states that on Base "there is no public mempool to be
front-run from, so a Flashbots-style private RPC buys nothing," with `:232-234` recording an explicit
instruction not to wire private submission as framed.

Consequently the atomicity guarantee the strategy rests on does not exist.
`docs/tranche-strategy.md:70-74` claims partial execution is "structurally impossible." With no relay
serving Base, the three transactions would go to the sequencer individually and **the pre-trade can
land alone, leaving an unhedged directional position** — precisely the failure mode declared
impossible.

---

## 3. The finding that makes venue selection moot

The audit's Gap 5 called the profit source "unsimulated" and prescribed extending the simulator. That
framing understates it. The profit source does not exist.

The strategy buys the collateral asset, has its **own** Executor sell that same asset as the
liquidation's swap leg, then sells again — and books the resulting price move as revenue
(`docs/tranche-strategy.md:67`). Two independent defects, either one fatal.

**(a) The direction is inverted.** `contracts/src/Executor.yul:172-180` calls
`callSwapExactTokens(dexRouter, collateralBalance, amountOutMin, collateralAsset, asset, ...)` — it
sells the *entire* seized collateral balance for the debt asset. Selling collateral raises the
collateral reserve and drains the debt reserve, so the collateral price **falls**.
`docs/tranche-strategy.md:53-54` asserts the swap "caus[es] price impact on the collateral asset at
the DEX, **elevating** the price to P′." Backwards. So `(P′ − P)` is negative and
`Remuneration = (P′ − P) × position_size − gas − tip` is a loss term by construction. (The §7 diagram
additionally subtracts dollars from a price.)

**(b) Correcting the direction does not help, because the counterparty is yourself.** This is the
load-bearing point. Under a constant-product invariant the collateral reserve traverses
`x0 → x0−dq → x0−dq+liq → x0+liq`. The endpoints are identical to the liquidation alone, and value
moved depends only on endpoints, so a round trip returning inventory to flat contributes **exactly
zero**. Modelled numerically: with fees set to zero, bundle-versus-baseline delta is `0.000000` to 30
decimal places across 22 combinations of three pools, liquidation sizes {1, 10, 137} and pre-trade
sizes {0.001, 3, 50}. With real fees the delta is strictly `−(fee_A + fee_C)`: −2.97 at 1bps, −14.85
at 5bps, −89.24 at 30bps, −298.57 at 100bps, before gas and tip. Reversing the legs to the genuinely
profitable tranche orientation hands the tranche account +582.41 while the Executor's swap proceeds
fall by *exactly* 582.41 — same beneficial owner, netting to zero minus fees. That orientation also
trips the Executor's own guards (`amountOutMin`, then
`balanceAfter > balanceBefore + premium + minProfit + tip`), reverting the middle leg.

The Aave liquidation bonus is orthogonal: the delta is identically −89.24 at 1%, 5% and 10% bonus,
because the bonus accrues inside `liquidationCall` regardless of the outer legs. Attributing it to
`(P′ − P)` would double-count revenue the base liquidation path already books.

Price impact is not a harvestable resource. It is the wedge between marginal and average execution
price, paid **by** the crossing trader **to** the LPs. trancheing pays only because a third party is
forced to trade at the worsened price: with the middle sell owned by someone else, the trancheer
nets +204.13 and the victim loses −292.06. Remove the victim and the revenue goes with it, leaving
the costs.

Three reviewers — constant-product AMM mechanics, a practitioner lens, and a deliberate steelman
instructed to build the strongest possible case — reached this independently and unanimously. The
steelman's verdict: three individually true premises "assembled in a way that cancels to zero."

The loss is analytically bounded away from zero, so **`CHIMERA_TRANCHE_MIN_PROFIT_USD` can never be
satisfied** and a correctly implemented profit gate would reject 100% of candidates.

---

## 4. The fork in the road — for the operator to decide

The strategy has exactly two coherent states.

**(a) Honor the documented single-executor constraint.** `docs/tranche-strategy.md:175-176`: "targets
only the project's own deployed Executor... never targets third-party transactions." Then EV is
negative by construction — two extra swap-fee crossings, self-inflicted slippage, two extra
transactions of gas, and the relay tip, for zero gross edge.

**(b) Relax the constraint so the middle transaction belongs to a third party.** Then the mechanism
works, and it is a tranche attack: extracting another liquidator's slippage tolerance, with
second-order harm to the liquidated borrower and the pool's LPs. `docs/monetization.md:111` already
rejects "trancheing or other predatory MEV" **categorically — not deferred, not gated, rejected** —
and `:39-43` grounds the project's ethical case on taking "nothing but the protocol's own bonus."

**The code is built for (b) while the document asserts (a).** `TrancheScanner::matches(to, calldata)`
is a filter over a stream of foreign transactions. `decode_target_tx` reconstructs `collateral`,
`asset`, `user`, `debt_to_cover` and `dex_router` from *observed* calldata. It harvests
`gas_price_wei` and `max_priority_fee` **from the observed transaction** — something you need only in
order to outbid someone else's ordering. `build_bundle` takes `target_raw` as an opaque already-signed
input, and `tranche_arbitrage.rs:257` calls it "the intercepted raw transaction." You cannot intercept
your own transaction: you hold it in memory before broadcast, and a transaction sent through a private
relay never enters a mempool you can scan. §5's stated benefit — "other searchers cannot observe and
replicate the pre-trade position" — is a concern about competing trancheers racing you to a victim,
incoherent under (a).

Independently, `execute(bytes)` is owner/worker-gated (`contracts/src/Executor.yul:35-38`; selector
`0x09c5eabe` confirmed), so a scanner keyed on `to == our_executor && selector` can only ever surface
transactions we signed ourselves. The scanner has no honest purpose under (a) and no wiring under (b).

**Recommendation: a third option the audit did not offer — withdraw the strategy,** correct or retract
`docs/tranche-strategy.md`, and remove the dead module. Option (b) is not something to implement.

**Mitigating fact:** nothing shipped. `build_bundle` and `submit_bundle` have zero call sites, no code
constructs the pre/post legs, and the module is a re-exported island. This is a documented design that
does not work, not a live system losing money.

---

## 5. What was implemented instead

A fail-closed startup guard, because it reduces risk under every possible strategy verdict.

`PacingConfig::validate_tranche_preconditions` (`core/src/config.rs`) refuses `tranche_enabled = true`
and names every unmet precondition at once. It is reached from `validate()`, so it gates `load`,
`load_with_env`, the `main.rs:181` startup path, and every SIGHUP reload — following the existing
`execute_mode == "live"` precedent.

Supporting changes:

- `TRANCHE_EXECUTION_SUPPORTED: bool = false` — a **code** constant, not a config field, so arming the
  path requires a source change reviewed alongside the capabilities it asserts. Flipping it alone does
  not open the gate; the relay/chain and bound checks still have to pass.
- `CHIMERA_TRANCHE_ENABLED` now parses via the erroring `override_parse!` macro. It previously used
  `v.parse::<bool>().unwrap_or(false)`, so `=1`, `=TRUE` and `=yes` silently resolved to `false` —
  an operator's intent disagreeing with the config in silence.
- `default_flashbots_relay()` returns an empty string. There is no correct default for Base, and a
  wrong-chain default reads as a vetted venue choice to whoever finds it next.
- CI now forbids committed `tranche_enabled: true` and any committed L1 Flashbots endpoint, mirroring
  the existing `shadow-guard` job.
- Six tests, including that the wrong-chain relay is named in the error and that `=1` fails loudly.

**Why this mattered more than it appears.** The module's own docstring claims
`CHIMERA_TRANCHE_ENABLED` "must be `true` before any bundles are submitted"
(`tranche_arbitrage.rs:16-17`). That invariant was enforced by nothing: no code outside `config.rs`
read the field, `submit_bundle` takes no config, and `TrancheBundler` never receives a `PacingConfig`.
The only thing preventing a bundle POST was that `submit_bundle` had no callers — an accident of
incompleteness, not a control. Wiring the module in "behind the disabled flag," as the audit
prescribed, would have made a reachable path whose gate nothing consulted.

---

## 6. What was deliberately not built

- **A venue comparison memo.** The question is settled negative twice over (§2). A memo would
  re-litigate it and frame venue as the blocker when the blocker is the absence of an edge.
- **End-to-end wiring behind the disabled flag.** The most dangerous item on the audit's list. It
  converts a structurally unreachable leaf into a reachable path, on a stack where the operator's
  live environment file is one line from live mode and is loaded wholesale by a systemd unit. If
  wiring ever happens, the guard must land first and must be the thing that fails the build.
- **Collateral inventory provisioning.** `contracts/src/FundDistributor.sol` is ETH-only (87 lines, no
  ERC20 import) — the audit is right. But building ERC20 staging commits real capital to inventory for
  a round trip whose gross edge is zero. Do not fund a position for a modelled loss.
- **The four documentation rewrites** (`architecture.md`, `threat-model.md`, `operator-manual.md`,
  `first-run-onboarding.md`). These would enshrine a thesis that did not survive scrutiny and would
  *destroy* correct statements those files make today: `architecture.md:212` "Atomic single-tx (not
  Flashbots bundle)", `:221` "Private submission is not currently wired", and the open MEV/RPC-exposure
  entries at `threat-model.md:170, 199-202, 216`. They describe the code accurately. Leave them.

---

## 7. Corrections needed regardless of the decision

`docs/tranche-strategy.md` should be retracted or corrected. If corrected, at minimum:

- **Price direction** (`:53-54`) is backwards, so the payoff formula (`:67`) describes a loss.
- **Flashbots on Base** (`:133-144`) is false on three counts: no relay serves 8453, Base has one
  operator-run builder rather than a market, and there is no public mempool to bypass.
- **Atomicity** (`:70-74`, `:194-196`) asserts a guarantee from a venue that does not serve Base.
- **Mempool monitoring** (`:36`) — OP Stack has no public mempool, a Protect relay is not a source of
  third-party pending transactions, and this repo has no mempool subscription at all (only `newHeads`,
  `mempool/watcher.rs:51`; `WatchEvent::NewTransaction` carries a hash only, while
  `decode_target_tx` needs `to` plus calldata).
- **Claimed wiring that does not exist** — assembler pre/post legs (`:216-219`),
  `RpcSubmitter → eth_sendBundle` (`:225`), breaker integration (`:169`). One claim in this section *is*
  now true and should be kept: `poll_receipt` really is wired into the live path
  (`orchestrator.rs:967`), booking reverts as zero-profit-plus-gas with `reverted=true`.
- **Metrics and dashboard** (`:249-262`) — `chimera_tranche_bundles_submitted_total` does not exist
  (registered name is `chimera_tranche_bundles_total`); `chimera_tranche_remuneration_usd_total` does
  not exist in any form; the five metric recorders at `metrics.rs:341-363` have **zero call sites**, so
  the gauges are permanent zeros and the counters emit no series at all; the documented Prometheus port
  9101 is wrong — `main.rs:166-168` adds `chain_id % 1000`, giving 9554.
- **Close factor** (`:84-88`) tabulates a flat 50% for `0.95 < HF < 1.0`, but the code interpolates
  50%→100% (`liquidation.rs:529-541`) and its own test asserts 90% at HF 0.96. The doc's prose and its
  table contradict each other, and `README.md:286-288` sides with the prose while
  `architecture.md:142` sides with the table. Unresolved, and worth care:
  `docs/research/aave-v3-liquidation-compendium.md:20-28` describes upstream Aave as a two-value
  default/max scheme, which points at the **code** as the divergent one. No Aave Solidity is vendored,
  so this cannot be closed from repo contents.

Separately, `README.md` now contradicts itself: `:126` writes off `Executor.yul` and the mempool
watcher as "latency machinery for a race that doesn't exist" while `:151-157` names `Executor.yul`
among three "independently sound" components the new strategy leverages; and `:192` keeps "Aave V3
Base" on the "dead list — do not re-research these" while `:5-6` targets exactly that. PR #26's only
edit to `docs/gate0-survey-2026-07-26.md` made the negative result *worse* (97% → 99% evaporation) and
retracted nothing.

---

## 8. Two audit claims that are false

Recorded so they are not carried forward:

- **Dependency drift between `alloy` and `ethers`.** There is no `ethers` anywhere — zero hits across
  `*.rs`, `*.toml` and `Cargo.lock`. Only stale prose at `CHANGELOG.md:332`. `alloy 1.8.3` only;
  `reqwest`, `hex` and `serde_json` are all properly declared.
- **Build health.** `cargo check --all-targets` is clean with zero warnings. Note that the absence of
  `dead_code` warnings is *not* evidence about the dangling module — `pub mod` plus `pub use` makes
  that lint structurally inapplicable, which is why the call-site census in §4 is the real evidence.

PR #26 did merge with 14 hunks of rustfmt drift, so `cargo fmt --check` was failing on `main` before
this branch. Fixed here.

---

## 9. Open items for the operator

1. **Decide the fork in §4.** Withdraw is the recommendation. This is the one item that needs a human.
2. **`.env.live` holds cleartext secrets** — a real `CHIMERA_KEYSTORE_PASSWORD` and
   `CHIMERA_OPERATOR_TOKEN` (`:26-27`) and live API keys embedded in RPC URLs (`:11-12`). It is
   gitignored, but `deploy/chimera.service:10` loads it wholesale via `EnvironmentFile` with
   `Restart=on-failure`. Rotate these. Unrelated to the tranche work and higher priority than it.
3. **Remove the six `CHIMERA_TRANCHE_*` / `CHIMERA_FLASHBOTS_RELAY` lines from `.env.live`** (`:43-55`).
   Inert today, but their presence signals that the venue was chosen and blessed when it was neither.
   Left in place rather than edited here: it is an untracked operator file containing live secrets.
4. **`deploy/chimera.service`** puts `StartLimitBurst` and `StartLimitIntervalSec` in `[Service]`,
   where systemd ignores them (they moved to `[Unit]` in v229). As committed, `Restart=on-failure` with
   `RestartSec=5` loops with no effective limit on a unit whose `EnvironmentFile` can carry live mode.
5. **If a falsification test is still wanted,** the cheapest form is an offline constant-product model
   — no chain access, no REVM work, no relay. Scope it explicitly as a kill test rather than as
   strategy enablement, because it returns a loss on every input.
6. **The genuinely positive lead from this review:** multi-venue split routing on the *existing*
   liquidation swap leg. Modelled at +146.90 against +71.81 for create-then-arb, with zero extra
   transactions and zero extra fee legs — strictly better than the tranche structure and available
   without any new venue. Worth its own work order. Keep the close-factor precision fix from PR #26
   too; it is independently correct.
