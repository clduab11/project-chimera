# Gate-0 survey — decision rule, fixed BEFORE any survey number was produced

**Written:** 2026-07-26, before Tier A–E results existed. This file is the
pre-commitment demanded by the work order: the threshold is declared here so it
cannot be fitted to the answer.

## The rule

- **If ≥ $100,000 / 30d of `bonus_exit`** (exit-denominated, post-oracle-premium
  bonus, NOT `bonus_oracle`) **survives Gate-0 across Tiers A–D combined** —
  i.e. sits in markets whose Gate-0 verdict is PASS (or REVIEW_PT judged
  favorable on discount-vs-maturity) — then: report it, name the venues,
  recommend re-opening the strategy with a fresh brief. **Do not build.**

- **If < $100,000 / 30d survives** — state plainly that the interest-accrual
  niche and the liquidation category as a whole are dead for this operator, and
  that the answer cost ~1.5 days instead of a quarter.

- **Tier E is reported separately and is NOT scored against this threshold** —
  it may surface something that is not a liquidation strategy at all.

## Definitions bound to this rule

- `bonus_exit` per market = Σ over 30d events of `repaid_usd × (LIF/(1+premium) − 1)`,
  premium = current `oraclePrice/tradedPrice − 1` with tradedPrice from an
  aggregator quote at the market's median clip (DefiLlama as fallback).
- Gate-0 PASS requires: same-chain aggregator-routable exit of ≥3× median clip
  at <2% price impact, AND |oracle premium| ≤ 1%, AND exit_type ∈ {DEX}.
  PT_REDEEMABLE markets are judged on discount-vs-time-to-maturity instead of
  DEX depth. BRIDGE_ONLY and NONE are automatic fails.
- SVR-gated venues (Aave/Compound/Venus behind DualAggregator) contribute $0
  regardless of their bonus numbers — the trigger is auctioned before it is
  visible.
