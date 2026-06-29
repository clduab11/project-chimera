#!/usr/bin/env python3
"""
recover_state.py
Project Chimera - Crash-Recovery State Rebuilder

Rebuilds aggregated pacing state from the JSONL audit trail, mirroring
core/src/state/recovery.rs `CrashRecovery::recover_from_jsonl`.

Each line of core/state/outcomes.jsonl is a JSON `OutcomeRecord` (see
core/src/state/mod.rs). Parsed fields:
    id              (str)
    timestamp       (RFC3339 DateTime<Utc>)
    decision        (str)
    realized_net_usd(Decimal)   <- summed for daily/weekly windows
    gas_spent_eth   (Decimal)
    reverted        (bool)       <- drives consecutive_reverts
    venue           (str)
    eoa             (str)
    chain_id        (int)

Aggregates produced (identical semantics to the Rust impl):
    daily_usage_usd     = sum(realized_net_usd) for records with age <= 1 day
    weekly_usage_usd    = sum(realized_net_usd) for records with age <= 7 days
    consecutive_reverts = count of trailing reverted records (in file order)
    last_outcome_time   = timestamp of the LAST record in the file

Recovery semantics mirrored EXACTLY from recovery.rs:
  - Missing file                -> zeroed/clean state, exit 0.
  - A corrupted line            -> log a warning and return a ZEROED state
                                   (the Rust impl bails to zeroed state on the
                                   first unparseable line rather than partial
                                   aggregation), exit 0.

INVARIANT (AGENTS.md #5): stdlib only. No web3.py. `--help` always works.
Monetary sums use `decimal.Decimal` (Invariant #3: never f64 for money).

Usage:
  python scripts/recover_state.py
  python scripts/recover_state.py --jsonl core/state/outcomes.jsonl --json
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
from datetime import datetime, timedelta, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("recover_state")

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
DEFAULT_JSONL: str = "core/state/outcomes.jsonl"
ONE_DAY = timedelta(days=1)
SEVEN_DAYS = timedelta(days=7)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
def parse_timestamp(raw: str) -> datetime:
    """
    Parse an RFC3339 / ISO-8601 timestamp (DateTime<Utc> serialization).
    Accepts a trailing 'Z'. Always returns a timezone-aware UTC datetime.
    """
    text = raw.strip()
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    dt = datetime.fromisoformat(text)
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.astimezone(timezone.utc)


def zeroed_state(last_time: datetime | None = None) -> dict[str, Any]:
    return {
        "daily_usage_usd": Decimal("0"),
        "weekly_usage_usd": Decimal("0"),
        "consecutive_reverts": 0,
        "last_outcome_time": last_time,
    }


# ---------------------------------------------------------------------------
# Core recovery (mirrors recovery.rs)
# ---------------------------------------------------------------------------
def recover_from_jsonl(jsonl_path: str) -> dict[str, Any]:
    """Rebuild aggregated state from a JSONL audit trail."""
    path = Path(jsonl_path)
    if not path.exists():
        logger.info("No audit trail at %s; returning clean/zeroed state.", jsonl_path)
        return zeroed_state()

    records: list[dict[str, Any]] = []
    try:
        with open(path, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    records.append(json.loads(line))
                except json.JSONDecodeError as exc:
                    # Mirror recovery.rs: bail to a zeroed state on first corruption.
                    logger.warning(
                        "Corrupted JSONL line in %s: %s. Returning zeroed state.",
                        jsonl_path,
                        exc,
                    )
                    return zeroed_state()
    except OSError as exc:
        logger.error("Failed to read %s: %s", jsonl_path, exc)
        return zeroed_state()

    now = datetime.now(timezone.utc)
    daily = Decimal("0")
    weekly = Decimal("0")

    for rec in records:
        try:
            ts = parse_timestamp(str(rec["timestamp"]))
            net = Decimal(str(rec["realized_net_usd"]))
        except (KeyError, ValueError, InvalidOperation) as exc:
            logger.warning("Skipping record with bad timestamp/amount: %s", exc)
            continue
        age = now - ts
        if age <= ONE_DAY:
            daily += net
        if age <= SEVEN_DAYS:
            weekly += net

    # Trailing consecutive reverts (file order, newest assumed last).
    consecutive_reverts = 0
    for rec in reversed(records):
        if rec.get("reverted") is True:
            consecutive_reverts += 1
        else:
            break

    last_time: datetime | None = None
    if records:
        try:
            last_time = parse_timestamp(str(records[-1]["timestamp"]))
        except (KeyError, ValueError) as exc:
            logger.warning("Could not parse last_outcome_time: %s", exc)

    return {
        "daily_usage_usd": daily,
        "weekly_usage_usd": weekly,
        "consecutive_reverts": consecutive_reverts,
        "last_outcome_time": last_time,
    }


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------
def print_human(state: dict[str, Any], jsonl_path: str) -> None:
    last = state["last_outcome_time"]
    print("\nChimera Recovered State")
    print("-" * 60)
    print(f"source              : {jsonl_path}")
    print(f"daily_usage_usd     : {state['daily_usage_usd']}")
    print(f"weekly_usage_usd    : {state['weekly_usage_usd']}")
    print(f"consecutive_reverts : {state['consecutive_reverts']}")
    print(f"last_outcome_time   : {last.isoformat() if last else 'none'}")
    print("-" * 60)


def state_to_json(state: dict[str, Any]) -> str:
    last = state["last_outcome_time"]
    return json.dumps(
        {
            "daily_usage_usd": str(state["daily_usage_usd"]),
            "weekly_usage_usd": str(state["weekly_usage_usd"]),
            "consecutive_reverts": state["consecutive_reverts"],
            "last_outcome_time": last.isoformat() if last else None,
        },
        indent=2,
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Rebuild aggregated pacing state from the JSONL audit trail."
    )
    parser.add_argument(
        "--jsonl",
        default=DEFAULT_JSONL,
        help=f"Path to outcomes.jsonl (default {DEFAULT_JSONL}).",
    )
    parser.add_argument("--json", action="store_true", help="Emit machine-readable JSON.")
    parser.add_argument(
        "--log-level",
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
        help="Logging verbosity.",
    )
    return parser


def main() -> int:
    args = build_parser().parse_args()
    logger.setLevel(getattr(logging, args.log_level))

    state = recover_from_jsonl(args.jsonl)

    if args.json:
        print(state_to_json(state))
    else:
        print_human(state, args.jsonl)

    return 0


if __name__ == "__main__":
    sys.exit(main())
