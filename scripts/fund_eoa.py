#!/usr/bin/env python3
"""
fund_eoa.py
Project Chimera - Treasury -> Worker EOA Funding Script

Tops up under-funded worker EOAs from a treasury account. Reads the worker
pool from config/eoa_pool.json (schema v2.0: a `wallets` array of objects,
each with an `address` and an `excluded` flag), checks each wallet's native
balance, and sends a fixed top-up to any wallet below the configured floor.

Nonce safety: the treasury nonce is fetched exactly ONCE before the send loop
and incremented locally per dispatched transaction. This avoids the classic
nonce-collision bug where calling get_transaction_count() per send (before
prior txs are mined) returns the same nonce repeatedly.

Usage:
  python scripts/fund_eoa.py --treasury-key 0x... --rpc https://mainnet.base.org
  python scripts/fund_eoa.py --treasury-key 0x... --rpc https://... --wait
  python scripts/fund_eoa.py --treasury-key x --rpc x --dry-run-offline   # no web3 needed

SECURITY: the treasury private key is NEVER logged. Prefer passing it via an
environment-sourced argument in production rather than shell history.
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
from pathlib import Path
from typing import Any

try:
    from web3 import Web3
except ImportError:  # web3 is an optional dependency (AGENTS.md invariant #5)
    Web3 = None

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("fund_eoa")


# ---------------------------------------------------------------------------
# Pool loading
# ---------------------------------------------------------------------------
def load_wallet_addresses(eoa_pool_path: str) -> list[str]:
    """Load non-excluded wallet addresses from the v2.0 eoa_pool.json.

    The schema uses the key `wallets` (NOT `workers`) and each entry is an
    object with an `address` field and an `excluded` flag.
    """
    path = Path(eoa_pool_path)
    if not path.exists():
        raise FileNotFoundError(f"EOA pool file not found: {eoa_pool_path}")

    try:
        with open(path, "r", encoding="utf-8") as f:
            pool = json.load(f)
    except json.JSONDecodeError as exc:
        raise ValueError(f"Malformed EOA pool JSON {eoa_pool_path}: {exc}") from exc

    wallets = pool.get("wallets")
    if not isinstance(wallets, list):
        raise ValueError(
            f"EOA pool {eoa_pool_path} missing 'wallets' array (got version "
            f"{pool.get('version')!r})"
        )

    addresses: list[str] = []
    for entry in wallets:
        if not isinstance(entry, dict):
            logger.warning("Skipping malformed wallet entry (not an object): %r", entry)
            continue
        if entry.get("excluded"):
            logger.debug("Skipping excluded wallet %s", entry.get("address"))
            continue
        address = entry.get("address")
        if not address:
            logger.warning("Skipping wallet entry with no address: %r", entry)
            continue
        addresses.append(address)

    return addresses


# ---------------------------------------------------------------------------
# Funding logic
# ---------------------------------------------------------------------------
def fund_wallets(
    treasury_key: str,
    rpc: str,
    eoa_pool_path: str,
    min_balance: float = 0.01,
    fund_amount: float = 0.02,
    wait: bool = False,
    dry_run: bool = False,
) -> int:
    """Fund under-funded worker EOAs. Returns the number of wallets funded."""
    if Web3 is None:
        logger.error(
            "web3 is not installed; cannot fund wallets. Install web3 or use "
            "--dry-run-offline to preview without network access."
        )
        return -1

    addresses = load_wallet_addresses(eoa_pool_path)
    logger.info("Loaded %d fundable wallet(s) from %s", len(addresses), eoa_pool_path)

    w3 = Web3(Web3.HTTPProvider(rpc, request_kwargs={"timeout": 60}))
    try:
        connected = w3.is_connected()
    except Exception as exc:  # noqa: BLE001 - surface any provider error cleanly
        logger.error("Failed to reach RPC %s: %s", rpc, exc)
        return -1
    if not connected:
        logger.error("Could not connect to RPC endpoint: %s", rpc)
        return -1

    treasury = w3.eth.account.from_key(treasury_key)
    logger.info("Treasury address: %s", treasury.address)  # address only, never the key

    min_wei = Web3.to_wei(min_balance, "ether")
    fund_wei = Web3.to_wei(fund_amount, "ether")

    # NONCE-COLLISION FIX: fetch the treasury nonce ONCE up-front, then use a
    # locally incremented counter for each successfully dispatched transaction.
    try:
        start_nonce = w3.eth.get_transaction_count(treasury.address)
    except Exception as exc:  # noqa: BLE001
        logger.error("Failed to fetch treasury nonce: %s", exc)
        return -1
    logger.info("Treasury starting nonce: %d", start_nonce)

    try:
        gas_price = w3.eth.gas_price
    except Exception as exc:  # noqa: BLE001
        logger.error("Failed to fetch gas price: %s", exc)
        return -1

    sent_count = 0
    tx_hashes: list[str] = []

    for address in addresses:
        try:
            checksum = Web3.to_checksum_address(address)
            balance = w3.eth.get_balance(checksum)
            if balance >= min_wei:
                logger.debug("%s funded (%s wei) - skipping", checksum, balance)
                continue

            nonce = start_nonce + sent_count  # local increment, no per-send RPC
            if dry_run:
                logger.info(
                    "[dry-run] would fund %s with %s wei (nonce=%d, current=%s)",
                    checksum,
                    fund_wei,
                    nonce,
                    balance,
                )
                sent_count += 1
                continue

            tx = {
                "to": checksum,
                "value": fund_wei,
                "gas": 21000,
                "gasPrice": gas_price,
                "nonce": nonce,
                "chainId": w3.eth.chain_id,
            }
            signed = treasury.sign_transaction(tx)
            raw = getattr(signed, "rawTransaction", None) or signed.raw_transaction
            tx_hash = w3.eth.send_raw_transaction(raw)
            tx_hex = tx_hash.hex()
            tx_hashes.append(tx_hex)
            sent_count += 1
            logger.info("Funded %s -> tx %s (nonce=%d)", checksum, tx_hex, nonce)
        except Exception as exc:  # noqa: BLE001 - isolate per-wallet failures
            logger.error("Failed to fund %s: %s", address, exc)
            continue

    if wait and tx_hashes and not dry_run:
        logger.info("Waiting for %d receipt(s)...", len(tx_hashes))
        for tx_hex in tx_hashes:
            try:
                receipt = w3.eth.wait_for_transaction_receipt(tx_hex, timeout=180)
                logger.info("Receipt %s status=%s", tx_hex, receipt.status)
            except Exception as exc:  # noqa: BLE001
                logger.error("Timed out / failed waiting for %s: %s", tx_hex, exc)

    logger.info("Funded %d wallet(s)", sent_count)
    return sent_count


def dry_run_offline(eoa_pool_path: str, min_balance: float, fund_amount: float) -> int:
    """Preview funding intent without web3 / network access.

    On-chain balances cannot be read offline, so every non-excluded wallet is
    reported as a funding candidate. Returns the candidate count.
    """
    addresses = load_wallet_addresses(eoa_pool_path)
    logger.info(
        "[offline] %d fundable wallet(s); min_balance=%s ETH, fund_amount=%s ETH",
        len(addresses),
        min_balance,
        fund_amount,
    )
    for address in addresses:
        logger.info("[offline] candidate: %s (would top up to >= %s ETH)", address, min_balance)
    return len(addresses)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Fund under-funded Chimera worker EOAs from the treasury."
    )
    parser.add_argument("--treasury-key", required=True, help="Treasury private key (NEVER logged).")
    parser.add_argument("--rpc", required=True, help="RPC endpoint URL.")
    parser.add_argument("--eoa-pool", default="config/eoa_pool.json", help="Path to eoa_pool.json.")
    parser.add_argument(
        "--min-balance",
        type=float,
        default=0.01,
        help="Minimum balance (ether) a wallet must hold to be skipped.",
    )
    parser.add_argument(
        "--fund-amount",
        type=float,
        default=0.02,
        help="Amount (ether) to send to each under-funded wallet.",
    )
    parser.add_argument("--wait", action="store_true", help="Wait for transaction receipts.")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Compute and log intended funding without sending (still needs RPC for balances).",
    )
    parser.add_argument(
        "--dry-run-offline",
        action="store_true",
        help="Preview funding candidates with no web3 / RPC (imports and runs without web3).",
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
        if args.dry_run_offline:
            dry_run_offline(args.eoa_pool, args.min_balance, args.fund_amount)
            return 0
    except (FileNotFoundError, ValueError) as exc:
        logger.error("%s", exc)
        return 1

    if Web3 is None:
        logger.error(
            "web3 is not installed and --dry-run-offline was not requested. "
            "Install web3.py or rerun with --dry-run-offline."
        )
        return 2

    try:
        result = fund_wallets(
            treasury_key=args.treasury_key,
            rpc=args.rpc,
            eoa_pool_path=args.eoa_pool,
            min_balance=args.min_balance,
            fund_amount=args.fund_amount,
            wait=args.wait,
            dry_run=args.dry_run,
        )
    except (FileNotFoundError, ValueError) as exc:
        logger.error("%s", exc)
        return 1

    if result < 0:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
