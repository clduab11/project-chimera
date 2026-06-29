#!/usr/bin/env python3
"""
check_balances.py
Project Chimera - EOA Pool Balance Inspector

Queries native ETH (and optionally ERC20) balances for every non-excluded
wallet in config/eoa_pool.json and prints a table flagging any wallet whose
native balance is below a configurable floor. Designed for health gating:
exits non-zero if any wallet is LOW, so it can be chained in operator checks.

INVARIANT (AGENTS.md #5): imports without web3.py. `web3` is optional; when it
is unavailable, or when `--mock` is passed, the script runs fully offline and
reports balances as "mock". `--help` always works without web3.

Usage:
  python scripts/check_balances.py --mock
  python scripts/check_balances.py --rpc https://mainnet.base.org --chain base --min-balance 0.01
  python scripts/check_balances.py --rpc $BASE_RPC_URL --token 0x833589... --json
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# Optional dependency. Never required for import or for --help (Invariant #5).
try:
    from web3 import Web3
except ImportError:  # pragma: no cover - environment dependent
    Web3 = None  # type: ignore[assignment]

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("check_balances")

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
DEFAULT_EOA_POOL: str = "config/eoa_pool.json"
DEFAULT_MIN_BALANCE_ETH: float = 0.0
WEI_PER_ETH: int = 10**18

# Minimal ERC20 ABI fragment (balanceOf + decimals + symbol).
ERC20_ABI = [
    {
        "constant": True,
        "inputs": [{"name": "_owner", "type": "address"}],
        "name": "balanceOf",
        "outputs": [{"name": "balance", "type": "uint256"}],
        "type": "function",
    },
    {
        "constant": True,
        "inputs": [],
        "name": "decimals",
        "outputs": [{"name": "", "type": "uint8"}],
        "type": "function",
    },
    {
        "constant": True,
        "inputs": [],
        "name": "symbol",
        "outputs": [{"name": "", "type": "string"}],
        "type": "function",
    },
]


# ---------------------------------------------------------------------------
# Data models
# ---------------------------------------------------------------------------
@dataclass
class WalletBalance:
    address: str
    label: str | None
    excluded: bool
    eth_balance: float | None  # None == mock/unknown
    low: bool = False
    tokens: dict[str, str] = field(default_factory=dict)  # token addr -> human balance

    def to_dict(self) -> dict[str, Any]:
        return {
            "address": self.address,
            "label": self.label,
            "excluded": self.excluded,
            "eth_balance": "mock" if self.eth_balance is None else f"{self.eth_balance:.6f}",
            "low": self.low,
            "tokens": self.tokens,
        }


# ---------------------------------------------------------------------------
# Pool loading
# ---------------------------------------------------------------------------
def load_wallets(pool_path: str) -> list[dict[str, Any]]:
    """Load wallet entries from eoa_pool.json. Raises on missing/corrupt file."""
    path = Path(pool_path)
    if not path.exists():
        raise FileNotFoundError(f"EOA pool not found at {pool_path}")
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    wallets = data.get("wallets", [])
    if not isinstance(wallets, list):
        raise ValueError("eoa_pool.json 'wallets' must be a list")
    logger.info("Loaded %d wallets from %s", len(wallets), pool_path)
    return wallets


# ---------------------------------------------------------------------------
# Live balance fetching (web3-gated)
# ---------------------------------------------------------------------------
def make_web3(rpc: str) -> Any:
    """Construct a Web3 HTTP client. Caller must ensure web3 is importable."""
    if Web3 is None:  # pragma: no cover - guarded by callers
        raise RuntimeError("web3 is not installed")
    w3 = Web3(Web3.HTTPProvider(rpc, request_kwargs={"timeout": 15}))
    if not w3.is_connected():
        raise ConnectionError(f"Unable to connect to RPC at {rpc}")
    return w3


def fetch_eth_balance(w3: Any, address: str) -> float:
    """Return native balance in ether for an address."""
    checksum = w3.to_checksum_address(address)
    wei = w3.eth.get_balance(checksum)
    return wei / WEI_PER_ETH


def fetch_token_balance(w3: Any, token_addr: str, holder: str) -> str:
    """Return a human-readable ERC20 balance string 'amount SYMBOL'."""
    token = w3.eth.contract(address=w3.to_checksum_address(token_addr), abi=ERC20_ABI)
    raw = token.functions.balanceOf(w3.to_checksum_address(holder)).call()
    try:
        decimals = token.functions.decimals().call()
    except Exception:
        decimals = 18
    try:
        symbol = token.functions.symbol().call()
    except Exception:
        symbol = token_addr[:8]
    human = raw / (10**decimals)
    return f"{human:.6f} {symbol}"


# ---------------------------------------------------------------------------
# Core
# ---------------------------------------------------------------------------
def collect_balances(
    wallets: list[dict[str, Any]],
    rpc: str | None,
    tokens: list[str],
    min_balance: float,
    mock: bool,
) -> list[WalletBalance]:
    """Build a balance report for every wallet. Offline-safe when mock/no web3."""
    offline = mock or Web3 is None or not rpc
    if offline:
        if mock:
            logger.info("Running in --mock mode: balances reported as 'mock'")
        elif Web3 is None:
            logger.warning("web3 not installed: balances reported as 'mock' (offline)")
        elif not rpc:
            logger.warning("No --rpc provided: balances reported as 'mock' (offline)")

    results: list[WalletBalance] = []
    w3 = None
    if not offline:
        w3 = make_web3(rpc)  # type: ignore[arg-type]

    for w in wallets:
        address = w.get("address", "?")
        label = w.get("label")
        excluded = bool(w.get("excluded", False))

        if excluded:
            logger.debug("Skipping excluded wallet %s", address)
            results.append(WalletBalance(address, label, excluded, None))
            continue

        if offline:
            results.append(WalletBalance(address, label, excluded, None))
            continue

        try:
            eth_balance = fetch_eth_balance(w3, address)
        except Exception as exc:
            logger.error("Failed to fetch ETH balance for %s: %s", address, exc)
            results.append(WalletBalance(address, label, excluded, None))
            continue

        low = eth_balance < min_balance
        wb = WalletBalance(address, label, excluded, eth_balance, low=low)

        for token_addr in tokens:
            try:
                wb.tokens[token_addr] = fetch_token_balance(w3, token_addr, address)
            except Exception as exc:
                logger.error("Failed ERC20 %s for %s: %s", token_addr, address, exc)
                wb.tokens[token_addr] = "error"

        results.append(wb)

    return results


def print_table(results: list[WalletBalance], chain: str, min_balance: float) -> None:
    """Pretty-print a balance table to stdout."""
    print(f"\nChimera EOA balances (chain={chain}, min={min_balance:.6f} ETH)")
    print("-" * 78)
    print(f"{'ADDRESS':<44}{'LABEL':<18}{'ETH':>10}  FLAG")
    print("-" * 78)
    for r in results:
        if r.excluded:
            eth_str = "excluded"
            flag = "-"
        elif r.eth_balance is None:
            eth_str = "mock"
            flag = "-"
        else:
            eth_str = f"{r.eth_balance:.6f}"
            flag = "LOW" if r.low else "ok"
        label = (r.label or "")[:17]
        print(f"{r.address:<44}{label:<18}{eth_str:>10}  {flag}")
        for token_addr, bal in r.tokens.items():
            print(f"    token {token_addr}: {bal}")
    print("-" * 78)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Query ETH/ERC20 balances for the Chimera EOA pool."
    )
    parser.add_argument("--rpc", default=None, help="RPC endpoint URL (omit for offline/mock).")
    parser.add_argument(
        "--eoa-pool",
        default=DEFAULT_EOA_POOL,
        help=f"Path to eoa_pool.json (default {DEFAULT_EOA_POOL}).",
    )
    parser.add_argument(
        "--chain",
        default="base",
        choices=["base", "arbitrum"],
        help="Target chain (used for labeling only).",
    )
    parser.add_argument(
        "--token",
        action="append",
        default=[],
        metavar="ADDR",
        help="ERC20 token address to also query (repeatable).",
    )
    parser.add_argument(
        "--min-balance",
        type=float,
        default=DEFAULT_MIN_BALANCE_ETH,
        help="Minimum ETH balance; wallets below are flagged LOW (default 0.0).",
    )
    parser.add_argument("--json", action="store_true", help="Emit machine-readable JSON.")
    parser.add_argument(
        "--mock",
        action="store_true",
        help="Offline mode: print pool addresses with 'mock' balances.",
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
        wallets = load_wallets(args.eoa_pool)
    except (FileNotFoundError, ValueError, json.JSONDecodeError) as exc:
        logger.error("Failed to load EOA pool: %s", exc)
        return 1

    try:
        results = collect_balances(
            wallets,
            rpc=args.rpc,
            tokens=args.token,
            min_balance=args.min_balance,
            mock=args.mock,
        )
    except (ConnectionError, RuntimeError) as exc:
        logger.error("Balance collection failed: %s", exc)
        return 1

    if args.json:
        print(
            json.dumps(
                {
                    "chain": args.chain,
                    "min_balance": args.min_balance,
                    "wallets": [r.to_dict() for r in results],
                },
                indent=2,
            )
        )
    else:
        print_table(results, args.chain, args.min_balance)

    low_wallets = [r for r in results if r.low and not r.excluded]
    if low_wallets:
        logger.warning("%d wallet(s) below minimum balance", len(low_wallets))
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
