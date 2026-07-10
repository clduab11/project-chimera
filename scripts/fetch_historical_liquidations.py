#!/usr/bin/env python3
"""
fetch_historical_liquidations.py
Project Chimera - Historical Liquidation Event Fetcher

Queries Aave V3 Pool contracts for LiquidationCall events on Base/Arbitrum,
reconstructs liquidation parameters, and writes a JSON fixture compatible
with tests/fixtures/golden_replays.json.

Usage:
  python scripts/fetch_historical_liquidations.py \
      --chain base \
      --from-block 18500000 \
      --to-block 18501000 \
      --output tests/fixtures/golden_replays.json
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import time
from dataclasses import asdict, dataclass
from decimal import Decimal
from pathlib import Path
from typing import Any

try:
    from web3 import Web3
    from web3.middleware import geth_poa_middleware
except ImportError:  # web3 is an optional dependency (AGENTS.md invariant #5)
    Web3 = None
    geth_poa_middleware = None

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("fetch_historical_liquidations")

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
RPC_URLS: dict[str, str] = {
    "base": os.getenv("BASE_RPC_URL", "https://mainnet.base.org"),
    "arbitrum": os.getenv("ARB_RPC_URL", "https://arb1.arbitrum.io/rpc"),
}

# Aave V3 Pool ABI - LiquidationCall event + supporting functions
POOL_ABI: list[dict[str, Any]] = [
    {
        "anonymous": False,
        "inputs": [
            {
                "indexed": True,
                "internalType": "address",
                "name": "collateralAsset",
                "type": "address",
            },
            {
                "indexed": True,
                "internalType": "address",
                "name": "debtAsset",
                "type": "address",
            },
            {
                "indexed": True,
                "internalType": "address",
                "name": "user",
                "type": "address",
            },
            {
                "indexed": False,
                "internalType": "uint256",
                "name": "debtToCover",
                "type": "uint256",
            },
            {
                "indexed": False,
                "internalType": "uint256",
                "name": "liquidatedCollateralAmount",
                "type": "uint256",
            },
            {
                "indexed": False,
                "internalType": "address",
                "name": "liquidator",
                "type": "address",
            },
            {
                "indexed": False,
                "internalType": "bool",
                "name": "receiveAToken",
                "type": "bool",
            },
        ],
        "name": "LiquidationCall",
        "type": "event",
    },
    {
        "inputs": [{"internalType": "address", "name": "asset", "type": "address"}],
        "name": "getReserveData",
        "outputs": [
            {"internalType": "uint256", "name": "configuration", "type": "uint256"},
            {"internalType": "uint128", "name": "liquidityIndex", "type": "uint128"},
            {"internalType": "uint128", "name": "currentLiquidityRate", "type": "uint128"},
            {"internalType": "uint128", "name": "variableBorrowIndex", "type": "uint128"},
            {"internalType": "uint128", "name": "currentVariableBorrowRate", "type": "uint128"},
            {"internalType": "uint128", "name": "currentStableBorrowRate", "type": "uint128"},
            {"internalType": "uint40", "name": "lastUpdateTimestamp", "type": "uint40"},
            {"internalType": "uint16", "name": "id", "type": "uint16"},
            {"internalType": "address", "name": "aTokenAddress", "type": "address"},
            {"internalType": "address", "name": "stableDebtTokenAddress", "type": "address"},
            {"internalType": "address", "name": "variableDebtTokenAddress", "type": "address"},
            {"internalType": "address", "name": "interestRateStrategyAddress", "type": "address"},
            {"internalType": "uint128", "name": "accruedToTreasury", "type": "uint128"},
            {"internalType": "uint128", "name": "unbacked", "type": "uint128"},
            {"internalType": "uint128", "name": "isolationModeTotalDebt", "type": "uint128"},
        ],
        "stateMutability": "view",
        "type": "function",
    },
]

# Known Aave V3 Pool addresses
DEFAULT_POOLS: dict[str, str] = {
    "base": "0xA238Dd80C259a72e81d7e4664a9801593F98d1c5",
    "arbitrum": "0x794a61358D6845594F94dc1DB02A252b5b4814aD",  # verify on docs.aave.com
}

# Fallback ETH/USD price used when the Chainlink feed is unreachable.
# Matches the pacing.yaml fallback so historical replay estimates stay consistent.
ETH_PRICE_USD_FALLBACK: Decimal = Decimal("1800.0")

# Chainlink ETH/USD proxy aggregator addresses (AggregatorV3Interface).
# Sources:
#   Base mainnet  -> https://basescan.org/address/0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70
#   Arbitrum mainnet -> https://arbiscan.io/address/0x639Fe6ab55C921f74e7fac1ee960C0B6293ba612
CHAINLINK_ETH_USD_FEEDS: dict[str, str] = {
    "base": "0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70",
    "arbitrum": "0x639Fe6ab55C921f74e7fac1ee960C0B6293ba612",
}

# Minimal AggregatorV3Interface ABI: decimals + latestRoundData.
AGGREGATOR_V3_ABI: list[dict[str, Any]] = [
    {
        "inputs": [],
        "name": "decimals",
        "outputs": [{"internalType": "uint8", "name": "", "type": "uint8"}],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [],
        "name": "latestRoundData",
        "outputs": [
            {"internalType": "uint80", "name": "roundId", "type": "uint80"},
            {"internalType": "int256", "name": "answer", "type": "int256"},
            {"internalType": "uint256", "name": "startedAt", "type": "uint256"},
            {"internalType": "uint256", "name": "updatedAt", "type": "uint256"},
            {"internalType": "uint80", "name": "answeredInRound", "type": "uint80"},
        ],
        "stateMutability": "view",
        "type": "function",
    },
]

# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------
@dataclass
class LiquidationEvent:
    tx_hash: str
    block_number: int
    log_index: int
    chain: str
    liquidated_user: str
    collateral_asset: str
    debt_asset: str
    liquidated_amount: str  # wei string
    debt_to_cover: str      # wei string
    liquidator: str
    profit_eth: str         # estimated profit in wei
    gas_used: int | None
    timestamp: int | None

    def to_golden_replay(self) -> dict[str, Any]:
        """Convert to the golden_replays.json fixture format."""
        return {
            "description": f"Liquidation of {self.liquidated_user[:10]}... at block {self.block_number}",
            "chain": self.chain,
            "block_number": self.block_number,
            "tx_hash": self.tx_hash,
            "user": self.liquidated_user,
            "collateral_asset": self.collateral_asset,
            "debt_asset": self.debt_asset,
            "debt_to_cover": self.debt_to_cover,
            "expected_profit_wei": self.profit_eth,
            "gas_used": self.gas_used or 450000,
            "_verified": False,
            "_source": "chain_historical",
        }


# ---------------------------------------------------------------------------
# Web3 helpers
# ---------------------------------------------------------------------------
def get_w3(chain: str, rpc_url: str | None = None) -> Web3:
    """Initialize a Web3 provider for the specified chain."""
    if Web3 is None:
        raise RuntimeError(
            "web3 is not installed; cannot connect to an RPC. Install web3.py to "
            "fetch historical liquidations."
        )
    url = rpc_url or RPC_URLS.get(chain)
    if not url:
        raise ValueError(f"No RPC URL configured for chain: {chain}")

    w3 = Web3(Web3.HTTPProvider(url, request_kwargs={"timeout": 60}))
    if chain == "base":
        w3.middleware_onion.inject(geth_poa_middleware, layer=0)

    if not w3.is_connected():
        raise ConnectionError(f"Failed to connect to {chain} RPC at {url}")

    logger.info("Connected to %s (chain_id=%s, latest_block=%s)", chain, w3.eth.chain_id, w3.eth.block_number)
    return w3


def _retry_rpc(func: callable, max_retries: int = 5) -> Any:
    """Simple retry with exponential backoff."""
    import random

    last_exc: Exception | None = None
    for attempt in range(1, max_retries + 1):
        try:
            return func()
        except Exception as exc:
            last_exc = exc
            if attempt == max_retries:
                break
            delay = min(2 ** (attempt - 1), 30)
            time.sleep(delay + random.uniform(0, delay * 0.3))
            logger.warning("RPC retry %d/%d after error: %s", attempt, max_retries, exc)
    raise ConnectionError(f"RPC failed after {max_retries} attempts: {last_exc}")


# ---------------------------------------------------------------------------
# Core logic
# ---------------------------------------------------------------------------
def get_eth_price_usd(w3: Web3, chain: str, fallback: Decimal | None = None) -> Decimal:
    """Fetch the latest ETH/USD price from Chainlink, with a configurable fallback.

    The price is returned in USD per ETH (e.g. 2500.0). If the RPC call fails or the
    aggregator is not configured for the chain, the fallback price is used and a
    warning is logged.
    """
    feed_address = CHAINLINK_ETH_USD_FEEDS.get(chain)
    if not feed_address:
        logger.warning("No Chainlink ETH/USD feed configured for %s; using fallback", chain)
        return fallback or ETH_PRICE_USD_FALLBACK

    try:
        aggregator = w3.eth.contract(
            address=Web3.to_checksum_address(feed_address),
            abi=AGGREGATOR_V3_ABI,
        )
        decimals = _retry_rpc(lambda: int(aggregator.functions.decimals().call()))
        _, answer, _, updated_at, _ = _retry_rpc(
            lambda: aggregator.functions.latestRoundData().call()
        )
        if not updated_at:
            logger.warning("Chainlink ETH/USD feed for %s returned zero timestamp", chain)
            return fallback or ETH_PRICE_USD_FALLBACK

        price = Decimal(answer) / (Decimal(10) ** decimals)
        logger.info("Chainlink ETH/USD price for %s: %s", chain, price)
        return price
    except Exception as exc:
        logger.warning("Failed to fetch Chainlink ETH/USD for %s: %s. Using fallback.", chain, exc)
        return fallback or ETH_PRICE_USD_FALLBACK


def fetch_liquidation_events(
    w3: Web3,
    pool_address: str,
    from_block: int,
    to_block: int,
    chain: str,
    eth_price_usd: Decimal | None = None,
) -> list[LiquidationEvent]:
    """Fetch LiquidationCall events from the Aave V3 Pool within a block range."""
    logger.info(
        "Querying LiquidationCall events from block %d to %d for pool %s",
        from_block,
        to_block,
        pool_address,
    )

    pool = w3.eth.contract(
        address=Web3.to_checksum_address(pool_address),
        abi=POOL_ABI,
    )

    # RPC providers often cap event log ranges; chunk the request
    CHUNK_SIZE = 2000
    all_events: list = []
    current = from_block
    while current <= to_block:
        chunk_end = min(current + CHUNK_SIZE, to_block)
        try:
            event_filter = pool.events.LiquidationCall().create_filter(
                fromBlock=current,
                toBlock=chunk_end,
            )
            entries = _retry_rpc(event_filter.get_all_entries)
            all_events.extend(entries)
            logger.debug("Fetched %d events in chunk %d-%d", len(entries), current, chunk_end)
        except Exception as exc:
            logger.warning("Failed to fetch chunk %d-%d: %s", current, chunk_end, exc)
        current = chunk_end + 1

    logger.info("Total LiquidationCall events found: %d", len(all_events))

    liquidations: list[LiquidationEvent] = []
    for entry in all_events:
        args = entry["args"]
        tx_hash = entry["transactionHash"].hex()
        block_number = entry["blockNumber"]
        log_index = entry["logIndex"]

        # Estimate profit: simplistic (liquidatedCollateral - debtToCover value)
        # Real profit requires oracle prices at block time.
        debt_to_cover = int(args["debtToCover"])
        liquidated_collateral = int(args["liquidatedCollateralAmount"])
        # Rough profit estimate denominated in ETH-equivalent units.
        # Without collateral/debt USD prices at block time this is a heuristic only;
        # the Chainlink ETH/USD price is used as a sanity-check denominator.
        eth_price = eth_price_usd or ETH_PRICE_USD_FALLBACK
        profit_eth = str(
            int(
                Decimal(liquidated_collateral)
                * Decimal("0.05")
                / eth_price
            )
        )

        # Fetch receipt for gas_used
        gas_used: int | None = None
        timestamp: int | None = None
        try:
            receipt = _retry_rpc(lambda: w3.eth.get_transaction_receipt(entry["transactionHash"]))
            gas_used = receipt.gasUsed if receipt else None
        except Exception as exc:
            logger.debug("Could not fetch receipt for %s: %s", tx_hash, exc)

        try:
            block = _retry_rpc(lambda: w3.eth.get_block(block_number))
            timestamp = block.timestamp if block else None
        except Exception as exc:
            logger.debug("Could not fetch block %d: %s", block_number, exc)

        liquidations.append(
            LiquidationEvent(
                tx_hash=tx_hash,
                block_number=block_number,
                log_index=log_index,
                chain=chain,
                liquidated_user=args["user"],
                collateral_asset=args["collateralAsset"],
                debt_asset=args["debtAsset"],
                liquidated_amount=str(args["liquidatedCollateralAmount"]),
                debt_to_cover=str(args["debtToCover"]),
                liquidator=args["liquidator"],
                profit_eth=profit_eth,
                gas_used=gas_used,
                timestamp=timestamp,
            )
        )

    return liquidations


def write_fixture(
    events: list[LiquidationEvent],
    output_path: str,
    append: bool = False,
) -> None:
    """Write events to golden_replays-compatible JSON."""
    new_replays = [ev.to_golden_replay() for ev in events]

    path = Path(output_path)
    path.parent.mkdir(parents=True, exist_ok=True)

    if append and path.exists():
        with open(path, "r", encoding="utf-8") as f:
            existing = json.load(f)
        if isinstance(existing, list):
            existing.extend(new_replays)
        else:
            existing = new_replays
        replays = existing
    else:
        replays = new_replays

    with open(path, "w", encoding="utf-8") as f:
        json.dump(replays, f, indent=2)

    logger.info("Wrote %d liquidation replays to %s", len(new_replays), output_path)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Fetch historical Aave V3 liquidation events for golden replay fixtures."
    )
    parser.add_argument(
        "--chain",
        required=True,
        choices=["base", "arbitrum"],
        help="Target L2 chain.",
    )
    parser.add_argument(
        "--rpc-url",
        default=None,
        help="Override the default RPC URL.",
    )
    parser.add_argument(
        "--pool-address",
        default=None,
        help="Aave V3 Pool contract address.",
    )
    parser.add_argument(
        "--from-block",
        type=int,
        required=True,
        help="Start block (inclusive).",
    )
    parser.add_argument(
        "--to-block",
        type=int,
        required=True,
        help="End block (inclusive).",
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Output JSON path (tests/fixtures/golden_replays.json).",
    )
    parser.add_argument(
        "--append",
        action="store_true",
        help="Append to existing fixture instead of overwriting.",
    )
    parser.add_argument(
        "--eth-price-usd",
        type=Decimal,
        default=None,
        help="Override Chainlink ETH/USD price used for profit estimation.",
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

    if Web3 is None:
        logger.error("web3 is not installed; cannot fetch liquidations. Install web3.py.")
        return 2

    if args.from_block > args.to_block:
        logger.error("--from-block must be <= --to-block")
        return 1

    w3 = get_w3(args.chain, args.rpc_url)
    pool_address = args.pool_address or DEFAULT_POOLS.get(args.chain)
    if not pool_address:
        logger.error("No default pool address for %s; pass --pool-address", args.chain)
        return 1

    eth_price_usd = args.eth_price_usd or get_eth_price_usd(w3, args.chain)

    events = fetch_liquidation_events(
        w3,
        pool_address,
        args.from_block,
        args.to_block,
        args.chain,
        eth_price_usd=eth_price_usd,
    )

    if not events:
        logger.warning("No liquidation events found in the specified block range.")

    write_fixture(events, args.output, append=args.append)
    return 0


if __name__ == "__main__":
    sys.exit(main())
