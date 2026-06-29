#!/usr/bin/env python3
"""
dry_run.py
Project Chimera - Shadow Preflight Validator

Validates that the engine is correctly configured for a SAFE (shadow) run and,
optionally, boots the compiled binary for a few seconds with shadow mode forced
to confirm it starts without panicking. This script NEVER broadcasts and NEVER
runs anything when execute_mode would be 'live'.

Checks:
  - config/pacing.yaml parses and execute_mode == 'shadow' (FAIL loudly on 'live').
  - snapshot file exists + parses (WARN if missing).
  - RPC reachable (only if --rpc given), via raw JSON-RPC.
  - keystore env presence is OPTIONAL in shadow (reported as NOTE).

Optional spawn (--run-secs N > 0):
  - Requires a built binary (--binary, default target/release/chimera or
    target/debug/chimera). Spawns it for N seconds with CHIMERA_EXECUTE_MODE=shadow
    forced into the env, captures stdout/stderr, terminates, and reports whether it
    booted without panic. If no binary is found, the spawn is skipped with a NOTE.

INVARIANT (AGENTS.md #5): stdlib only. No web3.py. `--help` always works. The
YAML parse uses PyYAML if available, else a minimal flat-map fallback (pacing.yaml
is a flat key:value document).

Usage:
  python scripts/dry_run.py
  python scripts/dry_run.py --rpc https://mainnet.base.org
  python scripts/dry_run.py --run-secs 5 --binary target/release/chimera
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import subprocess
import sys
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
logger = logging.getLogger("dry_run")

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
DEFAULT_PACING: str = "config/pacing.yaml"
DEFAULT_SNAPSHOT: str = "config/snapshot.json"
DEFAULT_BINARIES: tuple[str, ...] = ("target/release/chimera", "target/debug/chimera")
KEYSTORE_ENV_VARS: tuple[str, ...] = ("CHIMERA_KEYSTORE_PATH", "KEYSTORE_PATH", "CHIMERA_KEYSTORE")
HTTP_TIMEOUT: int = 10


# ---------------------------------------------------------------------------
# Minimal flat-YAML parser (stdlib-only fallback)
# ---------------------------------------------------------------------------
def parse_pacing_yaml(path: str) -> dict[str, Any]:
    """
    Parse a flat `key: value` YAML document. Uses PyYAML when present; otherwise
    falls back to a minimal parser sufficient for config/pacing.yaml (no nesting,
    `#` comments, scalar values). Raises on read errors.
    """
    text = Path(path).read_text(encoding="utf-8")

    try:
        import yaml  # type: ignore

        data = yaml.safe_load(text)
        return data if isinstance(data, dict) else {}
    except ImportError:
        pass  # fall through to minimal parser

    result: dict[str, Any] = {}
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].rstrip()
        if not line or ":" not in line:
            continue
        if line[0].isspace():
            # Nested content is not expected in pacing.yaml; skip defensively.
            continue
        key, _, value = line.partition(":")
        result[key.strip()] = value.strip().strip('"').strip("'")
    return result


# ---------------------------------------------------------------------------
# Checks
# ---------------------------------------------------------------------------
def check_pacing(pacing_path: str) -> tuple[bool, str | None]:
    """Return (ok, execute_mode). ok is False only on a hard FAIL (live or unparseable)."""
    path = Path(pacing_path)
    if not path.exists():
        logger.error("FAIL: pacing config not found at %s", pacing_path)
        return False, None
    try:
        cfg = parse_pacing_yaml(pacing_path)
    except OSError as exc:
        logger.error("FAIL: cannot read pacing config: %s", exc)
        return False, None

    mode = str(cfg.get("execute_mode", "")).strip()
    if mode == "live":
        logger.error("FAIL: execute_mode is 'live' - dry_run refuses to proceed.")
        return False, mode
    if mode != "shadow":
        logger.error("FAIL: execute_mode is '%s' (expected 'shadow').", mode or "<missing>")
        return False, mode or None

    logger.info("PASS: pacing.yaml parses and execute_mode == shadow")
    return True, mode


def check_snapshot(snapshot_path: str) -> bool:
    """WARN-only check: missing snapshot is acceptable (engine uses empty snapshot)."""
    path = Path(snapshot_path)
    if not path.exists():
        logger.warning("WARN: snapshot %s missing (engine boots with empty snapshot).", snapshot_path)
        return True
    try:
        with open(path, "r", encoding="utf-8") as f:
            json.load(f)
        logger.info("PASS: snapshot %s parses", snapshot_path)
        return True
    except (json.JSONDecodeError, OSError) as exc:
        logger.warning("WARN: snapshot %s unparseable: %s", snapshot_path, exc)
        return True


def check_rpc(rpc: str | None) -> bool:
    """Optional RPC reachability via raw JSON-RPC. Returns True if skipped or reachable."""
    if not rpc:
        logger.info("NOTE: no --rpc given; RPC reachability skipped.")
        return True
    payload = json.dumps(
        {"jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1}
    ).encode("utf-8")
    req = urllib_request.Request(
        rpc,
        data=payload,
        headers={"Content-Type": "application/json", "User-Agent": "chimera-dryrun/1.0"},
        method="POST",
    )
    try:
        with urllib_request.urlopen(req, timeout=HTTP_TIMEOUT) as resp:
            body = json.loads(resp.read().decode("utf-8"))
        if "result" in body:
            logger.info("PASS: RPC reachable (block %d)", int(body["result"], 16))
            return True
        logger.error("FAIL: RPC returned error: %s", body.get("error"))
        return False
    except (urllib_error.URLError, ValueError, TimeoutError) as exc:
        logger.error("FAIL: RPC unreachable: %s", exc)
        return False


def check_keystore_env() -> None:
    """Keystore is OPTIONAL in shadow. Report presence as a NOTE; never log values."""
    present = [v for v in KEYSTORE_ENV_VARS if os.getenv(v)]
    if present:
        logger.info("NOTE: keystore env present (%s) - optional in shadow.", ", ".join(present))
    else:
        logger.info("NOTE: no keystore env set - acceptable for shadow run.")


# ---------------------------------------------------------------------------
# Optional binary spawn
# ---------------------------------------------------------------------------
def resolve_binary(explicit: str | None) -> str | None:
    """Return a path to an existing binary, or None if not found."""
    candidates = [explicit] if explicit else list(DEFAULT_BINARIES)
    for cand in candidates:
        if not cand:
            continue
        p = Path(cand)
        for variant in (p, p.with_suffix(".exe")):  # Windows-friendly
            if variant.exists():
                return str(variant)
    return None


def spawn_shadow(binary: str, run_secs: int) -> bool:
    """
    Boot the binary for `run_secs` seconds with CHIMERA_EXECUTE_MODE=shadow forced.
    Returns True if it ran for the duration (or exited cleanly) without a panic.
    """
    env = dict(os.environ)
    env["CHIMERA_EXECUTE_MODE"] = "shadow"  # hard-force shadow; never live.

    logger.info("Spawning %s for %ds with CHIMERA_EXECUTE_MODE=shadow", binary, run_secs)
    try:
        proc = subprocess.Popen(
            [binary],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env=env,
            text=True,
        )
    except OSError as exc:
        logger.error("FAIL: could not spawn binary: %s", exc)
        return False

    stdout = ""
    booted = True
    try:
        stdout, _ = proc.communicate(timeout=run_secs)
        # Process exited before timeout. A non-zero exit may indicate a panic.
        if proc.returncode not in (0, None):
            logger.error("FAIL: binary exited early with code %s", proc.returncode)
            booted = False
    except subprocess.TimeoutExpired:
        # Expected path: it stayed up for the full window. Terminate it.
        proc.terminate()
        try:
            stdout, _ = proc.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            stdout, _ = proc.communicate()
        logger.info("Binary stayed up for %ds; terminated.", run_secs)

    if stdout and "panic" in stdout.lower():
        logger.error("FAIL: 'panic' detected in binary output.")
        booted = False

    if booted:
        logger.info("PASS: binary booted in shadow without panic.")
    return booted


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Preflight validation for a safe (shadow) Chimera run. Never broadcasts."
    )
    parser.add_argument("--rpc", default=None, help="RPC URL for an optional reachability check.")
    parser.add_argument(
        "--pacing-config",
        default=DEFAULT_PACING,
        help=f"Path to pacing.yaml (default {DEFAULT_PACING}).",
    )
    parser.add_argument(
        "--snapshot",
        default=DEFAULT_SNAPSHOT,
        help=f"Path to snapshot.json (default {DEFAULT_SNAPSHOT}).",
    )
    parser.add_argument(
        "--binary",
        default=None,
        help="Path to the compiled chimera binary (for --run-secs spawn).",
    )
    parser.add_argument(
        "--run-secs",
        type=int,
        default=0,
        help="Seconds to boot the binary in shadow (0 = preflight only).",
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

    pacing_ok, mode = check_pacing(args.pacing_config)
    if not pacing_ok:
        logger.error("Preflight aborted: pacing/execute_mode check failed.")
        return 1

    check_snapshot(args.snapshot)
    rpc_ok = check_rpc(args.rpc)
    check_keystore_env()

    if not rpc_ok:
        logger.error("Preflight FAIL: RPC check failed.")
        return 1

    # Optional spawn — only ever reached when execute_mode == shadow.
    if args.run_secs > 0:
        if mode != "shadow":  # belt-and-suspenders; should be unreachable here.
            logger.error("Refusing to spawn: execute_mode is not shadow.")
            return 1
        binary = resolve_binary(args.binary)
        if not binary:
            logger.info("NOTE: no binary found; skipping spawn (build with cargo to enable).")
        else:
            if not spawn_shadow(binary, args.run_secs):
                return 1

    logger.info("Preflight PASS (execute_mode=shadow).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
