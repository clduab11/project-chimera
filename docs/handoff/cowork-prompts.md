# Claude Cowork prompts — project-chimera handoff

Two self-contained prompts. Run them in Cowork with the Notion connector enabled.
They are independent — run Prompt 1 first (it is short), then Prompt 2.

Both are written to **confirm the destination before writing anything**. Notion
page creation is hard to undo cleanly, and neither prompt should guess where
things belong.

> **Before you run these:** replace `Sharona` below if that is not the exact name
> of the person or Notion space you want updated. Prompt 1 asks Cowork to search
> for it, but an exact name makes it one step instead of three.

---

## Prompt 1 — Status update: project-chimera is archived

```
I'm archiving a project and need Notion updated so the record is accurate.

FIRST: search Notion for "Sharona" and for "project-chimera" (also try "chimera").
Tell me what you find — a person, a page, a database, a project tracker — and ask
me where this update belongs before creating or editing anything. Do not create a
page until I confirm the destination.

Then write a status update with this content. Keep it factual and non-dramatic;
this is a project post-mortem, not a pitch.

---

**Project Chimera — ARCHIVED 2026-07-26**

**What it was:** A Rust + Yul atomic flash-loan execution engine for on-chain
lending liquidations and MEV, targeting Aave V3 on Base. The goal was to earn
recurring revenue comparable to a prior retail-forex bot (oanda-autobot).

**Outcome:** Archived with zero revenue. The engine reached production quality —
318 passing tests, a Yul contract deployed to Base mainnet, full risk/pacing
controls, REVM fork simulation. It ran live for six days and executed zero
liquidations. Direct measurement spend was roughly $350 plus substantial labor.

**Why it failed — one reason, confirmed five times independently:**

In crypto market-microstructure niches, the surplus is captured by whoever holds
one of exactly three things:
1. Exclusive or private order flow
2. The block-building or sequencing seat
3. A colocation and private-feed budget

None of those three is code. Code quality was this project's only asset, and no
venue was found where code quality was the binding constraint.

**The five investigations, all negative:**

| # | Investigation | Result |
|---|---|---|
| 1 | Base Aave V3 liquidations | $4,404/30d for the ENTIRE market; top incumbent takes 85.4%; ~$650/mo residual. Settled by a Chainlink SVR sealed-bid oracle auction, not a latency race — unfixable with a faster node. |
| 2 | Interest-accrual niche | Claimed $283.6k/30d collapsed to negative once re-denominated at achievable exit prices. The oracle premium that read as "bonus" actually reduced realisable value. |
| 3 | Landscape survey | Whole landscape ≈$69–79k/30d against a pre-committed $100k threshold. Every venue had an 85–99.7% incumbent. |
| 4 | "Bespoke Tranche" three-leg bundle | No profit source. You cannot profit by sandwiching your own transaction — it nets exactly zero gross and is negative after fees. Proven analytically and by a 6,125-parameter sweep. |
| 5 | Cross-chain survey (8 chains) | Every chain CLOSED or MARGINAL for a new entrant. Best case Polygon at ~$17k/mo residual against a 3–4 month build. |

**What was done right, and is worth carrying forward:**
- A decision threshold was written down and committed BEFORE the data was
  collected, so it could not be fitted to the answer.
- Strategies were killed with measured numbers rather than abandoned on vibes.
- Negative results were published in the repo rather than quietly dropped.
- Adversarial self-review caught two errors in the project's own analysis.

**The transferable asset:** a stdlib-only Python measurement toolchain that can
size any lending venue's liquidation flow — total value, incumbent
concentration, and whether the venue is auction-closed — in about a day for
under $100. That inversion (measure before building, not after) is the whole
lesson.

**Status of funds/keys:** the repo's README carries a wind-down checklist —
rotate the API keys and keystore password in .env.live, and sweep any remaining
balance from the Executor contract and treasury.

---

After I confirm the destination, create the update there. If there's an existing
project-chimera page, update its status to Archived rather than making a
duplicate, and add this as the closing entry.
```

---

## Prompt 2 — Build the "Crypto Corner" reference

```
I want to build a durable reference resource in Notion called "Crypto Corner".

BACKGROUND: I just archived a crypto MEV project that returned zero revenue
after months of work. The reference exists so that if I ever consider a
crypto project again, I start from hard-won structural knowledge instead of
rediscovering it. It needs to be something I can extend with Notion AI over
time.

FIRST: search Notion for an existing "Crypto Corner" page and ask me where this
should live before creating anything. Do not create the page until I confirm.

CONTENT: I have the full reference written already — it is attached / pasted
below. Do not rewrite or summarise it; it is dense on purpose and every number
in it was measured or verified. Your job is to get it into Notion well-structured.

STRUCTURE IT AS:
- A top-level page "Crypto Corner" with the front matter and the one-paragraph
  core lesson prominently near the top (use a callout block).
- Each "## " heading becomes its own SUB-PAGE, not just a heading — I want to be
  able to open "Chains", "DEX architectures", etc. independently and extend each
  one separately with Notion AI.
- Keep all tables as real Notion tables, not code blocks.
- Keep code snippets, addresses, and function selectors in code blocks so they
  stay copy-pasteable and don't get autocorrected.
- On the parent page, add a linked table of contents to the sub-pages.
- Add a "Last reviewed" date property set to today, and a note that facts are
  current as of July 2026 and this space moves fast.

THEN: at the end, add an empty sub-page called "Open questions & things to
re-verify" with a few starter bullets for anything in the document I flagged as
unmeasured, fast-moving, or worth re-checking before acting on.

Tell me what you created and where, with links.
```

---

## The content for Prompt 2

The reference document lives at **`docs/handoff/crypto-corner.md`** in this repo.
Paste its full contents into Cowork along with Prompt 2, or attach the file.

It covers, in order:

1. **Market structure first** — the framework for evaluating any crypto
   opportunity before writing code. Read this one before starting anything.
2. **Project Chimera post-mortem** — what happened and the rules that came out.
3. **Chains** — structural cheat sheet: Ethereum, Base, Arbitrum, Polygon,
   Avalanche, Solana, BNB Chain, Optimism.
4. **DEX architectures** — Uniswap V2/V3/V4, Aerodrome/Solidly, Curve, Balancer,
   Solana DEXs, and the integration gotchas.
5. **Smart contracts** — patterns, hazards, flash-loan economics, callback
   security, and when to write your own.
6. **Lending and liquidations** — Aave V3, Morpho Blue, Compound III, and why
   liquidations stopped being a latency race.
7. **MEV** — the honest map, including which strategies are foreclosed and why.
8. **Exchanges, custody, and operations** — stablecoins, bridging, key
   management, RPC providers, monitoring, record-keeping.
