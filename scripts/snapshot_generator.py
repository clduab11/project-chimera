#!/usr/bin/env python3
"""
snapshot_generator.py
Project Chimera - Aave V3 Market Snapshot Generator

Fetches real Aave V3 state (reserves, oracles, user positions) from L2 RPCs
via web3.py and serializes it for the Rust REVM simulator's pre-warm cache.

Supports both live RPC calls and a --mock fallback for testing.

Usage:
  python scripts/snapshot_generator.py --chain base --output config/snapshot.json
  python scripts/snapshot_generator.py --chain base --mock --output config/snapshot.json
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import time
import tomllib
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

try:
    from web3 import Web3
    try:
        from web3.middleware import geth_poa_middleware
    except ImportError:  # web3.py v7 renamed the PoA middleware.
        from web3.middleware import ExtraDataToPOAMiddleware as geth_poa_middleware
except ImportError:  # Allows --mock mode on machines without web3.py installed.
    Web3 = None  # type: ignore[assignment]
    geth_poa_middleware = None  # type: ignore[assignment]

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("snapshot_generator")

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
RPC_URLS: dict[str, str] = {
    "base": os.getenv("BASE_RPC_URL", "https://mainnet.base.org"),
    "arbitrum": os.getenv("ARB_RPC_URL", "https://arb1.arbitrum.io/rpc"),
}

# Aave V3 PoolDataProvider ABI (minimal fragment)
POOL_DATA_PROVIDER_ABI: list[dict[str, Any]] = [
    {
        "inputs": [],
        "name": "getReservesList",
        "outputs": [{"internalType": "address[]", "name": "", "type": "address[]"}],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [{"internalType": "address", "name": "asset", "type": "address"}],
        "name": "getReserveData",
        "outputs": [
            {"internalType": "uint256", "name": "unbacked", "type": "uint256"},
            {"internalType": "uint256", "name": "accruedToTreasuryShares", "type": "uint256"},
            {"internalType": "uint256", "name": "totalAToken", "type": "uint256"},
            {"internalType": "uint256", "name": "totalStableDebt", "type": "uint256"},
            {"internalType": "uint256", "name": "totalVariableDebt", "type": "uint256"},
            {"internalType": "uint256", "name": "liquidityRate", "type": "uint256"},
            {"internalType": "uint256", "name": "variableBorrowRate", "type": "uint256"},
            {"internalType": "uint256", "name": "stableBorrowRate", "type": "uint256"},
            {"internalType": "uint256", "name": "averageStableBorrowRate", "type": "uint256"},
            {"internalType": "uint256", "name": "liquidityIndex", "type": "uint256"},
            {"internalType": "uint256", "name": "variableBorrowIndex", "type": "uint256"},
            {"internalType": "uint40", "name": "lastUpdateTimestamp", "type": "uint40"},
        ],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [
            {"internalType": "address", "name": "asset", "type": "address"}
        ],
        "name": "getReserveConfigurationData",
        "outputs": [
            {"internalType": "uint256", "name": "decimals", "type": "uint256"},
            {"internalType": "uint256", "name": "ltv", "type": "uint256"},
            {"internalType": "uint256", "name": "liquidationThreshold", "type": "uint256"},
            {"internalType": "uint256", "name": "liquidationBonus", "type": "uint256"},
            {"internalType": "uint256", "name": "reserveFactor", "type": "uint256"},
            {"internalType": "bool", "name": "usageAsCollateralEnabled", "type": "bool"},
            {"internalType": "bool", "name": "borrowingEnabled", "type": "bool"},
            {"internalType": "bool", "name": "stableBorrowRateEnabled", "type": "bool"},
            {"internalType": "bool", "name": "isActive", "type": "bool"},
            {"internalType": "bool", "name": "isFrozen", "type": "bool"},
        ],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [{"internalType": "address", "name": "asset", "type": "address"}],
        "name": "getReserveTokensAddresses",
        "outputs": [
            {"internalType": "address", "name": "aTokenAddress", "type": "address"},
            {"internalType": "address", "name": "stableDebtTokenAddress", "type": "address"},
            {"internalType": "address", "name": "variableDebtTokenAddress", "type": "address"},
        ],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [
            {"internalType": "address", "name": "asset", "type": "address"},
            {"internalType": "address", "name": "user", "type": "address"},
        ],
        "name": "getUserReserveData",
        "outputs": [
            {"internalType": "uint256", "name": "currentATokenBalance", "type": "uint256"},
            {"internalType": "uint256", "name": "currentStableDebt", "type": "uint256"},
            {"internalType": "uint256", "name": "currentVariableDebt", "type": "uint256"},
            {"internalType": "uint256", "name": "principalStableDebt", "type": "uint256"},
            {"internalType": "uint256", "name": "scaledVariableDebt", "type": "uint256"},
            {"internalType": "uint256", "name": "stableBorrowRate", "type": "uint256"},
            {"internalType": "uint256", "name": "liquidityRate", "type": "uint256"},
            {"internalType": "uint40", "name": "stableRateLastUpdated", "type": "uint40"},
            {"internalType": "bool", "name": "usageAsCollateralEnabled", "type": "bool"},
        ],
        "stateMutability": "view",
        "type": "function",
    },
]

# Aave V3 Pool ABI (minimal fragment for events + config decode)
POOL_ABI: list[dict[str, Any]] = [
    {
        "anonymous": False,
        "inputs": [
            {"indexed": True, "internalType": "address", "name": "reserve", "type": "address"},
            {"indexed": True, "internalType": "address", "name": "user", "type": "address"},
        ],
        "name": "Supply",
        "type": "event",
    },
    {
        "anonymous": False,
        "inputs": [
            {"indexed": True, "internalType": "address", "name": "reserve", "type": "address"},
            {"indexed": True, "internalType": "address", "name": "user", "type": "address"},
        ],
        "name": "Borrow",
        "type": "event",
    },
    {
        "inputs": [{"internalType": "address", "name": "asset", "type": "address"}],
        "name": "getConfiguration",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [{"internalType": "uint8", "name": "id", "type": "uint8"}],
        "name": "getEModeCategoryData",
        "outputs": [
            {
                "internalType": "tuple",
                "name": "",
                "type": "tuple",
                "components": [
                    {"internalType": "uint16", "name": "ltv", "type": "uint16"},
                    {"internalType": "uint16", "name": "liquidationThreshold", "type": "uint16"},
                    {"internalType": "uint16", "name": "liquidationBonus", "type": "uint16"},
                    {"internalType": "address", "name": "collateralBitmap", "type": "address"},
                    {"internalType": "string", "name": "label", "type": "string"},
                ],
            }
        ],
        "stateMutability": "view",
        "type": "function",
    },
]

# Known Aave V3 PoolDataProvider addresses. Prefer config/pools.toml.
DEFAULT_DATA_PROVIDERS: dict[str, str] = {
    "base": "0x0F43731EB8d45A581f4a36DD74F5f358bc90C73A",  # Aave V3 Base PoolDataProvider
    "arbitrum": "0x243Aa95cAC2a25651eda86e80bEe66114413c43b",  # Aave V3 Arbitrum PoolDataProvider
}

# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------
@dataclass
class Reserve:
    address: str
    symbol: str
    decimals: int
    ltv: int
    liquidation_threshold: int
    liquidation_bonus: int
    liquidity_rate: int
    variable_borrow_rate: int
    total_variable_debt: int
    price_usd: float = 0.0
    a_token: str = "0x0000000000000000000000000000000000000000"
    variable_debt_token: str = "0x0000000000000000000000000000000000000000"
    # --- Aave V3 edge-case fields. All optional with serde defaults on the Rust side,
    #     so older snapshots still parse. Keep field names in sync with
    #     docs/snapshot-schema.md, prewarm::ReserveData, and detector::RawReserve. ---
    active: bool = True
    frozen: bool = False
    paused: bool = False
    siloed_borrowing: bool = False
    liquidation_protocol_fee: int = 0  # bps (bits 152-167)
    emode_category: int = 0  # bits 168-175
    emode_liquidation_threshold: int = 0  # bps; carried for schema parity (not packed on-chain here)
    emode_liquidation_bonus: int = 0  # bps; carried for schema parity
    is_isolated: bool = False
    debt_ceiling: str = "0"  # decimal string (bits 212-251); string avoids JSON int precision loss
    # --- Pre-warm indices / rates / timestamps (offset 1 & 3 in Pool ReserveData storage).
    #     Default to RAY (1e27) neutral values; live snapshots populate from on-chain state.
    liquidity_index: int = 10**27   # RAY
    variable_borrow_index: int = 10**27  # RAY
    stable_borrow_rate: int = 0     # RAY
    last_update_timestamp: int = 0  # unix seconds
    id: int = 0                     # Aave V3 reserve id (sequential index in active reserves)


def load_pool_config(chain: str, path: str = "config/pools.toml") -> dict[str, str]:
    """Load pool/data-provider/oracle addresses from config/pools.toml."""
    config_path = Path(path)
    if not config_path.exists():
        return {}
    with open(config_path, "rb") as f:
        data = tomllib.load(f)
    return data.get(chain, {})


@dataclass
class UserPosition:
    collateral: dict[str, str]  # asset_address -> scaled balance string
    debt: dict[str, str]      # asset_address -> scaled debt string
    emode_category: int = 0
    is_in_isolation: bool = False  # optional (serde default false on the Rust side)


# ---------------------------------------------------------------------------
# Retry / rate-limit helpers
# ---------------------------------------------------------------------------
class RPCError(Exception):
    """Custom exception for RPC call failures."""


def _retry_with_backoff(
    func: callable,
    max_retries: int = 5,
    base_delay: float = 1.0,
    max_delay: float = 30.0,
) -> Any:
    """Retry a callable with exponential backoff and jitter."""
    import random

    last_exception: Exception | None = None
    for attempt in range(1, max_retries + 1):
        try:
            return func()
        except Exception as exc:
            last_exception = exc
            if attempt == max_retries:
                break
            delay = min(base_delay * (2 ** (attempt - 1)), max_delay)
            jitter = random.uniform(0, delay * 0.3)
            sleep_time = delay + jitter
            logger.warning(
                "RPC call failed (attempt %d/%d): %s. Retrying in %.2fs...",
                attempt,
                max_retries,
                exc,
                sleep_time,
            )
            time.sleep(sleep_time)
    raise RPCError(f"RPC call failed after {max_retries} attempts: {last_exception}")


# ---------------------------------------------------------------------------
# Web3 provider
# ---------------------------------------------------------------------------
def get_w3(chain: str, rpc_url: str | None = None) -> Web3:
    """Initialize a Web3 provider for the specified chain."""
    if Web3 is None:
        raise ImportError("web3.py is required for live RPC snapshots. Install requirements.txt or use --mock.")

    url = rpc_url or RPC_URLS.get(chain)
    if not url:
        raise ValueError(f"No RPC URL configured for chain: {chain}")

    w3 = Web3(Web3.HTTPProvider(url, request_kwargs={"timeout": 30}))
    if chain == "base" and geth_poa_middleware is not None:
        w3.middleware_onion.inject(geth_poa_middleware, layer=0)

    if not w3.is_connected():
        raise ConnectionError(f"Failed to connect to {chain} RPC at {url}")

    logger.info("Connected to %s (chain_id=%s, block=%s)", chain, w3.eth.chain_id, w3.eth.block_number)
    return w3


# ---------------------------------------------------------------------------
# Bitmap decoding helpers
# ---------------------------------------------------------------------------
def _decode_configuration_bitmap(configuration: int) -> dict[str, Any]:
    """Decode an Aave V3 ReserveConfigurationMap into a dict of field values.

    Bit layout (Aave V3 Pool v1):
      0-15   : LTV
      16-31  : liquidationThreshold
      32-47  : liquidationBonus
      48-55  : decimals (not in the bitmap on-chain; set by config call)
      56     : active
      57     : frozen
      60     : paused
      62     : siloedBorrowing
      152-167: liquidationProtocolFee (16 bits)
      168-175: eModeCategory (8 bits)
      212-251: debtCeiling (40 bits)
    """
    paused = bool((configuration >> 60) & 1)
    siloed_borrowing = bool((configuration >> 62) & 1)
    liquidation_protocol_fee = (configuration >> 152) & 0xFFFF
    emode_category = (configuration >> 168) & 0xFF
    debt_ceiling = (configuration >> 212) & 0xFFFFFFFFFF  # 40 bits
    # is_isolated is derived on-chain from debt_ceiling > 0
    is_isolated = debt_ceiling > 0
    return {
        "paused": paused,
        "siloed_borrowing": siloed_borrowing,
        "liquidation_protocol_fee": liquidation_protocol_fee,
        "emode_category": emode_category,
        "debt_ceiling": str(debt_ceiling),
        "is_isolated": is_isolated,
    }


def _fetch_emode_category_data(
    w3: Web3, pool_address: str, category_id: int
) -> tuple[int, int]:
    """Fetch eMode liquidation threshold and bonus for a category from the Pool."""
    if category_id == 0:
        return 0, 0
    # Aave V3.3 returns a dynamic struct (leading 0x20 offset), so declare a
    # single tuple output; flat field lists fail to decode (verified on Base).
    pool_contract = w3.eth.contract(
        address=Web3.to_checksum_address(pool_address),
        abi=[
            {
                "inputs": [{"internalType": "uint8", "name": "id", "type": "uint8"}],
                "name": "getEModeCategoryData",
                "outputs": [
                    {
                        "internalType": "tuple",
                        "name": "",
                        "type": "tuple",
                        "components": [
                            {"internalType": "uint16", "name": "ltv", "type": "uint16"},
                            {"internalType": "uint16", "name": "liquidationThreshold", "type": "uint16"},
                            {"internalType": "uint16", "name": "liquidationBonus", "type": "uint16"},
                            {"internalType": "address", "name": "collateralBitmap", "type": "address"},
                            {"internalType": "string", "name": "label", "type": "string"},
                        ],
                    }
                ],
                "stateMutability": "view",
                "type": "function",
            },
        ],
    )
    result = _retry_with_backoff(
        lambda: pool_contract.functions.getEModeCategoryData(category_id).call()
    )
    # web3 may return the single tuple output nested ((...),) or flattened
    # (ltv, lt, lb, bitmap, label); handle both shapes.
    category = result[0]
    if isinstance(category, int):
        return int(result[1]), int(result[2])
    return int(category[1]), int(category[2])


# ---------------------------------------------------------------------------
# Reserve fetching
# ---------------------------------------------------------------------------
def fetch_reserves(
    w3: Web3,
    pool_address: str,
    data_provider_address: str,
) -> list[Reserve]:
    """Fetch active reserve data from the Aave PoolDataProvider contract."""
    logger.info("Fetching reserves from PoolDataProvider %s", data_provider_address)

    data_provider = w3.eth.contract(
        address=Web3.to_checksum_address(data_provider_address),
        abi=POOL_DATA_PROVIDER_ABI,
    )

    # getReservesList lives on the Pool contract; the AaveProtocolDataProvider
    # reverts on it (verified on Base mainnet). Per-reserve reads below still
    # go to the data provider.
    pool_list = w3.eth.contract(
        address=Web3.to_checksum_address(pool_address),
        abi=[POOL_DATA_PROVIDER_ABI[0]],
    )

    reserve_list: list[str] = _retry_with_backoff(
        lambda: pool_list.functions.getReservesList().call()
    )
    logger.info("Found %d reserves", len(reserve_list))

    reserves: list[Reserve] = []
    for idx, asset in enumerate(reserve_list, start=1):
        try:
            raw_data = _retry_with_backoff(
                lambda a=asset: data_provider.functions.getReserveData(a).call()
            )
            config = _retry_with_backoff(
                lambda a=asset: data_provider.functions.getReserveConfigurationData(a).call()
            )
            token_addresses = _retry_with_backoff(
                lambda a=asset: data_provider.functions.getReserveTokensAddresses(a).call()
            )

            # Attempt to resolve symbol via ERC20
            symbol = _resolve_symbol(w3, asset)

            # getReserveConfigurationData layout:
            #   [0] decimals, [1] ltv, [2] liquidationThreshold, [3] liquidationBonus,
            #   [4] reserveFactor, [5] usageAsCollateralEnabled, [6] borrowingEnabled,
            #   [7] stableBorrowRateEnabled, [8] isActive, [9] isFrozen
            is_active = bool(config[8])
            is_frozen = bool(config[9])

            # Read the full ReserveConfigurationMap bitmap from the Pool contract
            # and decode remaining edge-case fields (paused, siloed, protocol fee,
            # eMode category, debt ceiling, isolation flag).
            pool_contract = w3.eth.contract(
                address=Web3.to_checksum_address(pool_address),
                abi=[
                    {
                        "inputs": [{"internalType": "address", "name": "asset", "type": "address"}],
                        "name": "getConfiguration",
                        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
                        "stateMutability": "view",
                        "type": "function",
                    },
                ],
            )
            configuration = _retry_with_backoff(
                lambda a=asset: pool_contract.functions.getConfiguration(a).call()
            )
            bitmap = _decode_configuration_bitmap(int(configuration))

            # Fetch eMode category threshold/bonus if the reserve belongs to a category.
            emode_lt, emode_bonus = 0, 0
            if bitmap["emode_category"] > 0:
                try:
                    emode_lt, emode_bonus = _fetch_emode_category_data(
                        w3, pool_address, bitmap["emode_category"]
                    )
                except Exception as exc:
                    logger.debug("Failed to fetch eMode data for category %d: %s", bitmap["emode_category"], exc)

            reserves.append(
                Reserve(
                    address=asset,
                    symbol=symbol,
                    decimals=int(config[0]),
                    ltv=int(config[1]),
                    liquidation_threshold=int(config[2]),
                    liquidation_bonus=int(config[3]),
                    liquidity_rate=int(raw_data[5]),
                    variable_borrow_rate=int(raw_data[6]),
                    total_variable_debt=int(raw_data[4]),
                    price_usd=0.0,  # Would require oracle call
                    a_token=token_addresses[0],
                    variable_debt_token=token_addresses[2],
                    active=is_active,
                    frozen=is_frozen,
                    paused=bitmap["paused"],
                    siloed_borrowing=bitmap["siloed_borrowing"],
                    liquidation_protocol_fee=bitmap["liquidation_protocol_fee"],
                    emode_category=bitmap["emode_category"],
                    emode_liquidation_threshold=emode_lt,
                    emode_liquidation_bonus=emode_bonus,
                    is_isolated=bitmap["is_isolated"],
                    debt_ceiling=bitmap["debt_ceiling"],
                    # Indices / rates / timestamps from getReserveData tuple:
                    #   [9]=liquidityIndex, [10]=variableBorrowIndex, [7]=stableBorrowRate, [11]=lastUpdateTimestamp
                    liquidity_index=int(raw_data[9]),
                    variable_borrow_index=int(raw_data[10]),
                    stable_borrow_rate=int(raw_data[7]),
                    last_update_timestamp=int(raw_data[11]),
                    # id (reserve index) requires Pool.getReserveData, not PoolDataProvider.getReserveData.
                    # Must call Pool contract directly; defaults to 0 for live snapshots.
                    id=0,
                )
            )
            logger.debug("[%d/%d] %s (%s)", idx, len(reserve_list), symbol, asset)
        except Exception as exc:
            logger.warning("Failed to fetch data for reserve %s: %s", asset, exc)

    return reserves


def _resolve_symbol(w3: Web3, token_address: str) -> str:
    """Best-effort ERC20 symbol resolution."""
    try:
        erc20 = w3.eth.contract(
            address=Web3.to_checksum_address(token_address),
            abi=[
                {
                    "constant": True,
                    "inputs": [],
                    "name": "symbol",
                    "outputs": [{"name": "", "type": "string"}],
                    "type": "function",
                }
            ],
        )
        return _retry_with_backoff(lambda: erc20.functions.symbol().call())
    except Exception:
        return "UNKNOWN"


# ---------------------------------------------------------------------------
# User position fetching
# ---------------------------------------------------------------------------
def fetch_user_positions(
    w3: Web3,
    pool_address: str,
    data_provider_address: str,
    reserve_addresses: list[str],
) -> dict[str, UserPosition]:
    """
    Scan for users with active debt positions.

    NOTE: A full scan requires an indexer (The Graph, Dune, or custom events).
    This implementation does a best-effort scan of recent Borrow events to find
    candidate users, then queries their balances. For production, replace with
    a dedicated indexer query.
    """
    logger.info("Scanning for at-risk users (event-based heuristic)...")

    data_provider = w3.eth.contract(
        address=Web3.to_checksum_address(data_provider_address),
        abi=POOL_DATA_PROVIDER_ABI,
    )

    # Heuristic: scan last N blocks for Borrow events to find active borrowers
    latest = w3.eth.block_number
    from_block = max(latest - 5000, 0)

    pool = w3.eth.contract(
        address=Web3.to_checksum_address(pool_address),
        abi=POOL_ABI,
    )

    borrowers: set[str] = set()
    try:
        event_filter = pool.events.Borrow().create_filter(from_block=from_block, to_block=latest)
        entries = _retry_with_backoff(event_filter.get_all_entries)
        for entry in entries:
            borrowers.add(entry["args"]["user"])
        logger.info("Found %d unique borrowers in blocks %d-%d", len(borrowers), from_block, latest)
    except Exception as exc:
        logger.warning("Failed to scan Borrow events: %s", exc)

    positions: dict[str, UserPosition] = {}
    for user in borrowers:
        try:
            collateral: dict[str, str] = {}
            debt: dict[str, str] = {}

            for asset in reserve_addresses:
                user_data = _retry_with_backoff(
                    lambda a=asset, u=user: data_provider.functions.getUserReserveData(a, u).call()
                )
                a_token_balance = int(user_data[0])
                variable_debt = int(user_data[2])

                if a_token_balance > 0:
                    collateral[asset] = str(a_token_balance)
                if variable_debt > 0:
                    debt[asset] = str(variable_debt)

            if debt:
                positions[user] = UserPosition(
                    collateral=collateral,
                    debt=debt,
                    emode_category=0,  # Would require protocol data provider call
                )
        except Exception as exc:
            logger.debug("Failed to fetch user data for %s: %s", user, exc)

    logger.info("Resolved %d positions with active debt", len(positions))
    return positions


# ---------------------------------------------------------------------------
# Serialization
# ---------------------------------------------------------------------------
# Fields that must be serialized as decimal strings to avoid JSON integer precision
# loss for u128 values (serde_json requires `arbitrary_precision` for large ints).
_U128_STRING_FIELDS = frozenset({
    "liquidity_rate",
    "variable_borrow_rate",
    "total_variable_debt",
    "liquidity_index",
    "variable_borrow_index",
    "stable_borrow_rate",
})


def _validate_live_snapshot(
    reserves: list[Reserve], positions: dict[str, UserPosition]
) -> list[str]:
    """Validate active reserves for a live (non-mock) snapshot.

    Returns a list of human-readable validation error messages. An empty list = valid.
    """
    errors: list[str] = []
    ray = 10**27
    for r in reserves:
        if not r.active:
            continue  # inactive reserves may legitimately carry defaults
        if r.a_token == "0x0000000000000000000000000000000000000000":
            errors.append(f"{r.symbol} ({r.address}): a_token is zero address")
        if r.variable_debt_token == "0x0000000000000000000000000000000000000000":
            errors.append(f"{r.symbol} ({r.address}): variable_debt_token is zero address")
        if r.price_usd <= 0.0:
            errors.append(f"{r.symbol} ({r.address}): price_usd is zero or negative")
        if r.liquidity_index < ray:
            errors.append(f"{r.symbol} ({r.address}): liquidity_index {r.liquidity_index} < RAY")
        if r.variable_borrow_index < ray:
            errors.append(f"{r.symbol} ({r.address}): variable_borrow_index {r.variable_borrow_index} < RAY")
        if r.liquidity_rate == 0 and r.variable_borrow_rate == 0:
            errors.append(f"{r.symbol} ({r.address}): both liquidity_rate and variable_borrow_rate are zero")
    if not positions:
        errors.append("no user positions found for live snapshot")
    return errors


def _reserve_to_dict(r: Reserve) -> dict[str, Any]:
    """Serialize a Reserve to a JSON-safe dict with u128 fields as strings."""
    d = asdict(r)
    for key in _U128_STRING_FIELDS:
        if key in d and isinstance(d[key], int):
            d[key] = str(d[key])
    return d


def serialize_snapshot(
    reserves: list[Reserve],
    positions: dict[str, UserPosition],
    chain: str,
    block_number: int,
    output_path: str,
    pool_address: str = "0x0000000000000000000000000000000000000000",
) -> None:
    """Save snapshot to JSON with timestamp."""
    snapshot = {
        "chain": chain,
        "block_number": block_number,
        "pool": pool_address,
        "timestamp": int(time.time()),
        "reserves": [_reserve_to_dict(r) for r in reserves],
        "users": {addr: asdict(pos) for addr, pos in positions.items()},
    }

    path = Path(output_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    # Atomic write (tmp sibling + os.replace) so a crash mid-write cannot leave
    # a truncated snapshot that the engine would silently load as empty.
    tmp = path.with_suffix(path.suffix + ".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(snapshot, f, indent=2)
    os.replace(tmp, path)

    logger.info("Snapshot written to %s (%d reserves, %d users)", output_path, len(reserves), len(positions))


# ---------------------------------------------------------------------------
# Mock fallback (original snapshot-generator test behaviour)
# ---------------------------------------------------------------------------
def _mock_reserves(chain: str) -> list[Reserve]:
    """Return June 2026 Aave V3 reserve data for testing."""
    logger.info("Using MOCK reserve data (June 2026 Aave V3 fixtures)")
    # Rates are expressed in Aave's RAY format (1e27 = 100%).
    ray = 10**27
    if chain == "base":
        return [
            Reserve(
                address="0x4200000000000000000000000000000000000006",
                symbol="WETH",
                decimals=18,
                ltv=8000,
                liquidation_threshold=8250,
                liquidation_bonus=10500,
                liquidity_rate=3 * 10**24,      # ~0.3% supply APY
                variable_borrow_rate=5 * 10**24, # ~0.5% borrow APY
                total_variable_debt=500_000 * 10**18,
                price_usd=2500.0,
                a_token="0xD4a0e0b9149BCee3C920d2E00b5dE09138fd8bb7",
                variable_debt_token="0x24e6e0795b3c7c71D965fCc4f371803d1c1DcA1E",
                active=True,
                frozen=False,
                paused=False,
                siloed_borrowing=False,
                liquidation_protocol_fee=1000,  # 10% of the bonus
                # Exercise eMode: WETH sits in eMode category 1 with a higher LT/bonus.
                emode_category=1,
                emode_liquidation_threshold=9300,
                emode_liquidation_bonus=10200,
                is_isolated=False,
                debt_ceiling="0",
                liquidity_index=1056789123456789123456789123,
                variable_borrow_index=1034567891234567891234567891,
                stable_borrow_rate=0,
                last_update_timestamp=int(time.time()),
            ),
            Reserve(
                address="0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                symbol="USDC",
                decimals=6,
                ltv=7700,
                liquidation_threshold=8000,
                liquidation_bonus=10500,
                liquidity_rate=6 * 10**24,
                variable_borrow_rate=9 * 10**24,
                total_variable_debt=2_000_000 * 10**6,
                price_usd=1.0,
                a_token="0x4e65fE4DbA92790696d040ac24Aa414708F5c0AB",
                variable_debt_token="0x59dca05b6c26dbd64b5381374aAaC5CD05644C28",
                active=True,
                frozen=False,
                paused=False,
                siloed_borrowing=False,
                liquidation_protocol_fee=1000,  # 10%
                emode_category=0,
                emode_liquidation_threshold=0,
                emode_liquidation_bonus=0,
                is_isolated=False,
                debt_ceiling="0",
                liquidity_index=1056789123456789123456789123,
                variable_borrow_index=1034567891234567891234567891,
                stable_borrow_rate=0,
                last_update_timestamp=int(time.time()),
            ),
        ]
    # Arbitrum defaults
    return [
        Reserve(
            address="0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            symbol="WETH",
            decimals=18,
            ltv=8000,
            liquidation_threshold=8250,
            liquidation_bonus=10500,
            liquidity_rate=3 * 10**24,
            variable_borrow_rate=5 * 10**24,
            total_variable_debt=700_000 * 10**18,
            price_usd=2500.0,
            a_token="0xe50fA9b3c56FfB159cB0FCA61F5c9D750e8128c8",
            variable_debt_token="0x0c84331e39d6658Cd6e6b9ba04736cC4c4734351",
            active=True,
            frozen=False,
            paused=False,
            siloed_borrowing=False,
            liquidation_protocol_fee=1000,  # 10% of the bonus
            # Exercise eMode: WETH sits in eMode category 1 with a higher LT/bonus.
            emode_category=1,
            emode_liquidation_threshold=9300,
            emode_liquidation_bonus=10200,
            is_isolated=False,
            debt_ceiling="0",
            liquidity_index=1087654321987654321987654321,
            variable_borrow_index=1045678912345678912345678912,
            stable_borrow_rate=0,
            last_update_timestamp=int(time.time()),
        ),
        Reserve(
            address="0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            symbol="USDC",
            decimals=6,
            ltv=7700,
            liquidation_threshold=8000,
            liquidation_bonus=10500,
            liquidity_rate=6 * 10**24,
            variable_borrow_rate=9 * 10**24,
            total_variable_debt=3_500_000 * 10**6,
            price_usd=1.0,
            a_token="0x724dc807b04555b71ed48a6896b6F41593b8C637",
            variable_debt_token="0xf611aEb5013fD2c0511c9CD55c7dc5C1140741A6",
            active=True,
            frozen=False,
            paused=False,
            siloed_borrowing=False,
            liquidation_protocol_fee=1000,  # 10%
            emode_category=0,
            emode_liquidation_threshold=0,
            emode_liquidation_bonus=0,
            is_isolated=False,
            debt_ceiling="0",
            liquidity_index=1087654321987654321987654321,
            variable_borrow_index=1045678912345678912345678912,
            stable_borrow_rate=0,
            last_update_timestamp=int(time.time()),
        ),
    ]


def _mock_user_positions(chain: str) -> dict[str, UserPosition]:
    """Return mock user positions with realistic scaled balances for testing."""
    logger.info("Using MOCK user position data")
    if chain == "base":
        return {
            "0x00000000000000000000000000000000000d3b7a": UserPosition(
                collateral={"0x4200000000000000000000000000000000000006": "2000000000000000000"},
                debt={"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913": "4500000000"},
                emode_category=0,
                is_in_isolation=False,
            )
        }
    return {
        "0x00000000000000000000000000000000000a4b1d": UserPosition(
            collateral={"0x82aF49447D8a07e3bd95BD0d56f35241523fBab1": "2000000000000000000"},
            debt={"0xaf88d065e77c8cC2239327C5EDb3A432268e5831": "4500000000"},
            emode_category=0,
            is_in_isolation=False,
        )
    }


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Generate Aave V3 market snapshots for the Chimera simulator."
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
        help="Override the default RPC URL for the chain.",
    )
    parser.add_argument(
        "--pool-address",
        default=None,
        help="Aave V3 Pool contract address (optional, used for event scanning).",
    )
    parser.add_argument(
        "--data-provider",
        default=None,
        help="Aave V3 PoolDataProvider address (defaults to known address for chain).",
    )
    parser.add_argument(
        "--pools-config",
        default="config/pools.toml",
        help="Path to pools.toml for Pool/DataProvider defaults.",
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Path to output the JSON snapshot.",
    )
    parser.add_argument(
        "--mock",
        action="store_true",
        help="Use mock data instead of live RPC calls (for testing).",
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

    if args.mock:
        logger.info("MOCK mode enabled - skipping RPC calls")
        reserves = _mock_reserves(args.chain)
        positions = _mock_user_positions(args.chain)
        block_number = 0
        pool_address = "0x0000000000000000000000000000000000000000"
    else:
        w3 = get_w3(args.chain, args.rpc_url)
        block_number = w3.eth.block_number
        pool_cfg = load_pool_config(args.chain, args.pools_config)

        data_provider = args.data_provider or pool_cfg.get("pool_data_provider") or DEFAULT_DATA_PROVIDERS.get(args.chain)
        if not data_provider:
            logger.error("No default data provider for %s; pass --data-provider", args.chain)
            return 1

        pool_address = args.pool_address or pool_cfg.get("pool") or "0x0000000000000000000000000000000000000000"

        reserves = fetch_reserves(w3, pool_address, data_provider)
        reserve_addrs = [r.address for r in reserves]
        positions = fetch_user_positions(w3, pool_address, data_provider, reserve_addrs)

        # Validate live snapshot before writing.
        val_errors = _validate_live_snapshot(reserves, positions)
        if val_errors:
            for err in val_errors:
                logger.warning("Live snapshot validation: %s", err)
            logger.info(
                "Snapshot has %d validation warning(s); writing anyway (--mock to bypass)",
                len(val_errors),
            )

    serialize_snapshot(reserves, positions, args.chain, block_number, args.output, pool_address)
    return 0


if __name__ == "__main__":
    sys.exit(main())
