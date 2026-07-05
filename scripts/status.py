#!/usr/bin/env python3
"""
status.py
Project Chimera - Read-only CLI Status Dashboard

Renders a five-section operational dashboard for the MEV liquidation bot:
  1. Header   - current execute_mode (live | shadow), chain, timestamp.
  2. EOAs     - rotation pool with truncated addresses, labels, ETH balances.
  3. Pacing   - daily/weekly realized net USD vs. configured caps.
  4. Breaker  - circuit-breaker status (tripped / clear) and trailing reverts.
  5. Outcomes - last 5 audit-log entries translated to plain English.

The dashboard is strictly read-only: no files are mutated, no transactions sent.
Daily/weekly usage is aggregated from `core/state/outcomes.jsonl` exactly the way
`core::state::recovery::CrashRecovery::recover_from_jsonl` aggregates it in
the Rust core.

Usage:
  python scripts/status.py --chain base
  python scripts/status.py --chain arbitrum --mock
  python scripts/status.py --chain base --state-dir core/state --config-dir config

Required env vars (live mode only):
  BASE_RPC_URL - Base mainnet RPC endpoint.
  ARB_RPC_URL  - Arbitrum One RPC endpoint.

Dependencies:
  rich>=13.0   (required)
  PyYAML>=6.0  (required, already in requirements.txt)
  web3>=6.0    (optional - falls back to "RPC offline" if missing)
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
from datetime import datetime, timedelta, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any

import yaml

from rich.console import Console
from rich.panel import Panel
from rich.table import Table
from rich.text import Text

try:
    from web3 import Web3
    HAS_WEB3 = True
except ImportError:  # Keep script importable on machines without web3.py.
    Web3 = None  # type: ignore[assignment]
    HAS_WEB3 = False

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.WARNING,  # Dashboard noise: only show real problems.
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("status")

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
DEFAULT_STATE_DIR: str = "core/state"
DEFAULT_CONFIG_DIR: str = "config"
AUDIT_FILENAME: str = "outcomes.jsonl"
EOA_POOL_FILENAME: str = "eoa_pool.json"
PACING_FILENAME: str = "pacing.yaml"

RPC_ENV_VARS: dict[str, str] = {
    "base": "BASE_RPC_URL",
    "arbitrum": "ARB_RPC_URL",
}

DEFAULT_BREAKER_THRESHOLD: int = 3  # Matches pacing.yaml auto_halt_on_reverts default.
RECENT_OUTCOMES_DISPLAY: int = 5
MOCK_BALANCE_ETH: str = "0.0100 ETH"


# ---------------------------------------------------------------------------
# File loaders (each returns None on missing-file, raises on corrupt input)
# ---------------------------------------------------------------------------
def load_pacing(config_dir: Path) -> dict[str, Any] | None:
    """Load pacing.yaml. Returns None if the file is absent."""
    path = config_dir / PACING_FILENAME
    if not path.exists():
        return None
    with open(path, "r", encoding="utf-8") as f:
        data = yaml.safe_load(f) or {}
    if not isinstance(data, dict):
        logger.warning("pacing.yaml did not parse to a mapping; treating as empty")
        return {}
    return data


def load_eoa_pool(config_dir: Path) -> list[dict[str, Any]] | None:
    """
    Load wallets array from eoa_pool.json.

    The on-disk schema is an object with a `wallets` list (version 2.0+),
    not a plain list - mirrors what rotate_eoa.py expects.
    """
    path = config_dir / EOA_POOL_FILENAME
    if not path.exists():
        return None
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    wallets = data.get("wallets", [])
    if not isinstance(wallets, list):
        logger.warning("eoa_pool.json 'wallets' is not a list; treating as empty")
        return []
    return wallets


def read_audit_entries(audit_path: Path) -> list[dict[str, Any]]:
    """
    Stream parse outcomes.jsonl, skipping corrupted lines.

    Mirrors the tolerance applied by core/src/state/persistence.rs::load_recent:
    a single bad line never aborts the dashboard.
    """
    entries: list[dict[str, Any]] = []
    with open(audit_path, "r", encoding="utf-8") as f:
        for line_num, raw in enumerate(f, start=1):
            line = raw.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError as exc:
                logger.warning("Skipping corrupted audit line %d: %s", line_num, exc)
                continue
            if isinstance(obj, dict):
                entries.append(obj)
    return entries


# ---------------------------------------------------------------------------
# Web3 helpers
# ---------------------------------------------------------------------------
def get_w3(chain: str) -> Any | None:
    """
    Build a Web3 HTTP provider for `chain`, reading the RPC URL from env.

    Returns None on any of: web3.py not installed, env var missing,
    connection failure. The caller renders "RPC offline" for any None result.
    """
    if not HAS_WEB3:
        return None
    env_var = RPC_ENV_VARS.get(chain)
    if env_var is None:
        return None
    url = os.environ.get(env_var)
    if not url:
        return None
    try:
        w3 = Web3(Web3.HTTPProvider(url, request_kwargs={"timeout": 10}))
        if not w3.is_connected():
            logger.warning("RPC %s did not respond to is_connected check", env_var)
            return None
        return w3
    except Exception as exc:  # noqa: BLE001 - want broad catch for offline mode.
        logger.warning("Failed to initialise Web3 for %s: %s", chain, exc)
        return None


def fetch_balance_eth(w3: Any, address: str) -> str:
    """Return formatted ETH balance, or 'RPC offline' on any failure."""
    try:
        checksum = Web3.to_checksum_address(address)
        wei = w3.eth.get_balance(checksum)
        return f"{wei / 1e18:.4f} ETH"
    except Exception as exc:  # noqa: BLE001 - offline-tolerant.
        logger.warning("Balance fetch failed for %s: %s", address, exc)
        return "RPC offline"


# ---------------------------------------------------------------------------
# Pure formatting / aggregation helpers
# ---------------------------------------------------------------------------
def truncate_address(addr: str) -> str:
    """Render '0x1234abcd...wxyz' as '0x1234...wxyz' (6+ellipsis+4)."""
    if not isinstance(addr, str) or not addr.startswith("0x") or len(addr) < 12:
        return addr or ""
    return f"{addr[:6]}\u2026{addr[-4:]}"


def parse_timestamp(ts: Any) -> datetime | None:
    """
    Parse an ISO-8601 timestamp (with optional trailing 'Z') as serialised by
    `chrono::DateTime<Utc>` from the Rust core. Returns None on parse failure.
    """
    if not isinstance(ts, str):
        return None
    candidate = ts.replace("Z", "+00:00") if ts.endswith("Z") else ts
    try:
        parsed = datetime.fromisoformat(candidate)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed


def to_decimal(value: Any) -> Decimal:
    """
    Coerce a JSON value into Decimal. `rust_decimal::Decimal` is serialised
    by serde_json as a JSON string by default, so we accept str | int | float.
    """
    if value is None:
        return Decimal("0")
    try:
        return Decimal(str(value))
    except (InvalidOperation, ValueError, TypeError):
        return Decimal("0")


def aggregate_pacing(entries: list[dict[str, Any]]) -> tuple[Decimal, Decimal]:
    """
    Recreate the daily/weekly aggregation done by
    `CrashRecovery::recover_from_jsonl` so the dashboard agrees with the
    bot's internal pacing accountant.
    """
    now = datetime.now(timezone.utc)
    daily = Decimal("0")
    weekly = Decimal("0")
    for entry in entries:
        ts = parse_timestamp(entry.get("timestamp"))
        if ts is None:
            continue
        net = to_decimal(entry.get("realized_net_usd"))
        age = now - ts
        if age <= timedelta(days=7):
            weekly += net
            if age <= timedelta(days=1):
                daily += net
    return daily, weekly


def count_trailing_reverts(entries: list[dict[str, Any]]) -> int:
    """Count consecutive trailing reverts (newest first)."""
    count = 0
    for entry in reversed(entries):
        if bool(entry.get("reverted")):
            count += 1
        else:
            break
    return count


def outcome_text(entry: dict[str, Any]) -> str:
    """
    Translate a single audit entry into a plain-English outcome string.

    The Rust core writes `decision` as a string like "Allow" or a denial
    label such as "Deny:InsufficientFunds". We map:
      decision == "Allow", reverted=False  -> "Success"
      decision == "Allow", reverted=True   -> "Reverted"
      decision starts with "Deny"          -> "Denied: <reason or 'unspecified'>"
      anything else                        -> the raw decision string
    """
    decision = str(entry.get("decision", "")).strip()
    reverted = bool(entry.get("reverted", False))
    lowered = decision.lower()

    if lowered == "allow":
        return "Reverted" if reverted else "Success"
    if lowered.startswith("deny"):
        # Accept formats like "Deny", "Deny: foo", "Deny:foo".
        if ":" in decision:
            reason = decision.split(":", 1)[1].strip()
            return f"Denied: {reason or 'unspecified'}"
        return "Denied"
    return decision or "Unknown"


# ---------------------------------------------------------------------------
# Render functions (each prints one dashboard section to `console`)
# ---------------------------------------------------------------------------
def render_header(
    console: Console,
    pacing: dict[str, Any] | None,
    chain: str,
) -> None:
    """Top panel: execute mode, chain, timestamp."""
    mode = "unknown"
    if pacing is not None:
        mode = str(pacing.get("execute_mode", "unknown"))

    ts = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC")

    body = Text()
    body.append("Mode:  ")
    if mode == "live":
        body.append(mode.upper(), style="bold red")
        body.append("  (real funds at risk)", style="red")
    elif mode == "shadow":
        body.append(mode, style="bold cyan")
    else:
        body.append(mode, style="bold yellow")
    body.append("\n")
    body.append(f"Chain: {chain}\n")
    body.append(f"Time:  {ts}")

    console.print(Panel(body, title="Chimera Status", border_style="blue"))


def render_eoa_table(
    console: Console,
    wallets: list[dict[str, Any]],
    chain: str,
    mock: bool,
    w3: Any | None,
) -> None:
    """EOA pool table: truncated address, label, ETH balance."""
    table = Table(
        title=f"EOA Pool ({chain})",
        show_header=True,
        header_style="bold magenta",
        title_justify="left",
    )
    table.add_column("Address", style="cyan", no_wrap=True)
    table.add_column("Label")
    table.add_column("Balance", justify="right")

    if not wallets:
        table.add_row("—", "—", "no wallets configured")
        console.print(table)
        return

    for wallet in wallets:
        addr = str(wallet.get("address", ""))
        label = str(wallet.get("label", "") or "")
        excluded = bool(wallet.get("excluded", False))

        if mock:
            balance_cell: str | Text = MOCK_BALANCE_ETH
        elif w3 is None:
            balance_cell = Text("RPC offline", style="dim yellow")
        else:
            balance_cell = fetch_balance_eth(w3, addr)

        label_cell = Text(label, style="dim") if excluded else label
        table.add_row(truncate_address(addr), label_cell, balance_cell)

    console.print(table)


def _format_pacing_line(used: Decimal, cap: Decimal, label: str) -> Text:
    """Format a single 'Daily: $X / $Y (Z% remaining)' line with color."""
    line = Text()
    line.append(f"{label}: ")
    if cap <= 0:
        line.append(f"${used:,.2f} / $0.00 (cap unset)", style="yellow")
        return line

    remaining = cap - used
    remaining_pct = (remaining / cap) * Decimal("100")
    # Clamp display range to [0, 100] but preserve the actual used value.
    pct_display = max(Decimal("0"), min(Decimal("100"), remaining_pct))

    if remaining <= 0:
        style = "bold red"
    elif pct_display < Decimal("25"):
        style = "yellow"
    else:
        style = "green"

    line.append(f"${used:,.2f} / ${cap:,.2f} ", style="white")
    line.append(f"({pct_display:.1f}% remaining)", style=style)
    return line


def render_pacing_panel(
    console: Console,
    entries: list[dict[str, Any]],
    pacing: dict[str, Any] | None,
) -> None:
    """Daily/weekly spend vs. configured caps."""
    daily, weekly = aggregate_pacing(entries)

    if pacing is None:
        body = Text(
            "Pacing caps unknown (no pacing.yaml); showing usage only.\n",
            style="yellow",
        )
        body.append(f"Daily realized: ${daily:,.2f}\n")
        body.append(f"Weekly realized: ${weekly:,.2f}")
        console.print(Panel(body, title="Pacing State", border_style="yellow"))
        return

    max_daily = to_decimal(pacing.get("max_daily_net_usd", 0))
    max_weekly = to_decimal(pacing.get("max_weekly_net_usd", 0))

    body = Text()
    body.append(_format_pacing_line(daily, max_daily, "Daily "))
    body.append("\n")
    body.append(_format_pacing_line(weekly, max_weekly, "Weekly"))

    console.print(Panel(body, title="Pacing State", border_style="green"))


def render_breaker_panel(
    console: Console,
    entries: list[dict[str, Any]],
    pacing: dict[str, Any] | None,
) -> None:
    """Circuit breaker status, derived from trailing reverts."""
    threshold = DEFAULT_BREAKER_THRESHOLD
    if pacing is not None:
        try:
            threshold = int(pacing.get("auto_halt_on_reverts", DEFAULT_BREAKER_THRESHOLD))
        except (TypeError, ValueError):
            threshold = DEFAULT_BREAKER_THRESHOLD

    trailing = count_trailing_reverts(entries)
    tripped = trailing >= threshold and threshold > 0

    body = Text()
    if tripped:
        body.append("TRIPPED ", style="bold red")
        body.append(
            f"- {trailing} consecutive reverts >= threshold {threshold}",
            style="red",
        )
        border = "red"
    else:
        body.append("CLEAR ", style="bold green")
        body.append(
            f"- {trailing}/{threshold} consecutive trailing reverts",
            style="green",
        )
        border = "green"

    console.print(Panel(body, title="Circuit Breaker", border_style=border))


def render_outcomes_table(
    console: Console,
    entries: list[dict[str, Any]],
) -> None:
    """Last N outcomes formatted as plain English."""
    table = Table(
        title=f"Recent Outcomes (last {RECENT_OUTCOMES_DISPLAY})",
        show_header=True,
        header_style="bold magenta",
        title_justify="left",
    )
    table.add_column("Time", style="cyan", no_wrap=True)
    table.add_column("Opportunity ID", no_wrap=True)
    table.add_column("Outcome")
    table.add_column("Profit USD", justify="right")

    recent = entries[-RECENT_OUTCOMES_DISPLAY:] if entries else []
    if not recent:
        table.add_row("—", "—", "no entries yet", "—")
        console.print(table)
        return

    for entry in recent:
        ts = parse_timestamp(entry.get("timestamp"))
        ts_str = ts.strftime("%Y-%m-%d %H:%M:%S") if ts else str(entry.get("timestamp", ""))

        oid = str(entry.get("id", ""))
        outcome = outcome_text(entry)
        net = to_decimal(entry.get("realized_net_usd"))

        if "Denied" in outcome:
            outcome_cell: str | Text = Text(outcome, style="yellow")
        elif outcome == "Reverted":
            outcome_cell = Text(outcome, style="red")
        elif outcome == "Success":
            outcome_cell = Text(outcome, style="green")
        else:
            outcome_cell = outcome

        net_style = "green" if net > 0 else ("red" if net < 0 else "white")
        net_cell = Text(f"${net:,.2f}", style=net_style)

        table.add_row(ts_str, oid, outcome_cell, net_cell)

    console.print(table)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Read-only operational status dashboard for the Chimera bot.",
    )
    parser.add_argument(
        "--chain",
        default="base",
        choices=["base", "arbitrum"],
        help="Target chain (default: base).",
    )
    parser.add_argument(
        "--mock",
        action="store_true",
        help="Skip RPC calls; show placeholder balances. Use for offline / CI.",
    )
    parser.add_argument(
        "--state-dir",
        default=DEFAULT_STATE_DIR,
        help=f"Directory containing outcomes.jsonl (default: {DEFAULT_STATE_DIR}).",
    )
    parser.add_argument(
        "--config-dir",
        default=DEFAULT_CONFIG_DIR,
        help=f"Directory containing pacing.yaml and eoa_pool.json (default: {DEFAULT_CONFIG_DIR}).",
    )
    parser.add_argument(
        "--log-level",
        default="WARNING",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
        help="Logging verbosity (default: WARNING).",
    )
    return parser


def main() -> int:
    args = build_parser().parse_args()
    logger.setLevel(getattr(logging, args.log_level))

    console = Console()
    state_dir = Path(args.state_dir)
    config_dir = Path(args.config_dir)
    audit_path = state_dir / AUDIT_FILENAME

    # --- Section 1: Header -----------------------------------------------
    pacing = load_pacing(config_dir)
    if pacing is None:
        console.print("[yellow]No pacing config found at "
                      f"{config_dir / PACING_FILENAME}[/yellow]")
    render_header(console, pacing, args.chain)

    # --- Section 2: EOA Pool ---------------------------------------------
    wallets = load_eoa_pool(config_dir)
    if wallets is None:
        console.print("[yellow]No EOA pool config found at "
                      f"{config_dir / EOA_POOL_FILENAME}[/yellow]")
    else:
        w3 = get_w3(args.chain) if not args.mock else None
        if not args.mock and not HAS_WEB3:
            console.print("[yellow]web3.py not installed; "
                          "balances will show as RPC offline[/yellow]")
        render_eoa_table(console, wallets, args.chain, args.mock, w3)

    # --- Sections 3-5: state-dependent -----------------------------------
    if not audit_path.exists():
        console.print(
            Panel(
                Text(
                    f"No state file found at {audit_path}\n"
                    "Bot has not run yet, or --state-dir is incorrect.",
                    style="yellow",
                ),
                title="State",
                border_style="yellow",
            )
        )
        return 0

    try:
        entries = read_audit_entries(audit_path)
    except OSError as exc:
        logger.error("Failed to read audit log: %s", exc)
        return 1

    render_pacing_panel(console, entries, pacing)
    render_breaker_panel(console, entries, pacing)
    render_outcomes_table(console, entries)

    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as exc:  # noqa: BLE001 - top-level safety net.
        logger.exception("Unexpected error: %s", exc)
        sys.exit(1)
