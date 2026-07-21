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
import re
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
# Secret redaction
# ---------------------------------------------------------------------------
# RPC provider URLs embed the API key in the path (…/v2/<key>) or a query
# param (?apikey=<key>). web3 surfaces the full URL in connection/HTTP error
# strings, so an unredacted log line leaks the key. Redact before it reaches
# any handler. Patterns cover Alchemy/Infura path-keys and generic key params.
_REDACT_PATH_KEY = re.compile(r"(https?://[^\s\"']*?/(?:v2|v3)/)[A-Za-z0-9_\-]+")
_REDACT_QS_KEY = re.compile(r"([?&](?:api[_-]?key|key|token|apiKey)=)[A-Za-z0-9_\-]+", re.I)


def _redact(text: str) -> str:
    """Strip RPC API keys from a string (URLs in error messages)."""
    text = _REDACT_PATH_KEY.sub(r"\1<redacted>", text)
    return _REDACT_QS_KEY.sub(r"\1<redacted>", text)


class _RedactFilter(logging.Filter):
    """Redact RPC API keys from every log record before it is emitted."""

    def filter(self, record: logging.LogRecord) -> bool:
        try:
            msg = record.getMessage()
        except Exception:  # noqa: BLE001 - never let logging raise
            return True
        red = _redact(msg)
        if red != msg:
            record.msg = red
            record.args = ()
        return True


logger.addFilter(_RedactFilter())
for _h in logging.getLogger().handlers:  # cover the root handler basicConfig added
    _h.addFilter(_RedactFilter())

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
        # Aave V3 Borrow event — full signature. The debt-bearer is `onBehalfOf`
        # (indexed), NOT `user` (the caller). The prior 2-arg stub computed the
        # wrong topic0 and could not decode the borrower, so no scan could work.
        # Canonical: Borrow(address,address,address,uint256,uint8,uint256,uint16)
        "anonymous": False,
        "inputs": [
            {"indexed": True, "internalType": "address", "name": "reserve", "type": "address"},
            {"indexed": False, "internalType": "address", "name": "user", "type": "address"},
            {"indexed": True, "internalType": "address", "name": "onBehalfOf", "type": "address"},
            {"indexed": False, "internalType": "uint256", "name": "amount", "type": "uint256"},
            {"indexed": False, "internalType": "uint8", "name": "interestRateMode", "type": "uint8"},
            {"indexed": False, "internalType": "uint256", "name": "borrowRate", "type": "uint256"},
            {"indexed": True, "internalType": "uint16", "name": "referralCode", "type": "uint16"},
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
    {
        # Aggregated user account state. healthFactor is 1e18-scaled; it equals
        # type(uint256).max when the user has no debt. Used to triage discovered
        # borrowers down to the at-risk set before the per-asset balance reads.
        "inputs": [{"internalType": "address", "name": "user", "type": "address"}],
        "name": "getUserAccountData",
        "outputs": [
            {"internalType": "uint256", "name": "totalCollateralBase", "type": "uint256"},
            {"internalType": "uint256", "name": "totalDebtBase", "type": "uint256"},
            {"internalType": "uint256", "name": "availableBorrowsBase", "type": "uint256"},
            {"internalType": "uint256", "name": "currentLiquidationThreshold", "type": "uint256"},
            {"internalType": "uint256", "name": "ltv", "type": "uint256"},
            {"internalType": "uint256", "name": "healthFactor", "type": "uint256"},
        ],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [{"internalType": "address", "name": "user", "type": "address"}],
        "name": "getUserEMode",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function",
    },
]

# Aave V3 Oracle (IAaveOracle) — returns asset prices in the protocol base
# currency (USD, BASE_CURRENCY_UNIT = 1e8 on Base/Arbitrum). Used to populate
# reserve.price_usd, which the Rust detector consumes for health-factor math.
IAAVE_ORACLE_ABI: list[dict[str, Any]] = [
    {
        "inputs": [{"internalType": "address[]", "name": "assets", "type": "address[]"}],
        "name": "getAssetsPrices",
        "outputs": [{"internalType": "uint256[]", "name": "", "type": "uint256[]"}],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [{"internalType": "address", "name": "asset", "type": "address"}],
        "name": "getAssetPrice",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function",
    },
    {
        "inputs": [],
        "name": "BASE_CURRENCY_UNIT",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function",
    },
]

# Known Aave V3 Oracle addresses. Prefer config/pools.toml.
DEFAULT_ORACLES: dict[str, str] = {
    "base": "0x2Cc0Fc26eD4563A5ce5e8bdcfe1A2878676Ae156",
    "arbitrum": "0xb56c2F0B653B2e0b10C9b928C8580Ac5Df02C7C7",
}

# Public RPC endpoints used for the historical Borrow-event scan (getLogs) when
# the primary key can't serve it. Verified 2026-07-21: Alchemy's *free* tier
# caps eth_getLogs at a 10-block range (useless for discovery), while the
# official public endpoints below allow up to a 10,000-block range. State reads
# (eth_call) stay on the primary/private RPC; only getLogs uses these.
PUBLIC_LOGS_RPC: dict[str, str] = {
    "base": "https://mainnet.base.org",
    "arbitrum": "https://arb1.arbitrum.io/rpc",
}

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


def fetch_oracle_prices(
    w3: Web3,
    oracle_address: str,
    reserve_addresses: list[str],
) -> dict[str, float]:
    """Fetch USD prices for every reserve from the Aave V3 oracle.

    Returns {asset_address -> price_usd_float}. The oracle reports prices in the
    protocol base currency (USD) with BASE_CURRENCY_UNIT precision (1e8 on
    Base/Arbitrum); we normalise to a float USD value that the Rust detector
    converts back to 8-decimal fixed point. Falls back to per-asset calls if the
    batch call reverts, and leaves an asset unpriced (absent) on failure.
    """
    if not reserve_addresses:
        return {}
    logger.info("Fetching oracle prices for %d reserves from %s", len(reserve_addresses), oracle_address)
    oracle = w3.eth.contract(
        address=Web3.to_checksum_address(oracle_address),
        abi=IAAVE_ORACLE_ABI,
    )

    # BASE_CURRENCY_UNIT is the denominator (1e8). Default to 1e8 if the call
    # is unavailable so a transient failure does not zero every price.
    try:
        base_unit = int(_retry_with_backoff(lambda: oracle.functions.BASE_CURRENCY_UNIT().call()))
        if base_unit <= 0:
            base_unit = 10**8
    except Exception as exc:  # noqa: BLE001
        logger.warning("BASE_CURRENCY_UNIT call failed (%s); assuming 1e8", exc)
        base_unit = 10**8

    checksummed = [Web3.to_checksum_address(a) for a in reserve_addresses]
    prices: dict[str, float] = {}

    # Preferred: one batched getAssetsPrices call.
    try:
        raw = _retry_with_backoff(lambda: oracle.functions.getAssetsPrices(checksummed).call())
        for asset, raw_price in zip(reserve_addresses, raw):
            if int(raw_price) > 0:
                prices[asset] = int(raw_price) / base_unit
    except Exception as exc:  # noqa: BLE001 - fall back to per-asset below
        logger.warning("Batched getAssetsPrices failed (%s); falling back to per-asset", exc)

    # Fill any gaps (batch failed, or a specific asset returned 0) individually.
    for asset, checksum in zip(reserve_addresses, checksummed):
        if prices.get(asset, 0.0) > 0.0:
            continue
        try:
            raw_price = int(_retry_with_backoff(lambda c=checksum: oracle.functions.getAssetPrice(c).call()))
            if raw_price > 0:
                prices[asset] = raw_price / base_unit
        except Exception as exc:  # noqa: BLE001
            logger.warning("getAssetPrice failed for %s (%s); leaving unpriced", asset, exc)

    logger.info("Resolved %d/%d oracle prices", len(prices), len(reserve_addresses))
    return prices


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
def _scan_borrowers(
    pool: Any,
    latest: int,
    scan_blocks: int,
    log_chunk: int,
) -> set[str]:
    """Collect unique borrower addresses from recent Borrow events.

    Uses chunked ``eth_getLogs`` (via ``event.get_logs``) rather than a stateful
    ``eth_newFilter``: providers such as Alchemy reject the latter or cap the
    former's block range, so a single wide query 400s. Each chunk is retried and
    a failed chunk is skipped (logged) rather than aborting the whole scan.
    """
    # Guard the chunk step: a non-positive log_chunk would never advance `start`
    # (min(start + log_chunk - 1, latest) <= start - 1 < start), so the loop
    # would spin forever. Clamp to a single-block step.
    if log_chunk < 1:
        logger.warning("log_chunk %d < 1; clamping to 1 to keep the scan progressing", log_chunk)
        log_chunk = 1

    from_block = max(latest - scan_blocks + 1, 0)
    borrowers: set[str] = set()
    chunks = 0
    failed = 0
    start = from_block
    while start <= latest:
        end = min(start + log_chunk - 1, latest)
        chunks += 1
        try:
            entries = _retry_with_backoff(
                lambda s=start, e=end: pool.events.Borrow().get_logs(from_block=s, to_block=e)
            )
            for entry in entries:
                # The debt-bearer is onBehalfOf (the account whose debt increased),
                # not user (the caller). Collect it as the candidate borrower.
                # Skip a malformed log with neither field so `None` never enters
                # the set (it would break the later sorted(borrowers)).
                args = entry["args"]
                borrower = args.get("onBehalfOf") or args.get("user")
                if borrower:
                    borrowers.add(borrower)
        except Exception as exc:  # noqa: BLE001 - skip the chunk, keep scanning
            failed += 1
            logger.warning("Borrow log chunk %d-%d failed after retries: %s", start, end, exc)
        start = end + 1

    logger.info(
        "Scanned blocks %d-%d in %d chunk(s) (%d failed); %d unique borrowers",
        from_block, latest, chunks, failed, len(borrowers),
    )
    if failed:
        logger.warning(
            "%d/%d log chunk(s) failed — borrower set is INCOMPLETE for this window",
            failed, chunks,
        )
    return borrowers


def fetch_user_positions(
    w3: Web3,
    pool_address: str,
    data_provider_address: str,
    reserve_addresses: list[str],
    reserve_index: dict[str, dict[str, int]] | None = None,
    logs_w3: Web3 | None = None,
    scan_blocks: int = 50000,
    log_chunk: int = 9000,
    hf_max: float = 1.10,
    max_candidates: int = 1500,
    max_users: int = 300,
) -> dict[str, UserPosition]:
    """Discover at-risk Aave V3 positions and serialize them as scaled balances.

    Pipeline:
      1. Chunked Borrow-event scan over the last ``scan_blocks`` blocks →
         candidate borrower set (capped at ``max_candidates``).
      2. ``getUserAccountData`` triage: keep only users with active debt whose
         live health factor is ≤ ``hf_max`` (near or below liquidation), capped
         at ``max_users``.
      3. Per-asset ``getUserReserveData`` → SCALED balances the detector expects
         (it re-applies the reserve index): variable debt uses the on-chain
         ``scaledVariableDebt``; aToken collateral is de-scaled from the current
         balance via the reserve's ``liquidity_index`` (same index the detector
         re-applies, so the round-trip is consistent).

    NOTE: an event scan only surfaces borrowers active in the window. Complete
    at-risk coverage requires an indexer (The Graph / Dune / provider subgraph)
    queried by health factor. The caps above are logged when they bind so the
    snapshot never silently under-reports.
    """
    logger.info("Discovering at-risk users (Borrow scan → HF triage → scaled balances)...")
    ray = 10**27
    reserve_index = reserve_index or {}

    data_provider = w3.eth.contract(
        address=Web3.to_checksum_address(data_provider_address),
        abi=POOL_DATA_PROVIDER_ABI,
    )
    pool = w3.eth.contract(
        address=Web3.to_checksum_address(pool_address),
        abi=POOL_ABI,
    )

    # State reads (eth_call) use the primary RPC; the Borrow scan (getLogs) uses
    # logs_w3 when provided (the primary key may cap getLogs — e.g. Alchemy free
    # tier's 10-block limit). Its block height drives the scan range.
    scan_w3 = logs_w3 or w3
    logs_pool = scan_w3.eth.contract(
        address=Web3.to_checksum_address(pool_address),
        abi=POOL_ABI,
    )
    latest = scan_w3.eth.block_number
    borrowers = _scan_borrowers(logs_pool, latest, scan_blocks, log_chunk)

    candidates = sorted(borrowers)
    if len(candidates) > max_candidates:
        logger.warning(
            "Borrower set (%d) exceeds max_candidates (%d); triaging first %d only",
            len(candidates), max_candidates, max_candidates,
        )
        candidates = candidates[:max_candidates]

    # Triage: getUserAccountData health factor (1e18-scaled; uint256 max = no debt).
    hf_max_wad = int(hf_max * 10**18)
    at_risk: list[str] = []
    for user in candidates:
        try:
            acct = _retry_with_backoff(lambda u=user: pool.functions.getUserAccountData(u).call())
        except Exception as exc:  # noqa: BLE001
            logger.debug("getUserAccountData failed for %s: %s", user, exc)
            continue
        total_debt_base = int(acct[1])
        health_factor = int(acct[5])
        if total_debt_base > 0 and health_factor <= hf_max_wad:
            at_risk.append(user)
            if len(at_risk) >= max_users:
                logger.warning(
                    "Reached max_users (%d); stopping triage early (more at-risk users may exist)",
                    max_users,
                )
                break

    logger.info("Triaged %d candidates → %d at-risk (HF ≤ %.3f)", len(candidates), len(at_risk), hf_max)

    positions: dict[str, UserPosition] = {}
    for user in at_risk:
        try:
            emode = 0
            try:
                emode = int(_retry_with_backoff(lambda u=user: pool.functions.getUserEMode(u).call()))
            except Exception as exc:  # noqa: BLE001 - emode is enrichment, not critical
                logger.debug("getUserEMode failed for %s: %s", user, exc)

            collateral: dict[str, str] = {}
            debt: dict[str, str] = {}
            for asset in reserve_addresses:
                user_data = _retry_with_backoff(
                    lambda a=asset, u=user: data_provider.functions.getUserReserveData(a, u).call()
                )
                current_atoken = int(user_data[0])   # current (indexed) aToken balance
                scaled_variable_debt = int(user_data[4])  # already scaled on-chain

                if current_atoken > 0:
                    # De-scale to the balance the detector expects: it recomputes
                    # current = scaled * liquidity_index / RAY, so store the scaled form.
                    liq_index = reserve_index.get(asset, {}).get("liquidity_index", ray) or ray
                    scaled_collateral = current_atoken * ray // liq_index
                    if scaled_collateral > 0:
                        collateral[asset] = str(scaled_collateral)
                if scaled_variable_debt > 0:
                    debt[asset] = str(scaled_variable_debt)

            if debt:
                positions[user] = UserPosition(
                    collateral=collateral,
                    debt=debt,
                    emode_category=emode,
                )
        except Exception as exc:  # noqa: BLE001
            logger.debug("Failed to build position for %s: %s", user, exc)

    logger.info("Resolved %d at-risk positions with active debt", len(positions))
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
        "--oracle",
        default=None,
        help="Aave V3 Oracle address (defaults to pools.toml / known address for chain).",
    )
    parser.add_argument(
        "--logs-rpc",
        default=None,
        help="RPC for the Borrow-event getLogs scan only (state reads stay on the "
        "primary RPC). Defaults to the chain's public endpoint because Alchemy's "
        "free tier caps getLogs at 10 blocks. Pass 'primary' to force the main RPC.",
    )
    parser.add_argument(
        "--scan-blocks",
        type=int,
        default=50000,
        help="How many recent blocks to scan for Borrow events (default 50000).",
    )
    parser.add_argument(
        "--log-chunk",
        type=int,
        default=9000,
        help="eth_getLogs block-range chunk size (default 9000; public Base RPC caps at 10000).",
    )
    parser.add_argument(
        "--hf-max",
        type=float,
        default=1.10,
        help="Keep only users whose live health factor is ≤ this (default 1.10).",
    )
    parser.add_argument(
        "--max-candidates",
        type=int,
        default=1500,
        help="Cap on borrowers triaged via getUserAccountData (default 1500).",
    )
    parser.add_argument(
        "--max-users",
        type=int,
        default=300,
        help="Cap on at-risk positions written to the snapshot (default 300).",
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
        oracle_address = args.oracle or pool_cfg.get("oracle") or DEFAULT_ORACLES.get(args.chain)
        if not oracle_address:
            logger.error("No Aave oracle for %s; pass --oracle", args.chain)
            return 1

        reserves = fetch_reserves(w3, pool_address, data_provider)
        reserve_addrs = [r.address for r in reserves]

        # Enrich reserves with live oracle prices (detector HF math needs them).
        prices = fetch_oracle_prices(w3, oracle_address, reserve_addrs)
        for r in reserves:
            r.price_usd = prices.get(r.address, 0.0)

        # Reserve index map for de-scaling aToken collateral balances.
        reserve_index = {
            r.address: {
                "liquidity_index": r.liquidity_index,
                "variable_borrow_index": r.variable_borrow_index,
            }
            for r in reserves
        }

        # Separate RPC for the getLogs scan (see PUBLIC_LOGS_RPC). "primary"
        # forces the main RPC; an explicit URL overrides; default = public.
        logs_w3 = None
        if args.logs_rpc == "primary":
            logs_w3 = None
        else:
            logs_url = args.logs_rpc or PUBLIC_LOGS_RPC.get(args.chain)
            if logs_url:
                try:
                    logs_w3 = get_w3(args.chain, logs_url)
                    logger.info("Borrow scan will use logs RPC: %s", _redact(logs_url))
                except Exception as exc:  # noqa: BLE001 - fall back to primary
                    logger.warning("logs RPC unavailable (%s); using primary for getLogs", exc)
                    logs_w3 = None

        positions = fetch_user_positions(
            w3,
            pool_address,
            data_provider,
            reserve_addrs,
            reserve_index=reserve_index,
            logs_w3=logs_w3,
            scan_blocks=args.scan_blocks,
            log_chunk=args.log_chunk,
            hf_max=args.hf_max,
            max_candidates=args.max_candidates,
            max_users=args.max_users,
        )

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
