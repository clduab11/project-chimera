#!/usr/bin/env python3
"""Pre-flight QA gate for config/snapshot.json.

Stdlib only (no web3), importable offline, and fail-closed: a non-zero exit
means the snapshot is NOT safe to drive live execution from.

The snapshot is the engine's entire view of the market. Everything downstream —
health factors, close factors, seizure sizing, profit — is derived from it, so a
silently wrong snapshot produces confidently wrong money decisions rather than
an error. This script exists to make that class of failure loud and early.

Checks are grouped by severity:
  ERROR  — blocks live execution
  WARN   — degrades profitability or coverage, does not risk loss
  INFO   — observations worth knowing before a run

Usage:
    python scripts/validate_snapshot.py
    python scripts/validate_snapshot.py --snapshot config/snapshot.json \
        --routing config/routing.yaml --max-age-hours 24 --strict
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
from pathlib import Path

# Verified against the official Aave address book (aave-dao/aave-address-book,
# src/AaveV3Base.sol) — the Pool a Base-mainnet snapshot must reference.
AAVE_V3_BASE_POOL = "0xA238Dd80C259a72e81d7e4664a9801593F98d1c5"
BASE_CHAIN_ID = 8453

# Aave V3 constants relevant to sizing. CLOSE_FACTOR_HF_THRESHOLD is 0.95;
# MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD (v3.3+) is 2_000e8 in base-currency units.
CLOSE_FACTOR_HF_THRESHOLD = 0.95
MIN_BASE_MAX_CLOSE_FACTOR_USD = 2_000

RESERVE_REQUIRED = [
    "address", "symbol", "decimals", "ltv", "liquidation_threshold",
    "liquidation_bonus", "price_usd", "a_token", "variable_debt_token",
    "active", "frozen", "paused", "liquidity_index", "variable_borrow_index",
    "total_variable_debt", "last_update_timestamp",
]
USER_REQUIRED = ["collateral", "debt"]

ADDR_RE = re.compile(r"^0x[0-9a-fA-F]{40}$")
ZERO_ADDR = "0x" + "0" * 40


class Report:
    def __init__(self) -> None:
        self.errors: list[str] = []
        self.warns: list[str] = []
        self.infos: list[str] = []

    def error(self, msg: str) -> None:
        self.errors.append(msg)

    def warn(self, msg: str) -> None:
        self.warns.append(msg)

    def info(self, msg: str) -> None:
        self.infos.append(msg)

    def emit(self, strict: bool) -> int:
        for m in self.infos:
            print(f"  INFO   {m}")
        for m in self.warns:
            print(f"  WARN   {m}")
        for m in self.errors:
            print(f"  ERROR  {m}")
        print()
        print(f"  {len(self.errors)} error(s), {len(self.warns)} warning(s)")
        if self.errors:
            print("  VERDICT: snapshot is NOT safe for live execution")
            return 1
        if strict and self.warns:
            print("  VERDICT: clean, but --strict treats warnings as failures")
            return 1
        print("  VERDICT: snapshot passes")
        return 0


def _int(value) -> int | None:
    """Snapshot integer fields arrive as int or decimal string; accept both.

    Deliberately rejects floats rather than truncating: the bps and raw-unit
    fields this is used for are integers by definition, so a float there is a
    generator bug worth surfacing, not something to silently round.
    """
    if isinstance(value, bool) or isinstance(value, float):
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def _num(value) -> float | None:
    """Parse a numeric field that is legitimately fractional (USD prices).

    `prewarm::ReserveData.price_usd` is an `f64` denominated in whole USD, so
    stablecoins land just under 1.0 — using integer parsing here would read
    $0.9998 as zero and condemn every USDC position.
    """
    if isinstance(value, bool):
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def check_header(snap: dict, rep: Report, max_age_hours: float) -> None:
    pool = snap.get("pool", "")
    if pool.lower() != AAVE_V3_BASE_POOL.lower():
        rep.error(
            f"pool {pool!r} is not the Aave V3 Base Pool {AAVE_V3_BASE_POOL} — "
            "every position read is against the wrong contract"
        )

    block = _int(snap.get("block_number"))
    if not block:
        rep.error("block_number missing or unparseable")

    ts = _int(snap.get("timestamp"))
    if ts is None:
        rep.error("timestamp missing or unparseable")
    else:
        age_h = (time.time() - ts) / 3600.0
        if age_h > max_age_hours:
            rep.error(
                f"snapshot is {age_h:.1f}h old (limit {max_age_hours}h) — "
                "positions and prices have almost certainly moved"
            )
        elif age_h > max_age_hours / 2:
            rep.warn(f"snapshot is {age_h:.1f}h old; regenerate before a live run")
        else:
            rep.info(f"snapshot age {age_h:.1f}h at block {block}")


def check_reserves(snap: dict, rep: Report) -> dict:
    reserves = snap.get("reserves") or []
    if not reserves:
        rep.error("no reserves — the engine cannot price anything")
        return {}

    by_addr: dict[str, dict] = {}
    for i, r in enumerate(reserves):
        label = r.get("symbol") or f"reserve[{i}]"

        missing = [f for f in RESERVE_REQUIRED if f not in r]
        if missing:
            rep.error(f"{label}: missing field(s) {', '.join(missing)}")
            continue

        addr = r["address"]
        if not ADDR_RE.match(addr):
            rep.error(f"{label}: malformed address {addr!r}")
            continue
        if addr.lower() in by_addr:
            rep.error(f"{label}: duplicate reserve address {addr}")
            continue
        by_addr[addr.lower()] = r

        dec = _int(r["decimals"])
        if dec is None or not (0 <= dec <= 18):
            rep.error(f"{label}: implausible decimals {r['decimals']!r}")

        ltv = _int(r["ltv"]) or 0
        lt = _int(r["liquidation_threshold"]) or 0
        bonus = _int(r["liquidation_bonus"]) or 0

        if lt and ltv and lt < ltv:
            rep.error(
                f"{label}: liquidation_threshold {lt} < ltv {ltv} — "
                "positions would be liquidatable the moment they are opened"
            )
        if lt > 10_000 or ltv > 10_000:
            rep.error(f"{label}: ltv/threshold above 100% (ltv={ltv}, lt={lt})")

        # A bonus at or below 1.0x means seizing collateral yields no premium.
        if bonus and bonus <= 10_000:
            rep.error(f"{label}: liquidation_bonus {bonus} <= 10000 (no premium)")

        price = _num(r["price_usd"])
        active = bool(r.get("active"))
        paused = bool(r.get("paused"))
        if active and not paused:
            if price is None:
                rep.error(f"{label}: price_usd {r['price_usd']!r} is not numeric")
            elif price <= 0:
                rep.error(f"{label}: active reserve has zero price — HF math will be wrong")
            elif price != price or price in (float("inf"), float("-inf")):
                rep.error(f"{label}: price_usd is NaN/inf")

        # Prewarm needs real token addresses to seed balance slots.
        for tok in ("a_token", "variable_debt_token"):
            if r[tok] == ZERO_ADDR:
                rep.error(
                    f"{label}: {tok} is the zero address — prewarm falls back to "
                    "price-only mode, which must not drive live simulation"
                )

        if paused:
            rep.warn(f"{label}: reserve is paused; liquidations will revert")
        if bonus == 0 and lt == 0:
            rep.info(f"{label}: not seizable (zero bonus and threshold) — excluded from coverage")

    return by_addr


def check_users(snap: dict, reserves: dict, rep: Report) -> set:
    users = snap.get("users") or {}
    if not users:
        rep.error("no users — the engine has nothing to watch and cannot earn")
        return set()

    if len(users) < 100:
        rep.warn(
            f"only {len(users)} positions tracked; Aave V3 Base has far more. "
            "Coverage this thin will rarely surface a profitable liquidation"
        )

    collateral_assets: set[str] = set()
    borrowers = 0

    for addr, u in users.items():
        if not ADDR_RE.match(addr):
            rep.error(f"user key {addr!r} is not an address")
            continue
        missing = [f for f in USER_REQUIRED if f not in u]
        if missing:
            rep.error(f"user {addr}: missing {', '.join(missing)}")
            continue

        debt = u["debt"] or {}
        coll = u["collateral"] or {}
        if debt:
            borrowers += 1

        for asset, amt in list(coll.items()) + list(debt.items()):
            if asset.lower() not in reserves:
                rep.error(
                    f"user {addr}: references asset {asset} with no matching reserve — "
                    "health factor cannot be computed"
                )
            if _int(amt) is None:
                rep.error(f"user {addr}: unparseable amount {amt!r} for {asset}")

        collateral_assets.update(a.lower() for a in coll)

        if debt and not coll:
            rep.warn(f"user {addr}: debt with no collateral (bad debt) — not liquidatable for profit")

    rep.info(f"{len(users)} positions, {borrowers} with debt")
    return collateral_assets


def check_route_coverage(routing_path: Path, reserves: dict, held: set, rep: Report) -> None:
    """Cross-check seizable collateral against venues the engine can actually use.

    Deliberately a text scan, not a YAML parse: this script must stay
    dependency-free, and we only need the executable venue pairs.
    """
    if not routing_path.exists():
        rep.warn(f"{routing_path} not found; skipping exit-route coverage")
        return

    text = routing_path.read_text(encoding="utf-8")
    # Only "v2" venues are executable by the current Executor.
    v2_pairs: set[tuple[str, str]] = set()
    blocks = re.split(r"\n\s*-\s+name:", text)
    for b in blocks:
        if 'router_compatibility: "v2"' not in b:
            continue
        if "chain: \"base\"" not in b:
            continue
        toks = re.findall(r"token_(?:in|out):\s*\"(0x[0-9a-fA-F]{40})\"", b)
        for i in range(0, len(toks) - 1, 2):
            v2_pairs.add((toks[i].lower(), toks[i + 1].lower()))

    seizable = {
        a for a, r in reserves.items()
        if (_int(r.get("liquidation_bonus")) or 0) > 10_000
    }
    routable = {t for pair in v2_pairs for t in pair}

    # Same-asset liquidations need no route at all (Executor skips the swap leg).
    uncovered = sorted(
        reserves[a].get("symbol", a) for a in seizable
        if a not in routable
    )
    if uncovered:
        rep.warn(
            f"{len(uncovered)}/{len(seizable)} seizable collaterals have no executable "
            f"V2 exit route: {', '.join(uncovered)}. Same-asset liquidations still "
            "work (no swap needed); everything else needs the swap engine"
        )
    held_uncovered = sorted(
        reserves[a].get("symbol", a) for a in held
        if a in seizable and a not in routable
    )
    if held_uncovered:
        rep.info(
            "collateral actually held by tracked users but unroutable: "
            + ", ".join(held_uncovered)
        )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--snapshot", default="config/snapshot.json", type=Path)
    ap.add_argument("--routing", default="config/routing.yaml", type=Path)
    ap.add_argument("--max-age-hours", default=24.0, type=float)
    ap.add_argument("--strict", action="store_true", help="treat warnings as failures")
    args = ap.parse_args()

    print(f"\n  Snapshot QA — {args.snapshot}\n")

    if not args.snapshot.exists():
        print(f"  ERROR  {args.snapshot} does not exist")
        return 1
    try:
        snap = json.loads(args.snapshot.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        print(f"  ERROR  {args.snapshot} is not valid JSON: {e}")
        return 1

    rep = Report()
    check_header(snap, rep, args.max_age_hours)
    reserves = check_reserves(snap, rep)
    held = check_users(snap, reserves, rep) if reserves else set()
    if reserves:
        check_route_coverage(args.routing, reserves, held, rep)

    return rep.emit(args.strict)


if __name__ == "__main__":
    sys.exit(main())
