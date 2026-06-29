#!/usr/bin/env python3
"""
health_check.py
Project Chimera - Operator Health Probe

Runs a series of lightweight checks across the running stack and prints a
PASS / WARN / FAIL summary. Intended for cron, CI smoke tests, and on-call
triage. Exits 0 only if all *critical* checks pass; exits 1 if any FAIL.

Checks:
  (a) RPC reachable        - eth_blockNumber via raw JSON-RPC (needs --rpc).
  (b) Metrics endpoint      - HTTP GET http://localhost:<port>/ contains chimera_*.
  (c) Mode state            - core/state/mode.json present + parseable; reports
                              mode and shadow_since age.
  (d) Emergency flag        - core/state/emergency.flag; reports PAUSED if paused.
  (e) Snapshot freshness    - config/snapshot.json mtime / block age, if present.

INVARIANT (AGENTS.md #5): stdlib only. The RPC check uses a raw JSON-RPC POST
via urllib; web3.py is NOT required and `--help` always works.

Usage:
  python scripts/health_check.py --rpc https://mainnet.base.org
  python scripts/health_check.py --metrics-port 9100 --json
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
import time
from dataclasses import dataclass
from enum import Enum
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
logger = logging.getLogger("health_check")

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
DEFAULT_METRICS_PORT: int = 9100
DEFAULT_STATE_DIR: str = "core/state"
DEFAULT_SNAPSHOT: str = "config/snapshot.json"
SNAPSHOT_STALE_SECONDS: int = 3600  # 1h: warn if snapshot file is older than this.
HTTP_TIMEOUT: int = 10


# ---------------------------------------------------------------------------
# Result model
# ---------------------------------------------------------------------------
class Status(str, Enum):
    PASS = "PASS"
    WARN = "WARN"
    FAIL = "FAIL"


@dataclass
class CheckResult:
    name: str
    status: Status
    detail: str
    critical: bool = True  # only critical FAILs drive the non-zero exit code

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "status": self.status.value,
            "detail": self.detail,
            "critical": self.critical,
        }


# ---------------------------------------------------------------------------
# (a) RPC reachability via raw JSON-RPC
# ---------------------------------------------------------------------------
def check_rpc(rpc: str | None) -> CheckResult:
    name = "rpc_reachable"
    if not rpc:
        return CheckResult(name, Status.WARN, "No --rpc provided; skipped.", critical=False)

    payload = json.dumps(
        {"jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1}
    ).encode("utf-8")
    req = urllib_request.Request(
        rpc,
        data=payload,
        headers={"Content-Type": "application/json", "User-Agent": "chimera-health/1.0"},
        method="POST",
    )
    try:
        with urllib_request.urlopen(req, timeout=HTTP_TIMEOUT) as resp:
            body = json.loads(resp.read().decode("utf-8"))
        if "result" in body:
            block = int(body["result"], 16)
            return CheckResult(name, Status.PASS, f"block height {block}")
        return CheckResult(name, Status.FAIL, f"RPC error: {body.get('error')}")
    except (urllib_error.URLError, ValueError, TimeoutError) as exc:
        return CheckResult(name, Status.FAIL, f"RPC unreachable: {exc}")


# ---------------------------------------------------------------------------
# (b) Metrics endpoint
# ---------------------------------------------------------------------------
def check_metrics(port: int) -> CheckResult:
    name = "metrics_endpoint"
    url = f"http://localhost:{port}/"
    req = urllib_request.Request(url, headers={"User-Agent": "chimera-health/1.0"})
    try:
        with urllib_request.urlopen(req, timeout=HTTP_TIMEOUT) as resp:
            text = resp.read().decode("utf-8", errors="replace")
        if "chimera_" in text:
            return CheckResult(name, Status.PASS, f"chimera_ metrics present at {url}")
        return CheckResult(name, Status.FAIL, f"reachable but no chimera_ metrics at {url}")
    except (urllib_error.URLError, TimeoutError) as exc:
        return CheckResult(name, Status.FAIL, f"metrics endpoint unreachable: {exc}")


# ---------------------------------------------------------------------------
# (c) Mode state
# ---------------------------------------------------------------------------
def check_mode(state_dir: str) -> CheckResult:
    name = "mode_state"
    path = Path(state_dir) / "mode.json"
    if not path.exists():
        return CheckResult(
            name, Status.WARN, f"{path} absent (binary will stamp on next boot).", critical=False
        )
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except (json.JSONDecodeError, OSError) as exc:
        return CheckResult(name, Status.FAIL, f"unparseable mode.json: {exc}")

    mode = data.get("previous_mode", "unknown")
    shadow_since = int(data.get("shadow_since", 0) or 0)
    if shadow_since > 0:
        age_h = (time.time() - shadow_since) / 3600.0
        detail = f"mode={mode}, shadow_since age={age_h:.1f}h"
    else:
        detail = f"mode={mode}, shadow_since unstamped"
    return CheckResult(name, Status.PASS, detail)


# ---------------------------------------------------------------------------
# (d) Emergency flag
# ---------------------------------------------------------------------------
def check_emergency(state_dir: str) -> CheckResult:
    name = "emergency_flag"
    path = Path(state_dir) / "emergency.flag"
    if not path.exists():
        return CheckResult(name, Status.PASS, "no emergency flag (not paused)")
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except (json.JSONDecodeError, OSError) as exc:
        # Flag present but unreadable: treat as paused to be safe.
        return CheckResult(name, Status.FAIL, f"flag present but unreadable ({exc}); assume PAUSED")

    if data.get("paused") is True:
        reason = data.get("reason", "?")
        return CheckResult(name, Status.FAIL, f"PAUSED: {reason}")
    return CheckResult(name, Status.PASS, "flag present but paused=false (resumed)")


# ---------------------------------------------------------------------------
# (e) Snapshot freshness
# ---------------------------------------------------------------------------
def check_snapshot(snapshot_path: str) -> CheckResult:
    name = "snapshot_freshness"
    path = Path(snapshot_path)
    if not path.exists():
        return CheckResult(
            name, Status.WARN, f"{snapshot_path} missing (engine runs with empty snapshot).",
            critical=False,
        )
    try:
        mtime = path.stat().st_mtime
    except OSError as exc:
        return CheckResult(name, Status.WARN, f"cannot stat snapshot: {exc}", critical=False)

    age = time.time() - mtime
    block_detail = ""
    try:
        with open(path, "r", encoding="utf-8") as f:
            snap = json.load(f)
        block = snap.get("block_number") or snap.get("block")
        if block is not None:
            block_detail = f", block={block}"
    except (json.JSONDecodeError, OSError):
        block_detail = " (unparseable body)"

    if age > SNAPSHOT_STALE_SECONDS:
        return CheckResult(
            name, Status.WARN, f"stale: {age / 60:.1f}m old{block_detail}", critical=False
        )
    return CheckResult(name, Status.PASS, f"fresh: {age / 60:.1f}m old{block_detail}")


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------
def print_summary(results: list[CheckResult]) -> None:
    print("\nChimera Health Check")
    print("-" * 70)
    for r in results:
        print(f"[{r.status.value:<4}] {r.name:<22} {r.detail}")
    print("-" * 70)
    fails = [r for r in results if r.status is Status.FAIL]
    warns = [r for r in results if r.status is Status.WARN]
    print(f"{len(results)} checks: {len(fails)} FAIL, {len(warns)} WARN")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Operator health probe for Chimera.")
    parser.add_argument("--rpc", default=None, help="RPC URL for eth_blockNumber check (optional).")
    parser.add_argument(
        "--metrics-port",
        type=int,
        default=DEFAULT_METRICS_PORT,
        help=f"Prometheus metrics port (default {DEFAULT_METRICS_PORT}).",
    )
    parser.add_argument(
        "--state-dir",
        default=DEFAULT_STATE_DIR,
        help=f"Directory holding mode.json/emergency.flag (default {DEFAULT_STATE_DIR}).",
    )
    parser.add_argument(
        "--snapshot",
        default=DEFAULT_SNAPSHOT,
        help=f"Path to snapshot.json (default {DEFAULT_SNAPSHOT}).",
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

    results: list[CheckResult] = [
        check_rpc(args.rpc),
        check_metrics(args.metrics_port),
        check_mode(args.state_dir),
        check_emergency(args.state_dir),
        check_snapshot(args.snapshot),
    ]

    if args.json:
        critical_fail = any(r.status is Status.FAIL and r.critical for r in results)
        print(
            json.dumps(
                {
                    "ok": not critical_fail,
                    "checks": [r.to_dict() for r in results],
                },
                indent=2,
            )
        )
    else:
        print_summary(results)

    critical_fail = any(r.status is Status.FAIL and r.critical for r in results)
    return 1 if critical_fail else 0


if __name__ == "__main__":
    sys.exit(main())
