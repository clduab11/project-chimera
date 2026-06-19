#!/usr/bin/env python3
"""
emergency_pause.py
Project Chimera - Operator Emergency Stop Script

Writes a `PAUSED` flag to a state file that the Rust binary monitors.
Optionally dispatches a webhook alert (e.g., Slack, Discord, PagerDuty).

The --resume flag clears the pause state.

Usage:
  python scripts/emergency_pause.py --state-file core/state/emergency.flag --reason "manual breaker"
  python scripts/emergency_pause.py --state-file core/state/emergency.flag --reason "gas spike" --alert-url https://hooks.slack.com/services/xxx
  python scripts/emergency_pause.py --state-file core/state/emergency.flag --resume
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any
from urllib import error as urllib_error
from urllib import request as urllib_request

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("emergency_pause")

# ---------------------------------------------------------------------------
# Data models
# ---------------------------------------------------------------------------
@dataclass
class PauseState:
    paused: bool
    reason: str
    triggered_at: int
    triggered_by: str
    resumed_at: int | None = None
    resumed_by: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


# ---------------------------------------------------------------------------
# State file I/O
# ---------------------------------------------------------------------------
def read_state(state_file: str) -> PauseState | None:
    """Read existing pause state, or None if file does not exist."""
    path = Path(state_file)
    if not path.exists():
        return None
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
        return PauseState(**data)
    except (json.JSONDecodeError, TypeError) as exc:
        logger.warning("Corrupt state file %s: %s", state_file, exc)
        return None


def write_state(state_file: str, state: PauseState) -> None:
    """Atomically write pause state to disk."""
    path = Path(state_file)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(state.to_dict(), f, indent=2)
    tmp.replace(path)
    logger.info("State written to %s (paused=%s)", state_file, state.paused)


def clear_state(state_file: str) -> None:
    """Remove the pause flag file entirely."""
    path = Path(state_file)
    if path.exists():
        path.unlink()
        logger.info("Pause state cleared (%s removed)", state_file)
    else:
        logger.info("No pause state file to remove at %s", state_file)


# ---------------------------------------------------------------------------
# Alerting
# ---------------------------------------------------------------------------
def send_webhook(alert_url: str, payload: dict[str, Any]) -> bool:
    """POST JSON payload to a webhook URL."""
    data = json.dumps(payload).encode("utf-8")
    headers = {"Content-Type": "application/json", "User-Agent": "chimera-emergency-pause/1.0"}

    req = urllib_request.Request(alert_url, data=data, headers=headers, method="POST")
    try:
        with urllib_request.urlopen(req, timeout=10) as resp:
            logger.info("Webhook alert sent (HTTP %s)", resp.status)
            return True
    except urllib_error.HTTPError as exc:
        logger.error("Webhook alert failed (HTTP %s): %s", exc.code, exc.reason)
    except urllib_error.URLError as exc:
        logger.error("Webhook alert failed: %s", exc.reason)
    except Exception as exc:
        logger.error("Webhook alert failed: %s", exc)
    return False


def build_alert_payload(state: PauseState, hostname: str | None = None) -> dict[str, Any]:
    """Construct a standard alert payload."""
    return {
        "text": f"🚨 Chimera Emergency Pause Triggered",
        "status": "PAUSED" if state.paused else "RESUMED",
        "reason": state.reason,
        "triggered_at": state.triggered_at,
        "triggered_by": state.triggered_by,
        "hostname": hostname or os.getenv("HOSTNAME", "unknown"),
        "details": state.to_dict(),
    }


# ---------------------------------------------------------------------------
# Core actions
# ---------------------------------------------------------------------------
def trigger_pause(state_file: str, reason: str, alert_url: str | None = None) -> int:
    """Set the paused flag and optionally alert."""
    user = os.getenv("USER", os.getenv("USERNAME", "unknown"))
    state = PauseState(
        paused=True,
        reason=reason,
        triggered_at=int(time.time()),
        triggered_by=user,
    )

    write_state(state_file, state)
    logger.warning("EMERGENCY PAUSE triggered by %s: %s", user, reason)

    if alert_url:
        payload = build_alert_payload(state)
        send_webhook(alert_url, payload)

    return 0


def trigger_resume(state_file: str, alert_url: str | None = None) -> int:
    """Clear the paused flag and optionally alert."""
    user = os.getenv("USER", os.getenv("USERNAME", "unknown"))
    existing = read_state(state_file)

    if existing and existing.paused:
        # Write a final resume record before clearing
        resumed_state = PauseState(
            paused=False,
            reason=existing.reason,
            triggered_at=existing.triggered_at,
            triggered_by=existing.triggered_by,
            resumed_at=int(time.time()),
            resumed_by=user,
        )
        write_state(state_file, resumed_state)
        logger.info("EMERGENCY PAUSE cleared by %s", user)

        if alert_url:
            payload = build_alert_payload(resumed_state)
            send_webhook(alert_url, payload)
    else:
        logger.info("No active pause to resume")

    # Always remove the flag file so the Rust binary sees no pause
    clear_state(state_file)
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Trigger or clear the Chimera emergency pause flag."
    )
    parser.add_argument(
        "--state-file",
        required=True,
        help="Path to the pause state file (e.g., core/state/emergency.flag).",
    )
    parser.add_argument(
        "--reason",
        default="manual breaker",
        help="Reason for the pause (ignored with --resume).",
    )
    parser.add_argument(
        "--alert-url",
        default=None,
        help="Webhook URL for alert dispatch (optional).",
    )
    parser.add_argument(
        "--resume",
        action="store_true",
        help="Clear the pause state instead of setting it.",
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

    if args.resume:
        return trigger_resume(args.state_file, alert_url=args.alert_url)

    if not args.reason:
        logger.error("--reason is required when triggering a pause")
        return 1

    return trigger_pause(
        args.state_file,
        reason=args.reason,
        alert_url=args.alert_url,
    )


if __name__ == "__main__":
    sys.exit(main())
