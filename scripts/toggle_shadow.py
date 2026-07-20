#!/usr/bin/env python3
"""
toggle_shadow.py
Project Chimera - Mode-Transition State Manager

Reads and manages `core/state/mode.json`, the transition-gate state file the
Rust binary loads at startup. EXACT schema the binary expects (see
core/src/main.rs `ModeState`):

    { "previous_mode": "shadow", "shadow_since": <unix_seconds_integer> }

  - previous_mode : "shadow" | "live"
  - shadow_since  : unix seconds when shadow began (0 == unstamped / unknown)

IMPORTANT - what this script does NOT do:
  This script NEVER edits the committed `config/pacing.yaml` `execute_mode`
  field. Flipping the engine to live is a deliberate config change made
  separately. This tool only updates mode.json. At startup, Rust independently
  validates `shadow_since` age whenever the effective execute mode is live,
  regardless of mode.json `previous_mode`.

SOAK GATE STATUS (2026-07-20):
  The 7-day (604800s) soak requirement was DE-LISTED by operator decision on
  2026-07-20. SHADOW_SOAK_SECONDS is now 0: `--set-live` updates mode.json as
  soon as a shadow_since stamp exists (run --set-shadow once to stamp). Rust
  no longer enforces a minimum shadow age on live startup. The shadow_since
  timestamp is still preserved in mode.json for the audit trail.

INVARIANT (AGENTS.md #5): stdlib only. No web3.py. `--help` always works.
All writes are atomic (tmp file + os.replace).

Usage:
  python scripts/toggle_shadow.py --show
  python scripts/toggle_shadow.py --set-shadow
  python scripts/toggle_shadow.py --set-live
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import time
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
logger = logging.getLogger("toggle_shadow")

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
DEFAULT_STATE_FILE: str = "core/state/mode.json"
# 2026-07-20: 7-day soak gate de-listed by operator decision (final authority).
# Was 7 * 24 * 60 * 60 (604800). Zero disables the age refusal in --set-live.
SHADOW_SOAK_SECONDS: int = 0


# ---------------------------------------------------------------------------
# State I/O
# ---------------------------------------------------------------------------
def read_mode(state_file: str) -> dict[str, Any] | None:
    """Read mode.json, or None if it does not exist. Raises on corrupt JSON."""
    path = Path(state_file)
    if not path.exists():
        return None
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)


def write_mode(state_file: str, previous_mode: str, shadow_since: int) -> None:
    """Atomically write mode.json with the exact schema the binary expects."""
    path = Path(state_file)
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {"previous_mode": previous_mode, "shadow_since": int(shadow_since)}
    tmp = path.with_suffix(".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(payload, f, indent=2)
    os.replace(tmp, path)
    logger.info(
        "Wrote %s (previous_mode=%s, shadow_since=%d)", state_file, previous_mode, shadow_since
    )


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
def fmt_duration(seconds: int) -> str:
    """Human-readable H/M/S duration."""
    seconds = max(0, int(seconds))
    days, rem = divmod(seconds, 86400)
    hours, rem = divmod(rem, 3600)
    minutes, _ = divmod(rem, 60)
    return f"{days}d {hours}h {minutes}m"


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------
def do_show(state_file: str) -> int:
    state = read_mode(state_file)
    if state is None:
        logger.info("No mode.json at %s (binary defaults to shadow / unstamped).", state_file)
        print(json.dumps({"previous_mode": "shadow", "shadow_since": 0, "_exists": False}, indent=2))
        return 0
    shadow_since = int(state.get("shadow_since", 0) or 0)
    enriched = dict(state)
    enriched["_exists"] = True
    if shadow_since > 0:
        age = int(time.time()) - shadow_since
        enriched["_shadow_age"] = fmt_duration(age)
        enriched["_soak_satisfied"] = age >= SHADOW_SOAK_SECONDS
    print(json.dumps(enriched, indent=2))
    return 0


def do_set_shadow(state_file: str) -> int:
    """Set previous_mode=shadow; stamp shadow_since=now only if not already stamped."""
    state = read_mode(state_file)
    now = int(time.time())
    if state and int(state.get("shadow_since", 0) or 0) > 0:
        shadow_since = int(state["shadow_since"])
        logger.info("Preserving existing shadow_since=%d (soak clock not reset).", shadow_since)
    else:
        shadow_since = now
        logger.info("Stamping shadow_since=%d (now).", shadow_since)
    write_mode(state_file, "shadow", shadow_since)
    return 0


def do_set_live(state_file: str) -> int:
    """
    Update mode.json previous_mode=live. The 7-day soak gate was de-listed by
    operator decision on 2026-07-20; a stamped mode.json is still required so
    the binary loads an explicit mode state (run --set-shadow once first).
    """
    state = read_mode(state_file)
    if state is None:
        logger.error("Refusing --set-live: no mode.json (shadow_since unstamped).")
        logger.error("Run --set-shadow first to stamp the mode state.")
        return 1

    shadow_since = int(state.get("shadow_since", 0) or 0)
    if shadow_since <= 0:
        logger.error("Refusing --set-live: shadow_since is unstamped (0). Run --set-shadow first.")
        return 1

    age = int(time.time()) - shadow_since
    logger.warning(
        "Updating mode.json previous_mode=live in %s (shadow age %s; soak gate de-listed 2026-07-20).",
        state_file,
        fmt_duration(age),
    )
    logger.warning(
        "This only updates mode.json; it does not change config/pacing.yaml. "
        "Set CHIMERA_EXECUTE_MODE=live in the deployment-local environment separately."
    )
    # Preserve shadow_since for the audit trail of when the shadow clock started.
    write_mode(state_file, "live", shadow_since)
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Manage core/state/mode.json (the shadow->live transition gate)."
    )
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--show", action="store_true", help="Print current mode.json.")
    group.add_argument(
        "--set-shadow",
        action="store_true",
        help="Set previous_mode=shadow; stamp shadow_since=now if unset.",
    )
    group.add_argument(
        "--set-live",
        action="store_true",
        help="Set previous_mode=live (soak gate de-listed 2026-07-20; requires a stamped mode.json).",
    )
    parser.add_argument(
        "--state-file",
        default=DEFAULT_STATE_FILE,
        help=f"Path to mode.json (default {DEFAULT_STATE_FILE}).",
    )
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

    try:
        if args.show:
            return do_show(args.state_file)
        if args.set_shadow:
            return do_set_shadow(args.state_file)
        if args.set_live:
            return do_set_live(args.state_file)
    except json.JSONDecodeError as exc:
        logger.error("Corrupt mode.json: %s", exc)
        return 1
    except OSError as exc:
        logger.error("I/O error on %s: %s", args.state_file, exc)
        return 1

    return 1  # unreachable; mutually-exclusive group is required.


if __name__ == "__main__":
    sys.exit(main())
