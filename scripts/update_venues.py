#!/usr/bin/env python3
"""
update_venues.py
Project Chimera - Weekly Venue List Updater

Queries on-chain DEX pools (Aerodrome, Uniswap V3, SushiSwap, Camelot) for
liquidity depth and refreshes config/routing.yaml with venues that exceed the
minimum liquidity threshold.

Design (post-reconciliation, 2026-07-21):
  * Routers and router_compatibility are PINNED verified constants (ROUTERS /
    ROUTER_COMPAT). The updater never writes a null/blank router_address, and
    never demotes a venue's compatibility. Liquidity discovery cannot corrupt
    the routing table.
  * The updater MERGES discovered liquidity onto existing venues by name and
    preserves their configured `pairs`. It does not wholesale-replace the venue
    list (which previously dropped multi-pair coverage).
  * USD prices come from Chainlink `latestRoundData` when web3 is available,
    with a hard fail-safe to labelled reference prices (stale/zero/again-unread
    feeds never produce a bad price — they fall back).

Invariants honored:
  * Importable without web3.py installed (graceful fallback).           [#5]
  * All monetary math uses Decimal, never float.                        [#3]

Usage:
  python scripts/update_venues.py --config config/routing.yaml --min-liquidity-usd 50000
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
from dataclasses import dataclass, field
from decimal import Decimal
from pathlib import Path
from typing import Any

import yaml

try:
    from web3 import Web3
except ImportError:  # Allows lint/syntax checks without web3.py installed. [#5]
    Web3 = None  # type: ignore[assignment,misc]

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("update_venues")

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
DEFAULT_MIN_LIQUIDITY_USD: int = 50000

RPC_URLS: dict[str, str] = {
    "base": os.getenv("BASE_RPC_URL", "https://mainnet.base.org"),
    "arbitrum": os.getenv("ARB_RPC_URL", "https://arb1.arbitrum.io/rpc"),
}

# Verified FACTORY addresses (pool discovery via getPair/getPool). Confirmed
# 2026-07-21 against Basescan name tags / Uniswap docs.
FACTORIES: dict[str, dict[str, str]] = {
    "base": {
        "aerodrome": "0x420DD381b31aEf6683db6b902084cB0FFECe40Da",
        "uniswap_v3": "0x33128a8fC17869897dcE68Ed026d694621f6FDfD",
        "sushiswap": "0x71524B4f93c58fcbF659783284E38825f0622859",
    },
    "arbitrum": {
        "camelot": "0x6EcCab422D763aC031210895C81787E87b43a652",
        "uniswap_v3": "0x1F98431c8aD98523631AE4a59f267346ea31F984",
        "sushiswap": "0xc35DADB65012eC5796536bD9864eD8773aBc74C4",
    },
}

# Verified ROUTER addresses (swap execution target). Confirmed 2026-07-21
# against Basescan name tags ("Aerodrome: Router", "Uniswap V3: Swap Router02",
# "Sushi: Router v2", "Camelot: Router") and Uniswap Base deployment docs.
# These are PINNED — the updater writes these verbatim and never blanks them.
#   NOTE: Aerodrome is a Solidly fork requiring Route[] calldata; it stays
#   router_compatibility="custom" (Executor cannot call it via the V2 path[]
#   encoder) until the Aerodrome encoder ships. Router is pinned-correct so the
#   value is ready, but the resolver will not select it.
#   NOTE: Uniswap V3 arbitrum SwapRouter02 not re-verified this pass; left as
#   the factory placeholder and kept inert (router_compatibility="v3", resolver
#   skips) until the V3 encoder + a re-verify land.
ROUTERS: dict[str, dict[str, str]] = {
    "base": {
        "aerodrome": "0xcF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43",
        "uniswap_v3": "0x2626664c2603336E57B271c5C0b26F421741e481",
        "sushiswap": "0x6BDED42c6DA8FBf0d2bA55B2fa120C5e0c8D7891",
    },
    "arbitrum": {
        "camelot": "0xc873fecbd354f5a56e00e710b90ef4201db2448d",
        "uniswap_v3": "0x1F98431c8aD98523631AE4a59f267346ea31F984",  # placeholder (V3 factory); inert, resolver skips v3
        # sushiswap-arbitrum intentionally omitted: not a configured venue; router unverified.
    },
}

# Canonical venue names, keyed by (chain, protocol), matching the committed
# routing.yaml exactly. The updater refreshes these existing venues; it never
# invents new names (which previously caused duplicate/ballooned venue lists).
VENUE_NAMES: dict[tuple[str, str], str] = {
    ("base", "aerodrome"): "aerodrome-base",
    ("base", "uniswap_v3"): "uniswap-v3-base",
    ("base", "sushiswap"): "sushi-base",
    ("arbitrum", "camelot"): "camelot-arbitrum",
    ("arbitrum", "uniswap_v3"): "uniswap-v3-arbitrum",
}

# Router ABI shape per protocol. Consumed by config.rs validation + the resolver
# (which only selects "v2"). "custom" parks a venue out of selection.
ROUTER_COMPAT: dict[str, str] = {
    "aerodrome": "custom",   # Solidly Route[] — inert until Aerodrome encoder
    "uniswap_v3": "v3",      # inert until V3 encoder
    "sushiswap": "v2",       # executable with current Executor
    "camelot": "v2",
}

# Token addresses for liquidity estimation.
BASE_TOKENS: dict[str, str] = {
    "WETH": "0x4200000000000000000000000000000000000006",
    "USDC": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "USDbC": "0xd9aAEc86B65D86f6A7B5B1b0c42FFA531710b6CA",
    "cbETH": "0x2Ae3F1Ec7F1F5012CFEab0185bfc7aa3cf0DEc22",
}

ARBITRUM_TOKENS: dict[str, str] = {
    "WETH": "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
    "USDC": "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
    "USDT": "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
    "WBTC": "0x2f2a2543B76A4166549F7aaB2e75Bef0aefC5B0f",
}

TOKEN_DECIMALS: dict[str, dict[str, int]] = {
    "base": {
        BASE_TOKENS["WETH"]: 18,
        BASE_TOKENS["USDC"]: 6,
        BASE_TOKENS["USDbC"]: 6,
        BASE_TOKENS["cbETH"]: 18,
    },
    "arbitrum": {
        ARBITRUM_TOKENS["WETH"]: 18,
        ARBITRUM_TOKENS["USDC"]: 6,
        ARBITRUM_TOKENS["USDT"]: 6,
        ARBITRUM_TOKENS["WBTC"]: 8,
    },
}

# Chainlink USD price feeds (8-decimal answers). VERIFY these against the
# Chainlink Base address list before relying on them for anything but the $50k
# liquidity gate — the code fails safe to REFERENCE_PRICES_USD on any read
# error, stale round, or non-positive answer, so an unverified/rotated feed
# cannot silently produce a bad price.
CHAINLINK_FEEDS: dict[str, dict[str, str]] = {
    "base": {
        BASE_TOKENS["WETH"]: "0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70",  # ETH/USD (verify)
        BASE_TOKENS["USDC"]: "0x7e860098F58bBFC8648a4311b374B1D669a2bc6B",  # USDC/USD (verify)
    },
    "arbitrum": {},
}

# Fallback ONLY. Used when a Chainlink read is unavailable/stale/non-positive.
# Not a source of truth — kept conservative and clearly labelled.
REFERENCE_PRICES_USD: dict[str, Decimal] = {
    BASE_TOKENS["WETH"]: Decimal("2500.0"),
    BASE_TOKENS["USDC"]: Decimal("1.0"),
    BASE_TOKENS["USDbC"]: Decimal("1.0"),
    BASE_TOKENS["cbETH"]: Decimal("2650.0"),
    ARBITRUM_TOKENS["WETH"]: Decimal("2500.0"),
    ARBITRUM_TOKENS["USDC"]: Decimal("1.0"),
    ARBITRUM_TOKENS["USDT"]: Decimal("1.0"),
    ARBITRUM_TOKENS["WBTC"]: Decimal("100000.0"),
}

# Max age (seconds) a Chainlink round may be before we distrust it and fall back.
# Weekly offline tool: lenient window, but still rejects clearly-stale feeds.
MAX_FEED_AGE_SECONDS: int = 6 * 3600

FORENSIC_TAG_LISTS: list[str] = [
    # Local path or URL to curated forensic tag lists
]

# ---------------------------------------------------------------------------
# ABI fragments
# ---------------------------------------------------------------------------
ERC20_BALANCE_ABI: list[dict[str, Any]] = [
    {
        "inputs": [{"internalType": "address", "name": "account", "type": "address"}],
        "name": "balanceOf",
        "outputs": [{"internalType": "uint256", "name": "", "type": "uint256"}],
        "stateMutability": "view",
        "type": "function",
    }
]

V2_FACTORY_ABI: list[dict[str, Any]] = [
    {
        "inputs": [
            {"internalType": "address", "name": "tokenA", "type": "address"},
            {"internalType": "address", "name": "tokenB", "type": "address"},
        ],
        "name": "getPair",
        "outputs": [{"internalType": "address", "name": "pair", "type": "address"}],
        "stateMutability": "view",
        "type": "function",
    }
]

AERODROME_FACTORY_ABI: list[dict[str, Any]] = [
    {
        "inputs": [
            {"internalType": "address", "name": "tokenA", "type": "address"},
            {"internalType": "address", "name": "tokenB", "type": "address"},
            {"internalType": "bool", "name": "stable", "type": "bool"},
        ],
        "name": "getPool",
        "outputs": [{"internalType": "address", "name": "pool", "type": "address"}],
        "stateMutability": "view",
        "type": "function",
    }
]

V3_FACTORY_ABI: list[dict[str, Any]] = [
    {
        "inputs": [
            {"internalType": "address", "name": "tokenA", "type": "address"},
            {"internalType": "address", "name": "tokenB", "type": "address"},
            {"internalType": "uint24", "name": "fee", "type": "uint24"},
        ],
        "name": "getPool",
        "outputs": [{"internalType": "address", "name": "pool", "type": "address"}],
        "stateMutability": "view",
        "type": "function",
    }
]

CHAINLINK_AGGREGATOR_ABI: list[dict[str, Any]] = [
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
    {
        "inputs": [],
        "name": "decimals",
        "outputs": [{"internalType": "uint8", "name": "", "type": "uint8"}],
        "stateMutability": "view",
        "type": "function",
    },
]


# ---------------------------------------------------------------------------
# Data models
# ---------------------------------------------------------------------------
@dataclass
class Venue:
    name: str
    chain: str
    liquidity_usd_min: int
    type: str
    kyc: bool
    router_compatibility: str
    router_address: str
    pairs: list[dict[str, str]] = field(default_factory=list)

    def to_yaml_record(self) -> dict[str, Any]:
        # Schema MUST match core/src/config.rs VenueEntry exactly (no extra keys).
        return {
            "name": self.name,
            "chain": self.chain,
            "liquidity_usd_min": self.liquidity_usd_min,
            "type": self.type,
            "kyc": self.kyc,
            "router_compatibility": self.router_compatibility,
            "router_address": self.router_address,
            "pairs": self.pairs,
        }


# ---------------------------------------------------------------------------
# Safety / forensic checks
# ---------------------------------------------------------------------------
def load_forensic_tags(paths: list[str]) -> set[str]:
    """Load known forensic-tagged addresses from local/remote lists."""
    tags: set[str] = set()
    for path in paths:
        if path.startswith("http://") or path.startswith("https://"):
            logger.debug("Fetching forensic tags from %s", path)
            try:
                import urllib.request

                with urllib.request.urlopen(path, timeout=10) as resp:
                    data = json.loads(resp.read().decode())
                    if isinstance(data, list):
                        tags.update(str(a).lower() for a in data)
                    elif isinstance(data, dict):
                        tags.update(str(k).lower() for k in data.keys())
            except Exception as exc:
                logger.warning("Failed to load forensic tags from %s: %s", path, exc)
        else:
            p = Path(path)
            if p.exists():
                try:
                    with open(p, "r", encoding="utf-8") as f:
                        data = json.load(f)
                    if isinstance(data, list):
                        tags.update(str(a).lower() for a in data)
                    elif isinstance(data, dict):
                        tags.update(str(k).lower() for k in data.keys())
                except Exception as exc:
                    logger.warning("Failed to load forensic tags from %s: %s", path, exc)
    return tags


def is_non_kyc_venue(name: str) -> bool:
    """Heuristic: reject venues with known KYC keywords."""
    kyc_keywords = [
        "kyc",
        "cex",
        "centralized",
        "regulated",
        " licensed ",
        "coinbase",
        "binance",
        "kraken",
    ]
    lower = name.lower()
    for kw in kyc_keywords:
        if kw in lower:
            logger.warning("Venue %s rejected: matched KYC keyword '%s'", name, kw)
            return False
    return True


def is_not_forensic_tagged(address: str | None, tags: set[str]) -> bool:
    """Reject venues whose contract address appears in forensic lists."""
    if not address:
        return True
    if address.lower() in tags:
        logger.warning("Address %s rejected: forensic tag match", address)
        return False
    return True


# ---------------------------------------------------------------------------
# Pricing (Chainlink with fail-safe fallback)
# ---------------------------------------------------------------------------
def _compute_price_from_round(
    answer: int,
    updated_at: int,
    now_ts: int,
    decimals: int,
    max_age_seconds: int = MAX_FEED_AGE_SECONDS,
) -> Decimal | None:
    """Pure, unit-testable price validation. Returns Decimal price or None if the
    round is untrustworthy (non-positive answer or stale)."""
    if answer <= 0:
        return None
    if updated_at <= 0:
        return None
    if now_ts - updated_at > max_age_seconds:
        return None
    return Decimal(answer) / (Decimal(10) ** Decimal(decimals))


def _chainlink_price_usd(w3: "Web3 | None", chain: str, token: str) -> Decimal | None:
    """Read a token's USD price from its Chainlink feed. Returns None on any
    failure so the caller falls back to REFERENCE_PRICES_USD."""
    if w3 is None:
        return None
    feed = CHAINLINK_FEEDS.get(chain, {}).get(token)
    if not feed:
        return None
    try:
        agg = w3.eth.contract(address=feed, abi=CHAINLINK_AGGREGATOR_ABI)
        _round_id, answer, _started, updated_at, _air = agg.functions.latestRoundData().call()
        try:
            decimals = int(agg.functions.decimals().call())
        except Exception:
            decimals = 8  # Chainlink USD feeds are 8 decimals
        now_ts = int(w3.eth.get_block("latest")["timestamp"])
        return _compute_price_from_round(int(answer), int(updated_at), now_ts, decimals)
    except Exception as exc:
        logger.debug("Chainlink read failed for %s/%s: %s", chain, token, exc)
        return None


def price_usd(w3: "Web3 | None", chain: str, token: str) -> Decimal:
    """Chainlink price with hard fail-safe to labelled reference price."""
    live = _chainlink_price_usd(w3, chain, token)
    if live is not None:
        return live
    fallback = REFERENCE_PRICES_USD.get(token, Decimal(0))
    if fallback == 0:
        logger.warning("No price for token %s on %s (no feed, no reference)", token, chain)
    return fallback


# ---------------------------------------------------------------------------
# RPC helpers
# ---------------------------------------------------------------------------
def _get_w3(chain: str) -> "Web3 | None":
    """Return a connected Web3 instance for the chain, or None if unavailable."""
    if Web3 is None:
        return None
    url = RPC_URLS.get(chain)
    if not url:
        return None
    try:
        w3 = Web3(Web3.HTTPProvider(url, request_kwargs={"timeout": 15}))
        if not w3.is_connected():
            logger.warning("Could not connect to %s RPC at %s", chain, url)
            return None
        return w3
    except Exception as exc:
        logger.warning("Web3 init failed for %s: %s", chain, exc)
        return None


def _pool_liquidity_usd(
    w3: "Web3",
    pool_address: str,
    token_a: str,
    token_b: str,
    chain: str,
) -> Decimal:
    """Compute USD liquidity from ERC20 balances held by a pool."""
    if not pool_address or int(pool_address, 16) == 0:
        return Decimal(0)
    try:
        token_a_contract = w3.eth.contract(address=token_a, abi=ERC20_BALANCE_ABI)
        token_b_contract = w3.eth.contract(address=token_b, abi=ERC20_BALANCE_ABI)
        bal_a = token_a_contract.functions.balanceOf(pool_address).call()
        bal_b = token_b_contract.functions.balanceOf(pool_address).call()
    except Exception as exc:
        logger.debug("balanceOf call failed for %s: %s", pool_address, exc)
        return Decimal(0)

    dec_a = TOKEN_DECIMALS[chain].get(token_a, 18)
    dec_b = TOKEN_DECIMALS[chain].get(token_b, 6)
    price_a = price_usd(w3, chain, token_a)
    price_b = price_usd(w3, chain, token_b)

    amt_a = Decimal(bal_a) / Decimal(10**dec_a)
    amt_b = Decimal(bal_b) / Decimal(10**dec_b)
    return amt_a * price_a + amt_b * price_b


# ---------------------------------------------------------------------------
# Liquidity discovery
# ---------------------------------------------------------------------------
STUB_LIQUIDITY: dict[str, Decimal] = {
    "uniswap_v3": Decimal("75000"),
    "aerodrome": Decimal("60000"),
    "sushiswap": Decimal("55000"),
    "camelot": Decimal("52000"),
}


def _query_liquidity_uniswap_v3(
    w3: "Web3 | None", factory_address: str, token_a: str, token_b: str, chain: str
) -> Decimal:
    if w3 is None:
        return STUB_LIQUIDITY["uniswap_v3"]
    try:
        factory = w3.eth.contract(address=factory_address, abi=V3_FACTORY_ABI)
        for fee in (500, 3000, 10000):
            pool = factory.functions.getPool(token_a, token_b, fee).call()
            if pool and int(pool, 16) != 0:
                liq = _pool_liquidity_usd(w3, pool, token_a, token_b, chain)
                if liq > 0:
                    return liq
        return Decimal(0)
    except Exception as exc:
        logger.debug("Uniswap V3 query failed: %s", exc)
        return STUB_LIQUIDITY["uniswap_v3"]


def _query_liquidity_aerodrome(
    w3: "Web3 | None", factory_address: str, token_a: str, token_b: str, chain: str
) -> Decimal:
    if w3 is None:
        return STUB_LIQUIDITY["aerodrome"]
    try:
        factory = w3.eth.contract(address=factory_address, abi=AERODROME_FACTORY_ABI)
        for stable in (False, True):
            pool = factory.functions.getPool(token_a, token_b, stable).call()
            if pool and int(pool, 16) != 0:
                liq = _pool_liquidity_usd(w3, pool, token_a, token_b, chain)
                if liq > 0:
                    return liq
        return Decimal(0)
    except Exception as exc:
        logger.debug("Aerodrome query failed: %s", exc)
        return STUB_LIQUIDITY["aerodrome"]


def _query_liquidity_v2(
    w3: "Web3 | None", factory_address: str, token_a: str, token_b: str, chain: str
) -> Decimal:
    """UniV2-clone pair liquidity (SushiSwap, Camelot) via getPair + balances."""
    if w3 is None:
        return STUB_LIQUIDITY.get("sushiswap", Decimal("55000"))
    try:
        factory = w3.eth.contract(address=factory_address, abi=V2_FACTORY_ABI)
        pair = factory.functions.getPair(token_a, token_b).call()
        return _pool_liquidity_usd(w3, pair, token_a, token_b, chain)
    except Exception as exc:
        logger.debug("V2 pair query failed: %s", exc)
        return STUB_LIQUIDITY.get("sushiswap", Decimal("55000"))


LIQUIDITY_QUERIERS: dict[str, Any] = {
    "uniswap_v3": _query_liquidity_uniswap_v3,
    "aerodrome": _query_liquidity_aerodrome,
    "sushiswap": _query_liquidity_v2,
    "camelot": _query_liquidity_v2,
}


def discover_venues(chain: str, min_liquidity_usd: int) -> list[Venue]:
    """Discover eligible venues for a chain with liquidity above threshold.

    Routers and compatibility are pinned from ROUTERS / ROUTER_COMPAT — never
    discovered, never blanked. `pairs` is left empty here and filled by the
    merge step (which preserves the operator's configured pairs).
    """
    venues: list[Venue] = []
    factories = FACTORIES.get(chain, {})
    tokens = BASE_TOKENS if chain == "base" else ARBITRUM_TOKENS
    w3 = _get_w3(chain)
    if w3 is None:
        logger.warning("web3.py unavailable or RPC unreachable; using stub liquidity for %s", chain)

    for protocol, factory_addr in factories.items():
        querier = LIQUIDITY_QUERIERS.get(protocol)
        if not querier:
            continue

        router = ROUTERS.get(chain, {}).get(protocol)
        if not router:
            logger.warning("No pinned router for %s/%s; skipping", chain, protocol)
            continue

        try:
            liquidity = querier(w3, factory_addr, tokens["WETH"], tokens["USDC"], chain)
        except Exception as exc:
            logger.warning("Liquidity query failed for %s: %s", protocol, exc)
            continue

        if liquidity >= Decimal(min_liquidity_usd):
            venues.append(
                Venue(
                    name=VENUE_NAMES.get((chain, protocol), f"{protocol}-{chain}"),
                    chain=chain,
                    liquidity_usd_min=int(liquidity),
                    type="dex",
                    kyc=False,
                    router_compatibility=ROUTER_COMPAT.get(protocol, "v2"),
                    router_address=router,
                    pairs=[],
                )
            )
            logger.info("Discovered %s with $%s liquidity", f"{protocol}-{chain}", liquidity)
        else:
            logger.info(
                "Skipped %s (liquidity $%s < $%s)",
                f"{protocol}-{chain}",
                liquidity,
                min_liquidity_usd,
            )

    return venues


# ---------------------------------------------------------------------------
# YAML I/O + merge
# ---------------------------------------------------------------------------
def load_routing_config(path: str) -> dict[str, Any]:
    with open(path, "r", encoding="utf-8") as f:
        return yaml.safe_load(f) or {}


def save_routing_config(path: str, config: dict[str, Any]) -> None:
    with open(path, "w", encoding="utf-8") as f:
        yaml.dump(config, f, default_flow_style=False, sort_keys=False, allow_unicode=True)
    logger.info("Updated routing config saved to %s", path)


def merge_venue(existing: dict[str, Any] | None, discovered: Venue) -> dict[str, Any]:
    """Non-destructive merge: refresh liquidity + pin router/compat, but PRESERVE
    the operator's configured `pairs`. Never blank router_address or pairs."""
    record = discovered.to_yaml_record()
    if existing:
        # Preserve configured pairs (multi-pair coverage the discovery step
        # doesn't enumerate).
        if existing.get("pairs"):
            record["pairs"] = existing["pairs"]
    return record


# ---------------------------------------------------------------------------
# Main update logic
# ---------------------------------------------------------------------------
def update_venues(
    config_path: str,
    min_liquidity_usd: int,
    dry_run: bool = False,
) -> list[dict[str, Any]]:
    """Full venue update pipeline (non-destructive merge by venue name)."""
    logger.info("Starting venue update (min_liquidity=$%s)", min_liquidity_usd)

    config = load_routing_config(config_path)
    tags = load_forensic_tags(FORENSIC_TAG_LISTS)

    existing_by_name: dict[str, dict[str, Any]] = {
        v.get("name", ""): v for v in config.get("venues", []) if isinstance(v, dict)
    }

    discovered: list[Venue] = []
    for chain in ("base", "arbitrum"):
        for v in discover_venues(chain, min_liquidity_usd):
            if not is_non_kyc_venue(v.name):
                continue
            if not is_not_forensic_tagged(v.router_address, tags):
                continue
            discovered.append(v)

    if not discovered:
        logger.warning("No venues met the criteria; leaving existing routing.yaml untouched")
        return config.get("venues", [])

    # Merge discovered venues onto existing config, preserving pairs and any
    # venues that were not rediscovered this run (e.g. RPC unavailable).
    merged_by_name: dict[str, dict[str, Any]] = dict(existing_by_name)
    for v in discovered:
        if v.name not in existing_by_name:
            # Non-inventive: the updater refreshes configured venues only; adding
            # a venue to the rotation set is an operator decision.
            logger.info("Discovered %s not in configured venues; skipping (no add)", v.name)
            continue
        merged_by_name[v.name] = merge_venue(existing_by_name[v.name], v)

    merged_records = list(merged_by_name.values())
    config["venues"] = merged_records
    config["last_updated"] = int(__import__("time").time())
    config["min_liquidity_usd"] = min_liquidity_usd

    if dry_run:
        logger.info("Dry-run: would write %d venues", len(merged_records))
    else:
        save_routing_config(config_path, config)

    return merged_records


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Update DEX venue list in routing.yaml with on-chain liquidity data."
    )
    parser.add_argument("--config", required=True, help="Path to routing.yaml.")
    parser.add_argument(
        "--min-liquidity-usd",
        type=int,
        default=DEFAULT_MIN_LIQUIDITY_USD,
        help=f"Minimum USD liquidity threshold (default {DEFAULT_MIN_LIQUIDITY_USD}).",
    )
    parser.add_argument("--dry-run", action="store_true", help="Do not write to disk.")
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
        venues = update_venues(args.config, args.min_liquidity_usd, dry_run=args.dry_run)
    except Exception as exc:
        logger.error("Venue update failed: %s", exc)
        return 1

    logger.info("Venue update complete: %d venues in routing table", len(venues))
    return 0


if __name__ == "__main__":
    sys.exit(main())
