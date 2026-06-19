#!/usr/bin/env python3
"""
rotate_eoa.py
Project Chimera - EOA Wallet Rotation Utility

Reads a pool of externally-owned accounts (EOAs) from config/eoa_pool.json,
selects the next wallet in round-robin rotation (with a 24-hour cooldown),
updates the last-used timestamp, and outputs the selected address.

Usage:
  python scripts/rotate_eoa.py --config config/eoa_pool.json --chain base
  python scripts/rotate_eoa.py --config config/eoa_pool.json --chain base --min-eth 0.01
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

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("rotate_eoa")

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
DEFAULT_MIN_ETH: float = 0.005
DEFAULT_COOLDOWN_SECONDS: int = 24 * 3600  # 24 hours

# ---------------------------------------------------------------------------
# Data models
# ---------------------------------------------------------------------------
@dataclass
class Wallet:
    address: str
    label: str | None = None
    chain_balances: dict[str, str] | None = None  # chain -> wei string
    last_used: int = 0  # unix timestamp
    use_count: int = 0
    excluded: bool = False


@dataclass
class EOAPool:
    version: str = "1.0"
    wallets: list[Wallet] | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> EOAPool:
        wallets = [Wallet(**w) for w in data.get("wallets", [])]
        return cls(version=data.get("version", "1.0"), wallets=wallets)

    def to_dict(self) -> dict[str, Any]:
        return {
            "version": self.version,
            "wallets": [asdict(w) for w in (self.wallets or [])],
        }


# ---------------------------------------------------------------------------
# File I/O
# ---------------------------------------------------------------------------
def load_pool(config_path: str) -> EOAPool:
    """Load the EOA pool from JSON. Creates a template if missing."""
    path = Path(config_path)
    if not path.exists():
        logger.warning("Config not found at %s - creating template", config_path)
        template = EOAPool(
            version="1.0",
            wallets=[
                Wallet(
                    address="0x0000000000000000000000000000000000000000",
                    label="example-wallet-1",
                    chain_balances={"base": "0", "arbitrum": "0"},
                    last_used=0,
                    use_count=0,
                    excluded=False,
                )
            ],
        )
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            json.dump(template.to_dict(), f, indent=2)
        logger.info("Template written to %s - populate with real addresses", config_path)

    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)

    pool = EOAPool.from_dict(data)
    logger.info("Loaded %d wallets from %s", len(pool.wallets or []), config_path)
    return pool


def save_pool(pool: EOAPool, config_path: str) -> None:
    """Persist the updated EOA pool back to disk."""
    path = Path(config_path)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(pool.to_dict(), f, indent=2)
    logger.debug("Pool state saved to %s", config_path)


# ---------------------------------------------------------------------------
# Selection logic
# ---------------------------------------------------------------------------
def select_next_wallet(pool: EOAPool, chain: str, cooldown: int = DEFAULT_COOLDOWN_SECONDS) -> Wallet:
    """
    Select the next eligible wallet in round-robin order.

    Rules:
      1. Never reuse a wallet within `cooldown` seconds.
      2. Prefer the wallet with the lowest use_count.
      3. Skip excluded wallets.
      4. Raise RuntimeError if no wallet is eligible.
    """
    wallets = pool.wallets or []
    if not wallets:
        raise RuntimeError("EOA pool is empty")

    now = int(time.time())
    eligible: list[Wallet] = []

    for w in wallets:
        if w.excluded:
            continue
        if w.last_used == 0:
            eligible.append(w)
        elif (now - w.last_used) >= cooldown:
            eligible.append(w)

    if not eligible:
        raise RuntimeError(
            f"No eligible wallets for chain={chain} (all {len(wallets)} are within {cooldown}s cooldown)"
        )

    # Prefer least-used, then oldest last_used
    selected = min(eligible, key=lambda w: (w.use_count, w.last_used))
    logger.info(
        "Selected wallet %s (label=%s, use_count=%d, last_used=%s)",
        selected.address,
        selected.label,
        selected.use_count,
        selected.last_used,
    )
    return selected


def update_last_used(wallet: Wallet) -> None:
    """Mark a wallet as just used."""
    wallet.last_used = int(time.time())
    wallet.use_count += 1
    logger.info("Updated wallet %s last_used=%d use_count=%d", wallet.address, wallet.last_used, wallet.use_count)


def check_balance_requirements(wallet: Wallet, chain: str, min_eth: float) -> bool:
    """
    Verify the wallet holds at least `min_eth` on the target chain.

    NOTE: This is a static balance check from the config file.
    For a live check, integrate web3 here.
    """
    balances = wallet.chain_balances or {}
    balance_wei_str = balances.get(chain, "0")
    try:
        balance_wei = int(balance_wei_str)
    except ValueError:
        logger.error("Invalid balance value for %s on %s: %s", wallet.address, chain, balance_wei_str)
        return False

    balance_eth = balance_wei / 1e18
    if balance_eth < min_eth:
        logger.warning(
            "Wallet %s balance %.6f ETH on %s is below minimum %.6f ETH",
            wallet.address,
            balance_eth,
            chain,
            min_eth,
        )
        return False

    logger.info("Wallet %s balance %.6f ETH on %s OK", wallet.address, balance_eth, chain)
    return True


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Rotate through a pool of EOAs for safe transaction signing."
    )
    parser.add_argument(
        "--config",
        required=True,
        help="Path to eoa_pool.json.",
    )
    parser.add_argument(
        "--chain",
        required=True,
        choices=["base", "arbitrum"],
        help="Target chain (selects balance field).",
    )
    parser.add_argument(
        "--min-eth",
        type=float,
        default=DEFAULT_MIN_ETH,
        help=f"Minimum ETH balance required (default {DEFAULT_MIN_ETH}).",
    )
    parser.add_argument(
        "--cooldown-seconds",
        type=int,
        default=DEFAULT_COOLDOWN_SECONDS,
        help=f"Cooldown between reuses in seconds (default {DEFAULT_COOLDOWN_SECONDS}).",
    )
    parser.add_argument(
        "--output-format",
        default="address",
        choices=["address", "json"],
        help="Output format.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Do not update last_used timestamp.",
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

    pool = load_pool(args.config)

    try:
        wallet = select_next_wallet(pool, args.chain, cooldown=args.cooldown_seconds)
    except RuntimeError as exc:
        logger.error("Wallet selection failed: %s", exc)
        return 1

    if not check_balance_requirements(wallet, args.chain, args.min_eth):
        logger.error("Balance check failed for %s", wallet.address)
        return 1

    if not args.dry_run:
        update_last_used(wallet)
        save_pool(pool, args.config)
    else:
        logger.info("Dry-run: skipping state update")

    if args.output_format == "json":
        print(
            json.dumps(
                {
                    "address": wallet.address,
                    "label": wallet.label,
                    "chain": args.chain,
                    "use_count": wallet.use_count,
                    "last_used": wallet.last_used,
                }
            )
        )
    else:
        print(wallet.address)

    return 0


if __name__ == "__main__":
    sys.exit(main())
