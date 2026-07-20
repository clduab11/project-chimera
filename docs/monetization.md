# Monetization — Lawful Revenue Paths for Project Chimera

**Status**: evaluation and staged plan. No financial transaction, paid service, or
account creation has been initiated. Every stage below requires explicit operator
approval before activation.

> **Scope.** This document covers only lawful, ethical, human-approved revenue
> paths. It exists partly in response to external research transcripts
> (`chimera-expansion-I.md` / `chimera-expansion-II.md`, kept out of the repo)
> that surveyed "self-funding AI agents." The follow-up thread in that export
> (fraud-oriented "blackhat/grey-market" operation with liability shifted to a
> third party) is **rejected in full** — see [§5](#5-rejected-approaches).

---

## 1. Posture

1. Chimera's own capital and Chimera-executed protocol incentives only. Chimera
   never takes custody of third-party funds, never solicits investment, and
   never sells financial advice.
2. Human approval gates remain under operator control. The existing controls
   (shadow-first mode, multisig-owned Executor, pacing caps, breaker, operator
   token for `clear_breaker`) stay in force regardless of revenue pressure.
   The 7-day soak was de-listed by explicit operator decision on 2026-07-20.
3. Revenue never justifies weakening a guardrail. Any change to caps or gates
   requires a design doc and the validation gate in AGENTS.md.

---

## 2. Path A — Aave V3 liquidation incentives (primary; already built)

**Mechanism.** Permissionless liquidations of underwater Aave V3 positions on
Base/Arbitrum, executed atomically via flash loan through the multisig-owned
`Executor.yul`. Profit = protocol liquidation bonus minus gas, slippage, and
protocol fees. Capital-light: Aave supplies liquidation capital; operator funds
gas only. This is a protocol-sanctioned, economically useful function (it
de-risks the lending pool). See README "How It Earns".

**Why it is lawful and ethical.** Liquidation is an open, published incentive in
the Aave V3 protocol; any address may perform it. Chimera competes on execution
quality, not on exploiting counterparties: it does not manipulate oracles, does
not force positions underwater, and does not extract value from anything but the
protocol's own bonus.

**Status and gates (unchanged from README/AGENTS.md):**

| Gate | State |
|------|-------|
| Full toolchain validation (`cargo test`, `forge test`, slither) | cargo test green (199 passed, 2026-07-20); forge/slither pending on a workstation |
| 7-day shadow soak (`toggle_shadow.py`) | **de-listed by operator decision 2026-07-20** |
| Encrypted keystore + multisig-owned deployment | required before live |
| Protected/private tx submission | **not wired** — recommended before real-money operation |
| Pacing caps / breakers / emergency pause | code-enforced, must remain |

**Revenue expectations.** Honest assessment: liquidation MEV on major L2s is
highly competitive. Profit depends on gas efficiency, inclusion speed, and
market volatility. The 2.5x profit-over-gas hurdle and daily/weekly caps mean
Chimera is deliberately a *conservative* participant. Treat early live revenue
as offsetting infrastructure cost, not as guaranteed income.

---

## 3. Path B — Whitehat audit bounties (auxiliary; scaffolded)

**Mechanism.** `ai-audit/` monitors L2 contract-creation events, runs Slither +
a local model over verified sources, and emits **draft** reports into
`ai-audit/queue/`. A human reviews drafts and submits valid findings to formal
bug-bounty programs (e.g., Immunefi) **only where a live program explicitly
covers the target contract**.

**Rules (hard constraints):**

1. **No mainnet exploitation, ever.** Findings are proven on local forks/tests
   only. Any PoC that touches live user funds converts this from lawful
   whitehat work into a crime.
2. **Scope-locked.** Only contracts inside an active, public bounty program's
   stated scope are eligible for submission.
3. **Human-in-the-loop.** Reports leave the queue only after operator review;
   the pipeline never auto-submits.
4. **No responsible-disclosure violations.** Follow each program's disclosure
   policy exactly; no public posting before resolution.

**Status.** Auxiliary and manual today; not connected to the engine's funds
path. Reasonable target: occasional bounty income; not a reliable revenue base.

---

## 4. Path C — Paid data services (future; optional)

The snapshot/health-factor pipeline (`scripts/snapshot_generator.py`, oracle
layer, prewarm cache) produces data other operators pay for. A later phase
*could* expose a small paid API (HTTP-native micropayments, e.g., an
x402-style scheme) serving pool snapshots or health-factor watchlists.

**Preconditions:** Path A live and stable; documented demand; separate revenue
wallet; accounting export; legal review of the payment flow. **Do not build
before Path A is proven** — it splits operator attention from the funds path.

---

## 5. Rejected approaches

The following appear in the omitted thread of the source export and in common
"autonomous money-making" pitches. They are rejected categorically — not
deferred, not gated, rejected:

- Any fraud, deception, or misrepresentation toward users, markets, or auditors.
- Structuring an operation so a third party ("patsy") absorbs legal liability —
  this is itself evidence of fraudulent intent and does not transfer criminal
  liability.
- Oracle manipulation, forced liquidations, governance attacks, sandwiching or
  other predatory MEV, exploit execution, or unaudited flash-loan strategies.
- Custody or pooling of third-party funds, yield products, or investment
  contracts without licensing.
- Market manipulation of any token, NFT wash-trading, or memecoin promotion
  schemes.
- Removing or weakening human oversight, multisig custody, pacing caps, or the
  breaker to increase revenue.

Engineering support for any of the above is out of scope for this project.

---

## 6. Compliance checklist (operator homework before Path A live)

| Area | Action |
|------|--------|
| Tax | Liquidation profit is likely taxable income/property gain in most jurisdictions. The JSONL outcome log (`core/state/outcomes.jsonl`) plus Executor withdrawal records form the audit trail — export and reconcile with an accountant. |
| Sanctions/KYT | Screen interacted pool/counterparty exposure policy with counsel; consider a screening tool if volume justifies it. |
| Money transmission | Operating solely with own capital via protocol incentives is generally *not* money transmission, but confirm per jurisdiction. |
| Entity/liability | Operate through a legal entity chosen with counsel; the multisig owner should map to that entity's governance. |
| Insurance/risk | Size the gas treasury so the 0.005 ETH daily-loss breaker caps worst-case spend to an affordable number. |
| Record-keeping | Keep `outcomes.jsonl`, Grafana snapshots, and incident logs per `docs/incident-log.md`. |

This document is not legal or tax advice; it enumerates what to review with
qualified counsel before live operation.

---

## 7. Staged plan with human-approval gates

| Stage | Content | Approval gate to advance |
|-------|---------|--------------------------|
| 0 | Policy: this document + owner sign-off on compliance checklist | Operator sign-off |
| 1 | Validation gate on workstation: `forge test`, slither, full CI green | Operator reviews gate report |
| 2 | Testnet practice (Base Sepolia) per `docs/runbook-testnet-deploy.md` | Operator reviews testnet results |
| 3 | Mainnet shadow rehearsal per `docs/runbook-7day-soak.md` (mandatory soak de-listed 2026-07-20) | Operator decision |
| 4 | Live with tiny gas treasury, encrypted keystore, multisig-owned Executor, protected submission wired | Explicit go-live decision per `docs/runbook-keystore-multisig-go-live.md` |
| 5 | Operate within caps; weekly review of PnL, breaker events, inclusion rate | Continuing operator review |
| 6 | (Optional) Path B bounty submissions as findings arise | Per-report human review |
| 7 | (Optional, later) Path C paid API scoping | Separate design doc + legal review |

---

## 8. What this project will not do

No autonomous "AI agent with a wallet" features are planned. Chimera is a
deterministic engine with human custody and hard caps. The agentic-commerce
patterns in the research export (x402, agent wallets, prediction-market agents)
are noted here only as context for Path C; they are not a roadmap commitment and
would each require their own design doc, risk assessment, and legal review.
