#!/usr/bin/env python3
"""
scan_liquidation_venues.py
Project Chimera - cross-chain, cross-protocol liquidation FLOW + CONCENTRATION scan.

Generalises scripts/measure_liquidation_flow.py from one venue (Base Aave V3) to a
registry of lending venues across chains, and adds the metric that actually decides
strategy: WINNER CONCENTRATION. A venue is only worth entering if the flow is large
AND the winner set is fragmented.

Protocols supported:
  aave    - Aave V3 `LiquidationCall` (also covers Aave-V3 forks: Spark, Seamless, ...)
  morpho  - Morpho Blue `Liquidate` (resolves market id -> tokens via idToMarketParams)

Pricing: DefiLlama /prices/current (universal across chains; current prices, so the
same "current-price valuation" caveat as measure_liquidation_flow.py applies -- fine
for order-of-magnitude and concentration, not for per-event bonus figures).

Stdlib only. Credentials are read from .env.live and never printed.

Usage:
  python scripts/scan_liquidation_venues.py --days 30 --workdir <dir>
  python scripts/scan_liquidation_venues.py --days 30 --workdir <dir> --only arbitrum,ethereum
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import threading
import time
import urllib.request
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

TOPIC_AAVE_LIQ = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"
TOPIC_MORPHO_LIQ = "0xa4946ede45d0c6f06a0f5ce92c9ad3b4751452d2fe0e25010783bcab57a67e41"
SEL_ID_TO_MARKET_PARAMS = "0x2c3c9157"
SEL_DECIMALS = "0x313ce567"
SEL_SYMBOL = "0x95d89b41"
SEL_PRICE = "0xa035b1fe"                # IOracle.price()          (Morpho market oracle)
SEL_GET_CONFIGURATION = "0xc44b11f7"    # Aave Pool.getConfiguration(address)
SEL_ADDRESSES_PROVIDER = "0x0542975c"   # Aave Pool.ADDRESSES_PROVIDER()
SEL_GET_PRICE_ORACLE = "0xfca513a8"     # PoolAddressesProvider.getPriceOracle()
SEL_GET_ASSET_PRICE = "0xb3596f07"      # AaveOracle.getAssetPrice(address)  (8-dec USD)

# Morpho Blue: LIF = min(1.15, 1 / (1 - 0.3 * (1 - lltv))). Verified against
# Morpho.sol (MAX_LIQUIDATION_INCENTIVE_FACTOR=1.15e18, LIQUIDATION_CURSOR=0.3e18).
# seizedAssets = repaidAssets * LIF * ORACLE_PRICE_SCALE / oracle.price(), i.e. the
# seizure is denominated AT THE MARKET ORACLE PRICE. Any oracle-vs-market premium
# therefore shows up as phantom bonus if seized is valued structurally. That is why
# this scanner emits THREE numbers per venue, never one:
#   bonus_oracle       seized * (LIF-1)/LIF  -- what the protocol credits
#   bonus_exit         repaid_usd * (LIF/(1+premium) - 1) -- what an exit realises
#   oracle_premium_pct oraclePrice / tradedPrice - 1      -- the wedge between them
def morpho_lif(lltv: float) -> float:
    return min(1.15, 1.0 / (1.0 - 0.3 * (1.0 - lltv)))

# chain -> DefiLlama chain key
LLAMA_CHAIN = {
    "ethereum": "ethereum", "base": "base", "arbitrum": "arbitrum",
    "optimism": "optimism", "polygon": "polygon", "avalanche": "avax", "bsc": "bsc",
}

# (chain, protocol, label, address)
VENUES = [
    ("ethereum", "aave", "Aave V3", "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2"),
    ("ethereum", "aave", "Spark", "0xC13e21B648A5Ee794902342038FF3aDAB66BE987"),
    ("ethereum", "morpho", "Morpho Blue", "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb"),
    ("base", "aave", "Aave V3", "0xA238Dd80C259a72e81d7e4664a9801593F98d1c5"),
    ("base", "aave", "Seamless", "0x8F44Fd754285aa6A2b8B9B97739B79746e0475a7"),
    ("base", "morpho", "Morpho Blue", "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb"),
    ("arbitrum", "aave", "Aave V3", "0x794a61358D6845594F94dc1DB02A252b5b4814aD"),
    ("optimism", "aave", "Aave V3", "0x794a61358D6845594F94dc1DB02A252b5b4814aD"),
    ("polygon", "aave", "Aave V3", "0x794a61358D6845594F94dc1DB02A252b5b4814aD"),
    ("avalanche", "aave", "Aave V3", "0x794a61358D6845594F94dc1DB02A252b5b4814aD"),
    ("bsc", "aave", "Aave V3", "0x6807dc923806fE8Fd134338EABCA509979a7e0cB"),
]


def build_rpc_map() -> dict[str, str]:
    raw = (REPO / ".env.live").read_text(encoding="utf-8")
    ak = re.findall(r"alchemy\.com/v2/([A-Za-z0-9_-]{20,})", raw)
    ik = re.findall(r"infura\.io/(?:v3|ws/v3)/([A-Za-z0-9]{20,})", raw)
    if not ik and not ak:
        raise SystemExit("no provider key found in .env.live")
    a = ak[0] if ak else None
    i = ik[0] if ik else None
    m = {}
    if a:  # Alchemy app is scoped to eth + base only (probed 2026-07-25)
        m["base"] = f"https://base-mainnet.g.alchemy.com/v2/{a}"
        m["ethereum"] = f"https://eth-mainnet.g.alchemy.com/v2/{a}"
    if i:
        m.setdefault("ethereum", f"https://mainnet.infura.io/v3/{i}")
        m.setdefault("base", f"https://base-mainnet.infura.io/v3/{i}")
        m["arbitrum"] = f"https://arbitrum-mainnet.infura.io/v3/{i}"
        m["optimism"] = f"https://optimism-mainnet.infura.io/v3/{i}"
        m["polygon"] = f"https://polygon-mainnet.infura.io/v3/{i}"
        m["avalanche"] = f"https://avalanche-mainnet.infura.io/v3/{i}"
        m["bsc"] = f"https://bsc-mainnet.infura.io/v3/{i}"
    return m


RPC = build_rpc_map()
_lock = threading.Lock()
_last_call = {"t": 0.0}
MIN_GAP_S = 0.09  # global throttle; the shared Infura key 429s easily


def rpc(chain: str, method: str, params: list, timeout: int = 120, retries: int = 7):
    url = RPC[chain]
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    last = None
    for attempt in range(retries):
        with _lock:
            gap = time.time() - _last_call["t"]
            if gap < MIN_GAP_S:
                time.sleep(MIN_GAP_S - gap)
            _last_call["t"] = time.time()
        try:
            req = urllib.request.Request(
                url, data=body, headers={"Content-Type": "application/json"}
            )
            with urllib.request.urlopen(req, timeout=timeout) as r:
                out = json.loads(r.read())
            if "error" in out:
                raise RpcLogicError(out["error"])
            return out["result"]
        except RpcLogicError:
            raise
        except Exception as exc:  # 429 / 5xx / timeout
            last = exc
            time.sleep(min(3 * (2**attempt), 60))
    raise ConnectionError(f"{chain}.{method} failed: {last}")


class RpcLogicError(Exception):
    def __init__(self, err):
        self.code = err.get("code")
        self.message = str(err.get("message", ""))
        super().__init__(f"{self.code}: {self.message[:120]}")


def word(h: str, i: int) -> str:
    return h[2 + 64 * i: 2 + 64 * (i + 1)]


def addr_of(w: str) -> str:
    return "0x" + w[-40:]


def block_at_timestamp(chain: str, target_ts: int, tip: int, tip_ts: int) -> int:
    """Locate the block at target_ts by rate estimation + bounded refinement.

    A full binary search costs ~29 sequential RPC calls per chain, which on a
    rate-limited provider degenerates into a backoff stall. Chains here have
    near-constant block times, so estimate then refine: typically 3-5 calls.
    """
    probe = max(1, tip - 200_000)
    pb = rpc(chain, "eth_getBlockByNumber", [hex(probe), False])
    dt = (tip_ts - int(pb["timestamp"], 16)) / max(1, tip - probe)
    if dt <= 0:
        dt = 2.0
    guess = max(1, min(tip, int(tip - (tip_ts - target_ts) / dt)))
    for _ in range(12):
        b = rpc(chain, "eth_getBlockByNumber", [hex(guess), False])
        err = int(b["timestamp"], 16) - target_ts
        if abs(err) <= 180:  # within 3 minutes of a 30-day window: immaterial
            break
        nxt = max(1, min(tip, guess - int(err / dt)))
        if nxt == guess:
            break
        guess = nxt
    return guess


# chain -> largest eth_getLogs block span the provider accepts. Seeded from probing
# on 2026-07-25: the Infura L2 endpoints cap at 10k blocks, Ethereum/Base do not.
_SPAN_CAP: dict[str, int] = {
    "arbitrum": 10_000, "optimism": 10_000, "polygon": 10_000, "avalanche": 10_000,
}


def fetch_logs(chain: str, address: str, topic0: str, frm: int, to: int) -> list:
    """Adaptive-span log fetch.

    Providers cap eth_getLogs block ranges (Infura: 10k on the L2 endpoints) and
    state the cap in the error. Parse it and latch to it rather than rediscovering
    it by repeated halving -- and never regrow past the latched cap, otherwise every
    other request is a guaranteed rejection.
    """
    out, cur = [], frm
    span = _SPAN_CAP.get(chain, 1_000_000)
    last_pct = -10.0
    while cur <= to:
        end = min(cur + span - 1, to)
        try:
            logs = rpc(chain, "eth_getLogs", [{
                "address": address, "topics": [topic0],
                "fromBlock": hex(cur), "toBlock": hex(end)}])
        except RpcLogicError as e:
            m = re.search(r"limit of (\d+)", e.message)
            new = int(m.group(1)) if m else max(2_000, span // 4)
            if new >= span:
                new = max(2_000, span // 4)
            if span <= 2_000 and not m:
                raise
            span = new
            _SPAN_CAP[chain] = span
            print(f"      [{chain}] span-> {span:,} ({e.message[:55]})", flush=True)
            continue
        out.extend(logs)
        cur = end + 1
        span = min(_SPAN_CAP.get(chain, 1_000_000), span)
        if (to - frm) > 2_000_000:
            pct = 100.0 * (cur - frm) / (to - frm)
            if pct - last_pct >= 10.0:
                last_pct = pct
                print(f"      [{chain}] {pct:5.1f}%  {len(out)} events", flush=True)
    return out


def token_meta(chain: str, addr: str, cache: dict) -> dict:
    key = (chain, addr)
    if key in cache:
        return cache[key]
    dec, sym = 18, "?"
    try:
        dec = int(rpc(chain, "eth_call", [{"to": addr, "data": SEL_DECIMALS}, "latest"]), 16)
    except Exception:
        pass
    try:
        r = rpc(chain, "eth_call", [{"to": addr, "data": SEL_SYMBOL}, "latest"])
        raw = r[2:]
        if len(raw) == 64:
            sym = bytes.fromhex(raw).rstrip(b"\x00").decode("utf-8", "replace")
        elif len(raw) > 128:
            n = int(raw[64:128], 16)
            sym = bytes.fromhex(raw[128:128 + 2 * n]).decode("utf-8", "replace")
    except Exception:
        pass
    cache[key] = {"decimals": dec, "symbol": sym}
    return cache[key]


def llama_prices(pairs: list[tuple[str, str]]) -> dict:
    """pairs of (chain, token_addr) -> {(chain,addr): price_usd}"""
    out, B = {}, 40
    keys = [f"{LLAMA_CHAIN[c]}:{a}" for c, a in pairs]
    for i in range(0, len(keys), B):
        chunk = keys[i:i + B]
        url = "https://coins.llama.fi/prices/current/" + ",".join(chunk)
        try:
            with urllib.request.urlopen(url, timeout=60) as r:
                data = json.loads(r.read()).get("coins", {})
        except Exception as e:
            print(f"   price batch failed: {str(e)[:70]}")
            data = {}
        for k, v in data.items():
            ch, ad = k.split(":", 1)
            inv = {v2: k2 for k2, v2 in LLAMA_CHAIN.items()}
            out[(inv.get(ch, ch), ad.lower())] = v.get("price")
    return out


def morpho_market(chain: str, morpho_addr: str, mid: str, cache: dict) -> dict | None:
    if mid in cache:
        return cache[mid]
    try:
        r = rpc(chain, "eth_call", [{"to": morpho_addr,
                                     "data": SEL_ID_TO_MARKET_PARAMS + mid}, "latest"])
        cache[mid] = {"loan": addr_of(word(r, 0)).lower(),
                      "collateral": addr_of(word(r, 1)).lower(),
                      "oracle": addr_of(word(r, 2)).lower(),
                      "lltv": int(word(r, 4), 16) / 1e18,
                      "mid": mid}
    except Exception:
        cache[mid] = None
    return cache[mid]


def morpho_oracle_price(chain: str, oracle: str, dl: int, dc: int, cache: dict) -> float | None:
    """Current oracle price of 1 collateral token, expressed in LOAN tokens.

    IOracle.price() is scaled by 1e(36 + loanDecimals - collateralDecimals).
    A zero-address oracle (AZND-style hardcoded markets) returns None here and is
    treated as price 1.0 loan/collateral by the caller.
    """
    key = (chain, oracle)
    if key in cache:
        return cache[key]
    out = None
    if oracle and int(oracle, 16) != 0:
        try:
            r = rpc(chain, "eth_call", [{"to": oracle, "data": SEL_PRICE}, "latest"])
            out = int(r, 16) / 10 ** (36 + dl - dc)
        except Exception:
            out = None
    cache[key] = out
    return out


def aave_liq_bonus(chain: str, pool: str, asset: str, cache: dict) -> float | None:
    """Aave V3 per-reserve liquidationBonus factor (e.g. 1.05) from the
    getConfiguration bitmap, bits 32-47, in bps (10500 = 105%). eMode categories
    can override this per user; survey-grade approximation, noted in output."""
    key = (chain, pool, asset)
    if key in cache:
        return cache[key]
    out = None
    try:
        r = rpc(chain, "eth_call", [{"to": pool,
                                     "data": SEL_GET_CONFIGURATION + asset[2:].rjust(64, "0")},
                                    "latest"])
        bps = (int(r, 16) >> 32) & 0xFFFF
        out = bps / 10000.0 if bps else None
    except Exception:
        out = None
    cache[key] = out
    return out


def aave_oracle_usd(chain: str, pool: str, asset: str, caches: dict) -> float | None:
    """Current AaveOracle USD price (8 decimals) for an asset on this venue."""
    ocache = caches.setdefault("oracle_addr", {})
    pcache = caches.setdefault("px", {})
    if (chain, pool) not in ocache:
        try:
            prov = addr_of(rpc(chain, "eth_call",
                               [{"to": pool, "data": SEL_ADDRESSES_PROVIDER}, "latest"]))
            orc = addr_of(rpc(chain, "eth_call",
                              [{"to": prov, "data": SEL_GET_PRICE_ORACLE}, "latest"]))
            ocache[(chain, pool)] = orc
        except Exception:
            ocache[(chain, pool)] = None
    orc = ocache[(chain, pool)]
    if not orc:
        return None
    key = (chain, orc, asset)
    if key in pcache:
        return pcache[key]
    out = None
    try:
        r = rpc(chain, "eth_call", [{"to": orc,
                                     "data": SEL_GET_ASSET_PRICE + asset[2:].rjust(64, "0")},
                                    "latest"])
        out = int(r, 16) / 1e8
    except Exception:
        out = None
    pcache[key] = out
    return out


def scan_venue(chain, protocol, label, address, days, workdir):
    tag = f"{chain}/{label}"
    raw_path = workdir / f"raw_{chain}_{label.replace(' ', '')}.jsonl"
    meta_path = workdir / f"meta_{chain}_{label.replace(' ', '')}.json"
    try:
        # Reuse a completed fetch so a re-run does not re-hit the provider.
        if raw_path.exists() and meta_path.exists():
            meta = json.loads(meta_path.read_text())
            logs = [json.loads(l) for l in raw_path.read_text(encoding="utf-8").splitlines() if l]
            print(f"  [{tag}] cached: {len(logs)} events", flush=True)
            return {"venue": tag, "chain": chain, "protocol": protocol, "label": label,
                    "address": address, "logs": logs, **meta}

        tip = int(rpc(chain, "eth_blockNumber", []), 16)
        tb = rpc(chain, "eth_getBlockByNumber", [hex(tip), False])
        now_ts = int(tb["timestamp"], 16)
        code = rpc(chain, "eth_getCode", [address, "latest"])
        if len(code) <= 2:
            return {"venue": tag, "error": "no contract code at address"}
        frm = block_at_timestamp(chain, now_ts - days * 86400, tip, now_ts)
        topic = TOPIC_AAVE_LIQ if protocol == "aave" else TOPIC_MORPHO_LIQ
        print(f"  [{tag}] blocks {frm}-{tip} ({tip - frm:,})", flush=True)
        logs = fetch_logs(chain, address, topic, frm, tip)
        print(f"  [{tag}] {len(logs)} raw events", flush=True)
        raw_path.write_text("\n".join(json.dumps(x) for x in logs), encoding="utf-8")
        meta_path.write_text(json.dumps(
            {"from_block": frm, "to_block": tip, "now_ts": now_ts}))
        return {"venue": tag, "chain": chain, "protocol": protocol, "label": label,
                "address": address, "from_block": frm, "to_block": tip,
                "now_ts": now_ts, "logs": logs}
    except Exception as e:
        print(f"  [{tag}] FAILED: {str(e)[:110]}", flush=True)
        return {"venue": tag, "chain": chain, "label": label, "error": str(e)[:200]}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--days", type=int, default=30)
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--only", default=None, help="comma-separated chain filter")
    ap.add_argument("--exit-prices", default=None,
                    help="JSON {chain:{token: usd}} from the Gate-0 quote probe; "
                         "overrides DefiLlama as the traded price")
    ap.add_argument("--validate", action="store_true",
                    help="compare against the frozen Base Aave control and the "
                         "AVLT collapse expectation, print PASS/FAIL")
    args = ap.parse_args()

    workdir = Path(args.workdir)
    workdir.mkdir(parents=True, exist_ok=True)

    venues = [v for v in VENUES if v[0] in RPC]
    if args.only:
        keep = {c.strip() for c in args.only.split(",")}
        venues = [v for v in venues if v[0] in keep]
    print(f"Scanning {len(venues)} venues across {len({v[0] for v in venues})} chains, "
          f"{args.days}d window\n")

    with ThreadPoolExecutor(max_workers=4) as ex:
        scans = list(ex.map(
            lambda v: scan_venue(v[0], v[1], v[2], v[3], args.days, workdir), venues))

    # ---- decode ----------------------------------------------------------
    tmeta, mmarket = {}, {}
    decoded = {}
    for s in scans:
        if "error" in s:
            decoded[s["venue"]] = s
            continue
        chain, proto = s["chain"], s["protocol"]
        evs = []
        seen = set()
        for lg in s["logs"]:
            key = (lg["transactionHash"], lg["logIndex"])
            if key in seen:
                continue
            seen.add(key)
            d = lg["data"]
            if proto == "aave":
                ev = {"collateral": addr_of(lg["topics"][1]).lower(),
                      "debt": addr_of(lg["topics"][2]).lower(),
                      "user": addr_of(lg["topics"][3]).lower(),
                      "repaid_raw": int(word(d, 0), 16),
                      "seized_raw": int(word(d, 1), 16),
                      "liquidator": addr_of(word(d, 2)).lower()}
            else:
                mid = lg["topics"][1][2:]
                mk = morpho_market(chain, s["address"], mid, mmarket)
                if not mk:
                    continue
                ev = {"collateral": mk["collateral"], "debt": mk["loan"],
                      "user": addr_of(lg["topics"][3]).lower(),
                      "repaid_raw": int(word(d, 0), 16),
                      "seized_raw": int(word(d, 2), 16),
                      "liquidator": addr_of(lg["topics"][2]).lower(),
                      "mid": mid, "lltv": mk["lltv"], "oracle": mk["oracle"]}
            ev["block"] = int(lg["blockNumber"], 16)
            ev["tx"] = lg["transactionHash"]
            evs.append(ev)
        s["events"] = evs
        decoded[s["venue"]] = s

    # ---- price ------------------------------------------------------------
    pairs = set()
    for s in decoded.values():
        if "error" in s:
            continue
        for e in s["events"]:
            pairs.add((s["chain"], e["collateral"]))
            pairs.add((s["chain"], e["debt"]))
    print(f"\nPricing {len(pairs)} distinct tokens via DefiLlama...")
    prices = llama_prices(sorted(pairs))
    print(f"  resolved {len(prices)}/{len(pairs)}")

    # ---- aggregate ---------------------------------------------------------
    # exit-price overrides: {"<chain>": {"<token_lower>": usd_price}} produced by
    # the Gate-0 quote probe (aggregator-implied exit price at the median clip).
    exit_px: dict = {}
    if args.exit_prices:
        exit_px = json.loads(Path(args.exit_prices).read_text(encoding="utf-8"))
    ocache: dict = {}      # morpho oracle price cache
    acache: dict = {}      # aave liq-bonus cache
    aoracle: dict = {}     # aave oracle addr + px caches
    report = []
    for s in decoded.values():
        if "error" in s:
            report.append({"venue": s["venue"], "error": s["error"]})
            continue
        chain, proto = s["chain"], s["protocol"]
        priced, unpriced = [], 0
        markets = {}       # per market (morpho) / per collateral (aave) breakdown
        for e in s["events"]:
            mc = token_meta(chain, e["collateral"], tmeta)
            md = token_meta(chain, e["debt"], tmeta)
            pc = prices.get((chain, e["collateral"]))
            pd = prices.get((chain, e["debt"]))
            traded_pc = (exit_px.get(chain, {}) or {}).get(e["collateral"], pc)
            if pd is None or (pc is None and traded_pc is None):
                unpriced += 1
                continue
            e["debt_usd"] = e["repaid_raw"] / 10 ** md["decimals"] * pd
            e["csym"], e["dsym"] = mc["symbol"], md["symbol"]

            # --- LIF and current oracle premium, per market/asset -------------
            if proto == "morpho":
                lif = morpho_lif(e["lltv"])
                opx_loan = morpho_oracle_price(chain, e["oracle"],
                                               md["decimals"], mc["decimals"], ocache)
                if opx_loan is None:
                    opx_loan = 1.0  # hardcoded-1 oracle (AZND-style) or unreadable
                    e["oracle_note"] = "no-oracle-price-read"
                oracle_usd = opx_loan * pd
                mkey = e["mid"]
            else:
                lb = aave_liq_bonus(chain, s["address"], e["collateral"], acache)
                lif = lb if lb else 1.05
                oracle_usd = aave_oracle_usd(chain, s["address"], e["collateral"], aoracle)
                mkey = e["collateral"]

            premium = None
            if traded_pc and oracle_usd:
                premium = oracle_usd / traded_pc - 1.0

            # seized valued at traded/exit price (NOT structurally):
            val_pc = traded_pc if traded_pc is not None else pc
            e["seized_usd"] = e["seized_raw"] / 10 ** mc["decimals"] * val_pc
            e["bonus_naive_usd"] = e["seized_usd"] - e["debt_usd"]      # legacy number
            # what the protocol credits, at oracle denomination:
            e["bonus_oracle_usd"] = e["seized_usd"] * (lif - 1.0) / lif
            # what an exit realises: seized is worth repaid*LIF at ORACLE prices by
            # construction, so at market prices it is repaid*LIF/(1+premium).
            if premium is not None:
                e["bonus_exit_usd"] = e["debt_usd"] * (lif / (1.0 + premium) - 1.0)
            else:
                e["bonus_exit_usd"] = None
            e["lif"] = lif
            e["oracle_premium_pct"] = 100 * premium if premium is not None else None
            priced.append(e)

            m = markets.setdefault(mkey, {
                "market_id": ("0x" + mkey) if proto == "morpho" else None,
                "pair": f"{mc['symbol']}/{md['symbol']}", "n": 0,
                "collateral": e["collateral"], "debt": e["debt"], "lif": lif,
                "lltv": e.get("lltv"), "oracle": e.get("oracle"),
                "oracle_premium_pct": e["oracle_premium_pct"],
                "seized_usd": 0.0, "repaid_usd": 0.0,
                "bonus_oracle_usd": 0.0, "bonus_exit_usd": 0.0,
                "exit_priced": premium is not None, "_clips": []})
            m["n"] += 1
            m["seized_usd"] += e["seized_usd"]
            m["repaid_usd"] += e["debt_usd"]
            m["bonus_oracle_usd"] += e["bonus_oracle_usd"]
            m["_clips"].append(e["debt_usd"])
            if e["bonus_exit_usd"] is not None:
                m["bonus_exit_usd"] += e["bonus_exit_usd"]

        tot = sum(e["seized_usd"] for e in priced)
        bonus = sum(e["bonus_naive_usd"] for e in priced)
        bonus_oracle = sum(e["bonus_oracle_usd"] for e in priced)
        exit_events = [e for e in priced if e["bonus_exit_usd"] is not None]
        bonus_exit = sum(e["bonus_exit_usd"] for e in exit_events)
        big = [e for e in priced if e["seized_usd"] >= 1000]
        byliq = defaultdict(lambda: {"n": 0, "usd": 0.0})
        for e in priced:
            b = byliq[e["liquidator"]]
            b["n"] += 1
            b["usd"] += e["seized_usd"]
        ranked = sorted(byliq.items(), key=lambda kv: -kv[1]["usd"])
        top1 = 100 * ranked[0][1]["usd"] / tot if tot and ranked else 0.0
        top3 = 100 * sum(v["usd"] for _, v in ranked[:3]) / tot if tot else 0.0
        hhi = sum((100 * v["usd"] / tot) ** 2 for _, v in ranked) if tot else 0.0
        # concentration among MEANINGFUL events only (dust distorts counts)
        bigliq = defaultdict(float)
        for e in big:
            bigliq[e["liquidator"]] += e["seized_usd"]
        bigranked = sorted(bigliq.items(), key=lambda kv: -kv[1])
        bigtot = sum(bigliq.values())
        bigtop1 = 100 * bigranked[0][1] / bigtot if bigtot else 0.0

        mrows = sorted(markets.values(), key=lambda m: -m["bonus_oracle_usd"])
        for m in mrows:
            clips = sorted(m.pop("_clips"))
            m["median_clip_usd"] = clips[len(clips) // 2] if clips else 0.0
            m["p90_clip_usd"] = clips[int(len(clips) * 0.9)] if clips else 0.0
        report.append({
            "venue": s["venue"], "chain": chain, "label": s["label"],
            "address": s["address"], "blocks": [s["from_block"], s["to_block"]],
            "events": len(priced), "unpriced_events": unpriced,
            "seized_usd": tot,
            "gross_bonus_usd": bonus,               # legacy naive: seized - repaid @current px
            "bonus_oracle_usd": bonus_oracle,       # structural: seized * (LIF-1)/LIF
            "bonus_exit_usd": bonus_exit,           # repaid * (LIF/(1+premium) - 1)
            "bonus_exit_covered_events": len(exit_events),
            "events_ge_1k": len(big), "seized_usd_ge_1k": sum(e["seized_usd"] for e in big),
            "unique_liquidators": len(byliq),
            "unique_liquidators_ge_1k": len(bigliq),
            "top1_share_pct": top1, "top3_share_pct": top3, "hhi": hhi,
            "top1_share_pct_ge_1k": bigtop1,
            "top_liquidators": [{"addr": a, "n": v["n"], "usd": v["usd"]}
                                for a, v in ranked[:8]],
            "largest_event_usd": max((e["seized_usd"] for e in priced), default=0.0),
            "markets": mrows[:40],
        })

    report.sort(key=lambda r: -r.get("gross_bonus_usd", -1))
    (workdir / "venue_report.json").write_text(json.dumps(report, indent=2))

    # ---- print --------------------------------------------------------------
    print("\n" + "=" * 130)
    print(f"{args.days}-DAY LIQUIDATION FLOW BY VENUE  "
          f"(three-number bonus: oracle / exit / naive)")
    print("=" * 130)
    hdr = (f"{'venue':<26}{'events':>7}{'seized $':>13}{'bonus_oracle':>13}"
           f"{'bonus_exit':>12}{'naive':>10}"
           f"{'liqs':>6}{'top1%':>7}{'top1%>=1k':>10}")
    print(hdr)
    print("-" * 130)
    for r in report:
        if "error" in r:
            print(f"{r['venue']:<26} ERROR: {r['error'][:78]}")
            continue
        print(f"{r['venue']:<26}{r['events']:>7}"
              f"{r['seized_usd']:>13,.0f}{r['bonus_oracle_usd']:>13,.0f}"
              f"{r['bonus_exit_usd']:>12,.0f}{r['gross_bonus_usd']:>10,.0f}"
              f"{r['unique_liquidators']:>6}"
              f"{r['top1_share_pct']:>7.1f}{r['top1_share_pct_ge_1k']:>10.1f}")
    ok = [r for r in report if "error" not in r]
    print("-" * 130)
    print(f"{'TOTAL':<26}{sum(r['events'] for r in ok):>7}"
          f"{sum(r['seized_usd'] for r in ok):>13,.0f}"
          f"{sum(r['bonus_oracle_usd'] for r in ok):>13,.0f}"
          f"{sum(r['bonus_exit_usd'] for r in ok):>12,.0f}"
          f"{sum(r['gross_bonus_usd'] for r in ok):>10,.0f}")
    print("\nbonus_oracle = seized*(LIF-1)/LIF (protocol credit, oracle-denominated)")
    print("bonus_exit   = repaid*(LIF/(1+premium)-1) at the CURRENT oracle premium;")
    print("               assumes current premium ~ event-time premium (exact for frozen/pinned feeds)")
    print("naive        = seized - repaid at current prices (legacy; kept as the control channel)")
    print(f"\nArtifacts: {workdir / 'venue_report.json'}")

    # ---- validation gate ----------------------------------------------------
    if args.validate:
        print("\n" + "=" * 78)
        print("VALIDATION GATE — frozen controls (from 2026-07-25 measurement)")
        print("=" * 78)
        CONTROL = {"events": 116, "seized_usd": 79790.0, "gross_bonus_usd": 4404.0,
                   "top1_share_pct": 85.4}
        AVLT_MID = "49b89ee666acc242de6a2f25ffdbb25dcc693a7f4746d8d4afeb51c440fa938a"
        ok_all = True
        ba = next((r for r in report if r.get("venue") == "base/Aave V3"), None)
        if not ba or "error" in ba:
            print("FAIL: base/Aave V3 row missing or errored"); ok_all = False
        else:
            for k, want in CONTROL.items():
                got = ba[k]
                # events and top1 must match exactly; USD values drift with 24h of
                # DefiLlama price movement — allow 3%.
                tol = 0.0 if k in ("events",) else (0.15 if k == "top1_share_pct" else want * 0.03)
                good = abs(got - want) <= tol
                ok_all &= good
                print(f"  base/Aave V3 {k:<18} got {got:>12,.1f}  want {want:>12,.1f} "
                      f" {'OK' if good else 'FAIL'}")
        em = next((r for r in report if r.get("venue") == "ethereum/Morpho Blue"), None)
        if em and "error" not in em:
            avlt = next((m for m in em["markets"]
                         if m.get("collateral") == "0x74db7a52773a52699dbc0c01b1254e5301e3e119"
                         or AVLT_MID in str(m)), None)
            if avlt:
                collapsed = (avlt["bonus_exit_usd"] < 0.15 * avlt["bonus_oracle_usd"])
                ok_all &= collapsed
                print(f"  AVLT/USDC bonus_oracle ${avlt['bonus_oracle_usd']:,.0f} "
                      f"-> bonus_exit ${avlt['bonus_exit_usd']:,.0f} "
                      f"(premium {avlt['oracle_premium_pct']}) "
                      f"{'COLLAPSED OK' if collapsed else 'FAIL: did not collapse'}")
            else:
                print("  AVLT/USDC market not found in ethereum/Morpho Blue rows")
        print(f"\nVALIDATION: {'PASS' if ok_all else 'FAIL'}")
        return 0 if ok_all else 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
