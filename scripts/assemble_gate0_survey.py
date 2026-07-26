#!/usr/bin/env python3
"""
assemble_gate0_survey.py
Project Chimera - merge all Gate-0 tier outputs into config/gate0_survey.json
(one row per market/venue) and print the decision summary against the
pre-declared threshold in docs/gate0-decision-rule.md.

Usage:
  python scripts/assemble_gate0_survey.py --scratch <scratchpad dir> --out config/gate0_survey.json
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

FIELDS = ["chain", "protocol", "venue", "market_id", "pair", "collateral", "debt",
          "open_or_gated", "exit_type", "routable_at_2pct", "oracle_premium_pct",
          "bonus_oracle", "bonus_exit", "gate0_pass", "borrow_usd", "notes", "tier"]


def norm(row: dict, tier: str) -> dict:
    out = {k: row.get(k) for k in FIELDS}
    out["tier"] = tier
    if isinstance(out["notes"], list):
        out["notes"] = "; ".join(str(n) for n in out["notes"])
    return out


def load_gate0(path: Path, tier: str) -> list[dict]:
    if not path.exists():
        return []
    data = json.loads(path.read_text(encoding="utf-8"))
    rows = data["rows"] if isinstance(data, dict) else data
    return [norm(r, tier) for r in rows]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--out", default="config/gate0_survey.json")
    args = ap.parse_args()
    S = Path(args.scratch)

    rows: list[dict] = []
    # Tier A: ethereum + base Morpho gate0 sweeps
    rows += load_gate0(S / "tier_a" / "gate0_ethereum_morpho.json", "A")
    rows += load_gate0(S / "tier_a" / "gate0_base_morpho.json", "A")
    # Tier A: markets skipped below the borrow floor (recorded, not gated)
    enum_p = S / "tier_a" / "morpho_markets_ethereum.json"
    if enum_p.exists():
        en = json.loads(enum_p.read_text(encoding="utf-8"))
        for r in en["rows"]:
            if "gate0_skipped" in r:
                rows.append({**{k: r.get(k) for k in FIELDS},
                             "gate0_pass": "SKIPPED",
                             "bonus_oracle": r.get("bonus_oracle_usd", 0.0),
                             "notes": r["gate0_skipped"], "tier": "A"})
    # smoke pass covered ethereum/base Aave+Spark top collateral markets
    rows += load_gate0(S / "gate0_smoke.json", "A-control")
    # Tier B/C/D rows are venue- or protocol-level
    for tier, name in (("B", "tier_b/tier_b_rows.json"),
                       ("C", "tier_c/tier_c_rows.json"),
                       ("D", "tier_d/tier_d_rows.json")):
        p = S / name
        if p.exists():
            data = json.loads(p.read_text(encoding="utf-8"))
            if isinstance(data, dict):
                data = data.get("rows", [])
            for r in data:
                rows.append({**{k: r.get(k) for k in FIELDS}, "tier": tier,
                             "notes": (r.get("notes") if isinstance(r.get("notes"), str)
                                       else json.dumps(r.get("notes"))[:400]),
                             "bonus_oracle": r.get("bonus_oracle")
                             or r.get("bonus_oracle_30d") or r.get("bonus_oracle_usd"),
                             "bonus_exit": r.get("bonus_exit")
                             or r.get("bonus_exit_30d") or r.get("bonus_exit_usd"),
                             "venue": r.get("venue") or r.get("protocol"),
                             "open_or_gated": r.get("open_or_gated")})
        else:
            rows.append({"tier": tier, "gate0_pass": "MISSING",
                         "notes": f"{name} not produced"})

    # dedupe by (chain, market_id/pair-key), preferring tier A over A-control
    seen, deduped = {}, []
    for r in rows:
        key = (r.get("chain"), r.get("market_id") or f"{r.get('collateral')}/{r.get('debt')}",
               r.get("venue"))
        if key in seen:
            continue
        seen[key] = True
        deduped.append(r)

    # decision summary: OPEN venues only, PASS rows
    surviving = [r for r in deduped
                 if r.get("gate0_pass") == "PASS"
                 and (r.get("open_or_gated") or "OPEN") == "OPEN"
                 and r.get("bonus_exit")]
    pt_review = [r for r in deduped if r.get("gate0_pass") == "REVIEW_PT"]
    total_exit = sum(r["bonus_exit"] for r in surviving)
    pt_exit = sum(r["bonus_exit"] or 0 for r in pt_review)

    out = {
        "generated": datetime.now(timezone.utc).isoformat(),
        "decision_rule": "docs/gate0-decision-rule.md (fixed before survey numbers)",
        "threshold_usd_30d": 100_000,
        "surviving_bonus_exit_usd_30d": total_exit,
        "surviving_markets": len(surviving),
        "review_pt_bonus_exit_usd_30d": pt_exit,
        "rows": deduped,
    }
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(out, indent=1), encoding="utf-8")

    print(f"rows: {len(deduped)}")
    for v in ("PASS", "FAIL", "REVIEW_PT", "REVIEW_VAULT", "REVIEW", "SKIPPED",
              "ERROR", "MISSING", None):
        k = sum(1 for r in deduped if r.get("gate0_pass") == v)
        if k:
            print(f"  {str(v):<14}{k:>5}")
    print(f"\nsurviving OPEN bonus_exit (PASS rows): ${total_exit:,.0f} /30d "
          f"across {len(surviving)} markets")
    print(f"REVIEW_PT bucket (judge separately):    ${pt_exit:,.0f} /30d "
          f"across {len(pt_review)} markets")
    top = sorted(surviving, key=lambda r: -(r.get("bonus_exit") or 0))[:15]
    for r in top:
        print(f"   {r.get('pair') or r.get('venue'):<30} {str(r.get('chain')):<10}"
              f" exit=${r['bonus_exit']:>10,.0f}  premium={r.get('oracle_premium_pct')}")
    print(f"\nwrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
