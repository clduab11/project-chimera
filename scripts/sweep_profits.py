#!/usr/bin/env python3
"""
sweep_profits.py
Project Chimera - Worker EOA -> Treasury Profit Sweeper

Consolidates funds from worker EOAs back to the treasury. Supports:
  * Native ETH sweep: sends (balance - min_keep) from each worker, leaving a
    small gas reserve.
  * ERC20 sweep: for each --token, reads balanceOf(worker) and transfers the
    full balance to the treasury using a minimal inline ERC20 ABI. USDT-style
    tokens (which return no bool from transfer) are handled by NOT asserting a
    return value and instead checking the mined transaction receipt status.

Worker keys:
  Private keys MUST NOT be passed on the CLI in production (they leak into
  shell history and process listings). Provide them via --keys-file: a file
  with one private key per line (blank lines and `#` comments ignored). Use a
  gitignored extension/path such as *.key or private_keys/ (both are already
  in .gitignore). A single --worker-key is supported for ad-hoc/testing use.

Usage:
  python scripts/sweep_profits.py --rpc https://... --treasury 0xTreasury --keys-file workers.key
  python scripts/sweep_profits.py --rpc https://... --treasury 0xT --keys-file w.key \
      --token 0xUSDC --token 0xUSDT --wait
  python scripts/sweep_profits.py --dry-run-offline --treasury 0xT --keys-file w.key

SECURITY: private keys are NEVER logged (only derived addresses are).
"""

from __future__ import annotations

import argparse
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
logger = logging.getLogger("sweep_profits")

# Minimal ERC20 ABI: balanceOf, transfer, decimals, symbol.
ERC20_ABI: list[dict[str, Any]] = [
    {
        "constant": True,
        "inputs": [{"name": "owner", "type": "address"}],
        "name": "balanceOf",
        "outputs": [{"name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "constant": False,
        "inputs": [
            {"name": "to", "type": "address"},
            {"name": "value", "type": "uint256"},
        ],
        "name": "transfer",
        # USDT omits a return value; declaring bool here is tolerated, but we
        # never assert on it - receipt.status is the source of truth.
        "outputs": [{"name": "", "type": "bool"}],
        "stateMutability": "nonpayable",
        "type": "function",
    },
    {
        "constant": True,
        "inputs": [],
        "name": "decimals",
        "outputs": [{"name": "", "type": "uint8"}],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "constant": True,
        "inputs": [],
        "name": "symbol",
        "outputs": [{"name": "", "type": "string"}],
        "stateMutability": "view",
        "type": "function",
    },
]


# ---------------------------------------------------------------------------
# Key loading
# ---------------------------------------------------------------------------
def load_worker_keys(keys_file: str | None, worker_key: str | None) -> list[str]:
    """Collect worker private keys from a keys-file and/or a single --worker-key."""
    keys: list[str] = []

    if keys_file:
        path = Path(keys_file)
        if not path.exists():
            raise FileNotFoundError(f"Keys file not found: {keys_file}")
        with open(path, "r", encoding="utf-8") as f:
            for line in f:
                stripped = line.strip()
                if not stripped or stripped.startswith("#"):
                    continue
                keys.append(stripped)

    if worker_key:
        keys.append(worker_key.strip())

    return keys


# ---------------------------------------------------------------------------
# Native ETH sweep
# ---------------------------------------------------------------------------
def sweep_native(
    w3: "Web3",
    acct: Any,
    treasury: str,
    min_keep_wei: int,
    gas_price: int,
    chain_id: int,
    dry_run: bool,
) -> str | None:
    """Sweep native ETH from one worker, leaving min_keep_wei for gas."""
    balance = w3.eth.get_balance(acct.address)
    gas_cost = 21000 * gas_price
    # Leave both the requested reserve and the gas needed for this send.
    spendable = balance - min_keep_wei - gas_cost
    if spendable <= 0:
        logger.info("%s: nothing to sweep (balance=%s wei)", acct.address, balance)
        return None

    if dry_run:
        logger.info("[dry-run] %s -> %s: %s wei native", acct.address, treasury, spendable)
        return None

    nonce = w3.eth.get_transaction_count(acct.address)
    tx = {
        "to": Web3.to_checksum_address(treasury),
        "value": spendable,
        "gas": 21000,
        "gasPrice": gas_price,
        "nonce": nonce,
        "chainId": chain_id,
    }
    signed = acct.sign_transaction(tx)
    raw = getattr(signed, "rawTransaction", None) or signed.raw_transaction
    tx_hash = w3.eth.send_raw_transaction(raw)
    tx_hex = tx_hash.hex()
    logger.info("Swept %s wei native from %s -> tx %s", spendable, acct.address, tx_hex)
    return tx_hex


# ---------------------------------------------------------------------------
# ERC20 sweep
# ---------------------------------------------------------------------------
def sweep_erc20(
    w3: "Web3",
    acct: Any,
    treasury: str,
    token_address: str,
    gas_price: int,
    chain_id: int,
    dry_run: bool,
) -> str | None:
    """Sweep the full ERC20 balance of one worker to the treasury."""
    token = w3.eth.contract(
        address=Web3.to_checksum_address(token_address),
        abi=ERC20_ABI,
    )

    try:
        symbol = token.functions.symbol().call()
    except Exception:  # noqa: BLE001 - symbol() is optional/decorative
        symbol = token_address[:10]

    balance = token.functions.balanceOf(acct.address).call()
    if balance <= 0:
        logger.info("%s: no %s balance to sweep", acct.address, symbol)
        return None

    if dry_run:
        logger.info(
            "[dry-run] %s -> %s: %s %s (raw units)",
            acct.address,
            treasury,
            balance,
            symbol,
        )
        return None

    dest = Web3.to_checksum_address(treasury)
    nonce = w3.eth.get_transaction_count(acct.address)

    transfer_fn = token.functions.transfer(dest, balance)
    try:
        gas_estimate = transfer_fn.estimate_gas({"from": acct.address})
        gas_limit = int(gas_estimate * 1.2)
    except Exception as exc:  # noqa: BLE001 - fall back to a safe default
        logger.warning("Gas estimate failed for %s on %s: %s; using 120000", symbol, acct.address, exc)
        gas_limit = 120000

    tx = transfer_fn.build_transaction(
        {
            "from": acct.address,
            "gas": gas_limit,
            "gasPrice": gas_price,
            "nonce": nonce,
            "chainId": chain_id,
        }
    )
    signed = acct.sign_transaction(tx)
    raw = getattr(signed, "rawTransaction", None) or signed.raw_transaction
    tx_hash = w3.eth.send_raw_transaction(raw)
    tx_hex = tx_hash.hex()
    # USDT-style: do NOT rely on a bool return; confirm via receipt status.
    receipt = w3.eth.wait_for_transaction_receipt(tx_hex, timeout=180)
    if receipt.status == 1:
        logger.info("Swept %s %s from %s -> tx %s", balance, symbol, acct.address, tx_hex)
    else:
        logger.error("ERC20 transfer reverted (status=0) for %s on %s, tx %s", symbol, acct.address, tx_hex)
    return tx_hex


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------
def run_sweep(
    rpc: str,
    treasury: str,
    keys: list[str],
    min_keep: float,
    tokens: list[str],
    wait: bool,
    dry_run: bool,
) -> int:
    """Sweep all provided workers. Returns the number of transactions sent."""
    if Web3 is None:
        logger.error("web3 is not installed; cannot sweep. Use --dry-run-offline to preview.")
        return -1

    w3 = Web3(Web3.HTTPProvider(rpc, request_kwargs={"timeout": 60}))
    try:
        connected = w3.is_connected()
    except Exception as exc:  # noqa: BLE001
        logger.error("Failed to reach RPC %s: %s", rpc, exc)
        return -1
    if not connected:
        logger.error("Could not connect to RPC endpoint: %s", rpc)
        return -1

    try:
        gas_price = w3.eth.gas_price
        chain_id = w3.eth.chain_id
    except Exception as exc:  # noqa: BLE001
        logger.error("Failed to read gas price / chain id: %s", exc)
        return -1

    min_keep_wei = Web3.to_wei(min_keep, "ether")
    sent = 0
    summary: list[str] = []

    for key in keys:
        try:
            acct = w3.eth.account.from_key(key)
        except Exception as exc:  # noqa: BLE001 - never log the key itself
            logger.error("Skipping invalid worker key (index %d): %s", keys.index(key), exc)
            continue

        if tokens:
            for token_address in tokens:
                try:
                    tx_hex = sweep_erc20(
                        w3, acct, treasury, token_address, gas_price, chain_id, dry_run
                    )
                    if tx_hex:
                        sent += 1
                        summary.append(f"{acct.address} {token_address} -> {tx_hex}")
                except Exception as exc:  # noqa: BLE001 - isolate per-token failures
                    logger.error("ERC20 sweep failed for %s token %s: %s", acct.address, token_address, exc)
        else:
            try:
                tx_hex = sweep_native(
                    w3, acct, treasury, min_keep_wei, gas_price, chain_id, dry_run
                )
                if tx_hex:
                    sent += 1
                    summary.append(f"{acct.address} ETH -> {tx_hex}")
            except Exception as exc:  # noqa: BLE001 - isolate per-wallet failures
                logger.error("Native sweep failed for %s: %s", acct.address, exc)

    if wait and not dry_run and summary:
        logger.info("Waiting for sweep receipts...")
        for line in summary:
            tx_hex = line.rsplit("-> ", 1)[-1]
            try:
                receipt = w3.eth.wait_for_transaction_receipt(tx_hex, timeout=180)
                logger.info("Receipt %s status=%s", tx_hex, receipt.status)
            except Exception as exc:  # noqa: BLE001
                logger.error("Failed waiting for %s: %s", tx_hex, exc)

    logger.info("Sweep summary (%d tx):", sent)
    for line in summary:
        logger.info("  %s", line)

    return sent


def dry_run_offline(keys: list[str], treasury: str, tokens: list[str]) -> int:
    """Preview sweep targets with no web3 / RPC. Derives addresses only if web3
    is available; otherwise reports key count without exposing the keys."""
    logger.info("[offline] treasury destination: %s", treasury)
    if tokens:
        logger.info("[offline] tokens to sweep: %s", ", ".join(tokens))
    else:
        logger.info("[offline] native ETH sweep")

    if Web3 is not None:
        for idx, key in enumerate(keys):
            try:
                acct = Web3().eth.account.from_key(key)
                logger.info("[offline] worker %d: %s", idx, acct.address)
            except Exception as exc:  # noqa: BLE001 - never log the key
                logger.warning("[offline] worker %d: invalid key (%s)", idx, exc)
    else:
        logger.info("[offline] %d worker key(s) loaded (web3 unavailable; addresses not derived)", len(keys))

    return len(keys)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Sweep profits from worker EOAs back to the Chimera treasury."
    )
    parser.add_argument("--rpc", help="RPC endpoint URL (required unless --dry-run-offline).")
    parser.add_argument("--treasury", required=True, help="Destination treasury address.")
    parser.add_argument(
        "--eoa-pool",
        default="config/eoa_pool.json",
        help="Path to eoa_pool.json (reference only; sweeping requires keys).",
    )
    parser.add_argument(
        "--keys-file",
        default=None,
        help="File with one worker private key per line (gitignored; e.g. *.key). "
        "Keys must NOT be passed on the CLI in production.",
    )
    parser.add_argument(
        "--worker-key",
        default=None,
        help="Single worker private key (ad-hoc/testing only; avoid in production).",
    )
    parser.add_argument(
        "--min-keep",
        type=float,
        default=0.005,
        help="Native ether to leave in each worker for gas (default 0.005).",
    )
    parser.add_argument(
        "--token",
        action="append",
        default=[],
        dest="tokens",
        help="ERC20 token address to sweep (repeatable). If omitted, sweeps native ETH.",
    )
    parser.add_argument(
        "--keystore-dir",
        default=None,
        help="Reserved: directory of encrypted keystores (not yet implemented).",
    )
    parser.add_argument("--wait", action="store_true", help="Wait for transaction receipts.")
    parser.add_argument("--dry-run", action="store_true", help="Compute and log intended sweeps without sending.")
    parser.add_argument(
        "--dry-run-offline",
        action="store_true",
        help="Preview targets with no web3 / RPC (imports and runs without web3).",
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

    if args.keystore_dir:
        logger.warning("--keystore-dir is reserved and not yet implemented; ignoring.")

    try:
        keys = load_worker_keys(args.keys_file, args.worker_key)
    except FileNotFoundError as exc:
        logger.error("%s", exc)
        return 1

    if not keys:
        logger.error("No worker keys provided. Use --keys-file or --worker-key.")
        return 1

    if args.dry_run_offline:
        dry_run_offline(keys, args.treasury, args.tokens)
        return 0

    if Web3 is None:
        logger.error(
            "web3 is not installed and --dry-run-offline was not requested. "
            "Install web3.py or rerun with --dry-run-offline."
        )
        return 2

    if not args.rpc:
        logger.error("--rpc is required unless --dry-run-offline is used.")
        return 1

    result = run_sweep(
        rpc=args.rpc,
        treasury=args.treasury,
        keys=keys,
        min_keep=args.min_keep,
        tokens=args.tokens,
        wait=args.wait,
        dry_run=args.dry_run,
    )

    if result < 0:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
