#!/usr/bin/env python3
"""
check_venue_open.py
Project Chimera - is a lending venue still OPEN to public-RPC competition?

Chainlink SVR (Smart Value Recapture) routes a protocol's liquidation flow into a
sealed-bid oracle auction. Where it is enabled, the price update that makes a
position liquidatable lands inside the winning solver's own bundle -- so a bot
watching a public RPC sees the opportunity only after it is gone. That is not a
latency problem and cannot be fixed with a faster node.

This is the two-call gate that detects it, and it should be run BEFORE any
engineering is spent on a new venue:

    Pool -> ADDRESSES_PROVIDER() -> getPriceOracle() -> getSourceOfAsset(asset)
         -> typeAndVersion() on the returned aggregator

If the aggregator reports "DualAggregator" (or otherwise wraps an SVR feed), the
venue is CLOSED. A plain EACAggregatorProxy / AccessControlledOffchainAggregator
means OPEN.

Note: SVR covers Aave, Compound and Venus. Morpho Blue is NOT an SVR protocol --
its markets carry their own oracles -- so Morpho is structurally open regardless
of what this script reports for Aave on the same chain.

Usage:
  python scripts/check_venue_open.py
  python scripts/check_venue_open.py --chain base
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

SEL = {
    "ADDRESSES_PROVIDER()": "0x0542975c",
    "getPriceOracle()": "0xfca513a8",
    "getSourceOfAsset(address)": "0x92bf2be0",
    "typeAndVersion()": "0x181f5a77",
    "description()": "0x7284e416",
    "aggregator()": "0x245a7bfc",
    "symbol()": "0x95d89b41",
    # --- extension: Compound III Comet ---
    "numAssets()": "0xa46fe83b",
    "getAssetInfo(uint8)": "0xc8c7fe6b",
    "baseTokenPriceFeed()": "0xe7dad6bd",
    "baseToken()": "0xc55dae63",
    "underlyingPriceFeed()": "0x2ac0a1a5",   # Comet ScalingPriceFeed wrapper
    # --- extension: Morpho Blue ---
    "idToMarketParams(bytes32)": "0x2c3c9157",
    "BASE_FEED_1()": "0xf50a4718",
    "BASE_FEED_2()": "0xdc53858c",
    "QUOTE_FEED_1()": "0x56095e11",
    "QUOTE_FEED_2()": "0xacfbd39e",
    # --- extension: Moonwell / Compound V2 fork ---
    "oracle()": "0x7dc0d1d0",
    "getFeed(string)": "0x3b39a51c",
    "underlying()": "0x6f307dc3",
    # --- extension: Aave "Capped" price adapters ---
    "ASSET_TO_USD_AGGREGATOR()": "0x4ebdc284",
    "BASE_TO_USD_AGGREGATOR()": "0xd221087c",
    "ASSET_TO_BASE_AGGREGATOR()": "0x89f9625b",
}

# Terminal Chainlink aggregator names: reaching one of these ends the walk.
TERMINAL = ("accesscontrolledoffchainaggregator", "accesscontrolledocr2aggregator",
            "offchainaggregator", "ocr2aggregator")
# Names that mean the venue's trigger price is inside an SVR auction bundle.
CLOSED_MARKERS = ("dualaggregator", "svr")

# venue -> (chain, pool address, [assets to sample])
VENUES = {
    "Aave V3 (Base)": ("base", "0xA238Dd80C259a72e81d7e4664a9801593F98d1c5", [
        ("WETH", "0x4200000000000000000000000000000000000006"),
        ("cbBTC", "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf"),
        ("USDC", "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
    ]),
    "Seamless (Base)": ("base", "0x8F44Fd754285aa6A2b8B9B97739B79746e0475a7", [
        ("WETH", "0x4200000000000000000000000000000000000006"),
        ("USDC", "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
    ]),
    "Aave V3 (Ethereum)": ("ethereum", "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2", [
        ("WETH", "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        ("WBTC", "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
        ("USDC", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
    ]),
    "Spark (Ethereum)": ("ethereum", "0xC13e21B648A5Ee794902342038FF3aDAB66BE987", [
        ("WETH", "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        ("wstETH", "0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0"),
    ]),
}


def rpc_map() -> dict[str, str]:
    raw = (REPO / ".env.live").read_text(encoding="utf-8")
    ak = re.findall(r"alchemy\.com/v2/([A-Za-z0-9_-]{20,})", raw)
    ik = re.findall(r"infura\.io/(?:v3|ws/v3)/([A-Za-z0-9]{20,})", raw)
    m = {}
    if ak:
        m["base"] = f"https://base-mainnet.g.alchemy.com/v2/{ak[0]}"
        m["ethereum"] = f"https://eth-mainnet.g.alchemy.com/v2/{ak[0]}"
    elif ik:
        m["base"] = f"https://base-mainnet.infura.io/v3/{ik[0]}"
        m["ethereum"] = f"https://mainnet.infura.io/v3/{ik[0]}"
    if not m:
        raise SystemExit("no provider key found in .env.live")
    return m


RPC = rpc_map()


# Budget guard: this gate is meant to be cheap. Every eth_call is counted, and
# identical (chain, to, data) triples are served from cache -- the same Chainlink
# ETH/USD proxy is reached from a dozen different venues, so caching is most of
# the savings.
RPC_BUDGET = 420
_stats = {"calls": 0, "cached": 0, "capped": False}
_cache: dict[tuple[str, str, str], str | None] = {}


def call(chain: str, to: str, data: str) -> str | None:
    key = (chain, to.lower(), data.lower())
    if key in _cache:
        _stats["cached"] += 1
        return _cache[key]
    if _stats["calls"] >= RPC_BUDGET:
        _stats["capped"] = True
        return None
    _stats["calls"] += 1
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "eth_call",
                       "params": [{"to": to, "data": data}, "latest"]}).encode()
    out = None
    try:
        req = urllib.request.Request(RPC[chain], data=body,
                                     headers={"Content-Type": "application/json"})
        o = json.loads(urllib.request.urlopen(req, timeout=45).read())
        out = None if "error" in o else o["result"]
    except Exception:
        out = None
    _cache[key] = out
    return out


def decode_string(res: str | None) -> str:
    if not res or res == "0x":
        return ""
    raw = res[2:]
    if len(raw) == 64:
        return bytes.fromhex(raw).rstrip(b"\x00").decode("utf-8", "replace")
    try:
        n = int(raw[64:128], 16)
        return bytes.fromhex(raw[128:128 + 2 * n]).decode("utf-8", "replace")
    except Exception:
        return ""


def addr(res: str | None) -> str | None:
    if not res or len(res) < 66:
        return None
    a = "0x" + res[-40:]
    return None if int(a, 16) == 0 else a


# ---------------------------------------------------------------------------
# EXTENSION: the same two-call gate, applied past Aave-family pools.
#
# Aave exposes its feed via oracle.getSourceOfAsset(). Every other venue family
# hands you the aggregator by a different route (Comet.getAssetInfo().priceFeed,
# Moonwell's ChainlinkOracle.getFeed(symbol), Morpho's per-market oracle), but
# once you HAVE an aggregator address the verdict logic is identical. walk_feed()
# is that shared tail: descend wrappers/proxies until a terminal Chainlink
# aggregator names itself, and call it CLOSED the moment "DualAggregator" or
# "SVR" shows up at any level.
# ---------------------------------------------------------------------------

def immutable_children(chain: str, a: str) -> list[str]:
    """Aave's Capped/peg adapters and Comet's CAPO feeds hold their constituent
    aggregators as Solidity *immutables* -- inlined into the deployed bytecode as
    PUSH32 (address in the low 20 bytes) or PUSH20, with no getter at all. One
    eth_getCode recovers them. Without this, a composite adapter looks like a
    dead end and gets scored OPEN when a DualAggregator is sitting underneath it
    (this is exactly how Aave-Ethereum WBTC and Comet-Ethereum WBTC hide theirs).
    """
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "eth_getCode",
                       "params": [a, "latest"]}).encode()
    if _stats["calls"] >= RPC_BUDGET:
        _stats["capped"] = True
        return []
    _stats["calls"] += 1
    try:
        req = urllib.request.Request(RPC[chain], data=body,
                                     headers={"Content-Type": "application/json"})
        code = json.loads(urllib.request.urlopen(req, timeout=45).read()).get("result") or "0x"
    except Exception:
        return []
    h, out = code[2:], []
    for m in re.findall(r"7f(0{24}[0-9a-f]{40})", h):
        c = "0x" + m[24:]
        if int(c, 16) > (1 << 100) and c not in out:
            out.append(c)
    for m in re.findall(r"73([0-9a-f]{40})", h):
        c = "0x" + m
        if int(c, 16) > (1 << 130) and c not in out:
            out.append(c)
    return out[:10]


def walk_feed(chain: str, feed: str, max_depth: int = 4) -> dict:
    """Return {'chain','feed','path':[...], 'label','desc','verdict'}.

    verdict is CLOSED / OPEN / UNRESOLVED. UNRESOLVED is NOT open -- it means the
    walk never reached a self-identifying Chainlink aggregator, and must be
    settled by hand before the venue is treated as competable.
    """
    path, desc = [], ""
    cur, seen = feed, set()
    for depth in range(max_depth):
        if not cur or cur.lower() in seen:
            break
        seen.add(cur.lower())
        tv = decode_string(call(chain, cur, SEL["typeAndVersion()"]))
        de = decode_string(call(chain, cur, SEL["description()"]))
        if depth == 0:
            desc = de
        path.append({"addr": cur, "typeAndVersion": tv, "description": de})
        # Marker can appear in EITHER field: Chainlink's SVR feeds are plain
        # proxies with no typeAndVersion but a description of "X / USD SVR Price
        # Feed". Testing typeAndVersion alone scores those OPEN. It is not.
        low = f"{tv} {de}".lower()
        if any(m in low for m in CLOSED_MARKERS):
            return {"chain": chain, "feed": feed, "path": path,
                    "label": tv or de, "desc": desc, "verdict": "CLOSED"}
        if any(t in low for t in TERMINAL):
            return {"chain": chain, "feed": feed, "path": path,
                    "label": tv, "desc": desc, "verdict": "OPEN"}
        nxt = None
        for s in ("aggregator()", "underlyingPriceFeed()", "priceFeed()",
                  "ASSET_TO_USD_AGGREGATOR()", "BASE_TO_USD_AGGREGATOR()",
                  "ASSET_TO_BASE_AGGREGATOR()"):
            if s not in SEL:
                continue
            nxt = addr(call(chain, cur, SEL[s]))
            if nxt:
                break
        if not nxt and depth == 0:
            # composite adapter with no getters -- score every immutable it holds
            for cand in immutable_children(chain, cur):
                ctv = decode_string(call(chain, cand, SEL["typeAndVersion()"]))
                cde = decode_string(call(chain, cand, SEL["description()"]))
                cin = addr(call(chain, cand, SEL["aggregator()"]))
                ctvi = decode_string(call(chain, cin, SEL["typeAndVersion()"])) if cin else ""
                if not (ctv or cde or cin):
                    continue
                blob = f"{ctv} {ctvi} {cde}".lower()
                path.append({"addr": cand, "typeAndVersion": ctv or ctvi,
                             "description": cde})
                if any(m in blob for m in CLOSED_MARKERS):
                    return {"chain": chain, "feed": feed, "path": path,
                            "label": ctv or ctvi or cde, "desc": desc,
                            "verdict": "CLOSED"}
            return {"chain": chain, "feed": feed, "path": path,
                    "label": f"composite adapter: {desc}"[:60], "desc": desc,
                    "verdict": "OPEN" if len(path) > 1 else "UNRESOLVED"}
        cur = nxt
    return {"chain": chain, "feed": feed, "path": path,
            "label": (path[-1]["typeAndVersion"] if path else "") or "(unresolved)",
            "desc": desc, "verdict": "UNRESOLVED"}


def row(venue, chain, asset, w, note=""):
    return {"venue": venue, "chain": chain, "asset": asset,
            "aggregator": w["path"][-1]["addr"] if w["path"] else w["feed"],
            "typeAndVersion": w["label"], "desc": w["desc"],
            "verdict": w["verdict"], "hops": len(w["path"]), "note": note,
            "path": [p["addr"] for p in w["path"]]}


# --- family 1: Compound III (Comet) --------------------------------------
COMETS = {
    "Compound III cUSDCv3 (Base)":  ("base", "0xb125E6687d4313864e53df431d5425969c15Eb2F"),
    "Compound III cWETHv3 (Base)":  ("base", "0x46e6b214b524310239732D51387075E0e70970bf"),
    "Compound III cUSDCv3 (Eth)":   ("ethereum", "0xc3d688B66703497DAA19211EEdff47f25384cdc3"),
    "Compound III cWETHv3 (Eth)":   ("ethereum", "0xA17581A9E3356d9A858b789D68B4d866e593aE94"),
}


def probe_comets(max_assets: int = 4) -> list[dict]:
    out = []
    for name, (chain, comet) in COMETS.items():
        n_raw = call(chain, comet, SEL["numAssets()"])
        n = int(n_raw, 16) if n_raw and n_raw != "0x" else 0
        # base token feed: what absorb() prices the DEBT in
        bf = addr(call(chain, comet, SEL["baseTokenPriceFeed()"]))
        if bf:
            out.append(row(name, chain, "BASE(debt)", walk_feed(chain, bf), f"numAssets={n}"))
        for i in range(min(n, max_assets)):
            res = call(chain, comet, SEL["getAssetInfo(uint8)"] + f"{i:064x}")
            if not res or len(res) < 2 + 64 * 3:
                continue
            raw = res[2:]
            asset = "0x" + raw[64:128][-40:]
            pfeed = "0x" + raw[128:192][-40:]
            sym = decode_string(call(chain, asset, SEL["symbol()"])) or asset[:10]
            if int(pfeed, 16) == 0:
                continue
            out.append(row(name, chain, sym, walk_feed(chain, pfeed), f"collateral idx {i}"))
        print(f"  [comet] {name}: {len([r for r in out if r['venue']==name])} feeds "
              f"(rpc={_stats['calls']})")
    return out


# --- family 2: Moonwell (Base, Compound V2 fork) --------------------------
MOONWELL_COMPTROLLER = "0xfBb21d0380beE3312B33c4353c8936a0F13EF26C"
MOONWELL_MTOKENS = [
    ("mWETH",  "0x628ff693426583D9a7FB391E54366292F509D457"),
    ("mcbBTC", "0xF877ACaFA28c19b96727966690b2f44d35aD5976"),
    ("mUSDC",  "0xEdc817A28E8B93B03976FBd4a3dDBc9f7D176c22"),
]


def probe_moonwell() -> list[dict]:
    chain = "base"
    oracle = addr(call(chain, MOONWELL_COMPTROLLER, SEL["oracle()"]))
    if not oracle:
        print("  [moonwell] could not read comptroller.oracle() -- UNMEASURED")
        return []
    otv = decode_string(call(chain, oracle, SEL["typeAndVersion()"]))
    print(f"  [moonwell] ChainlinkOracle {oracle} tv={otv or '(none)'}")
    out = []
    for sym, mtoken in MOONWELL_MTOKENS:
        # Moonwell's ChainlinkOracle is keyed by the UNDERLYING's symbol string.
        und = addr(call(chain, mtoken, SEL["underlying()"]))
        usym = decode_string(call(chain, und, SEL["symbol()"])) if und else ""
        if not usym:
            continue
        enc = usym.encode()
        data = (SEL["getFeed(string)"] + f"{32:064x}" + f"{len(enc):064x}"
                + enc.hex().ljust(64, "0"))
        feed = addr(call(chain, oracle, data))
        if not feed:
            out.append({"venue": "Moonwell (Base)", "chain": chain, "asset": usym,
                        "aggregator": "-", "typeAndVersion": "UNMEASURED (getFeed reverted)",
                        "desc": "", "verdict": "UNMEASURED", "hops": 0,
                        "note": f"mToken {mtoken}", "path": []})
            continue
        out.append(row("Moonwell (Base)", chain, usym, walk_feed(chain, feed),
                       f"mToken {mtoken}"))
    return out


# --- family 3: Morpho Blue, top markets by realised liquidation value -----
MORPHO = "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb"


def probe_morpho(top_path: Path, verify_ids: int = 2) -> list[dict]:
    top = json.loads(top_path.read_text(encoding="utf-8"))
    out = []
    for chain, mkts in top.items():
        for k, m in enumerate(mkts):
            oracle = m["oracle"]
            if not oracle or int(oracle, 16) == 0:
                out.append({"venue": "Morpho Blue", "chain": chain, "asset": m["pair"],
                            "aggregator": "0x0 (hardcoded-1 oracle)",
                            "typeAndVersion": "no oracle contract",
                            "desc": "", "verdict": "OPEN", "hops": 0,
                            "note": f"${m['usd']:,.0f}/30d", "path": []})
                continue
            # cross-check the oracle against Morpho itself, and resolve any
            # collateral whose symbol() the log-scraper could not decode.
            if k < verify_ids or "None" in m["pair"]:
                res = call(chain, MORPHO,
                           SEL["idToMarketParams(bytes32)"] + m["mid"][2:])
                if res and len(res) >= 2 + 64 * 5:
                    raw = res[2:]
                    coll = "0x" + raw[64:128][-40:]
                    onchain = "0x" + raw[128:192][-40:]
                    if onchain.lower() != oracle.lower():
                        m["pair"] += " (!oracle mismatch)"
                    if "None" in m["pair"]:
                        cs = decode_string(call(chain, coll, SEL["symbol()"]))
                        m["pair"] = m["pair"].replace("None", cs or coll[:10])
            otv = decode_string(call(chain, oracle, SEL["typeAndVersion()"]))
            # MorphoChainlinkOracleV2 exposes up to 4 constituent feeds; BASE_FEED_1
            # is the one that prices the collateral, i.e. the liquidation trigger.
            bf = addr(call(chain, oracle, SEL["BASE_FEED_1()"]))
            if not bf:
                out.append({"venue": "Morpho Blue", "chain": chain, "asset": m["pair"],
                            "aggregator": oracle,
                            "typeAndVersion": otv or "(custom oracle, no BASE_FEED_1)",
                            "desc": "", "verdict": "OPEN", "hops": 1,
                            "note": f"${m['usd']:,.0f}/30d, n={m['n']}", "path": [oracle]})
                continue
            w = walk_feed(chain, bf)
            r = row("Morpho Blue", chain, m["pair"], w,
                    f"${m['usd']:,.0f}/30d, n={m['n']}, mkt-oracle {oracle}")
            out.append(r)
        print(f"  [morpho] {chain}: {len(mkts)} markets probed (rpc={_stats['calls']})")
    return out


# --- family 4: further Aave V3 forks / instances --------------------------
EXTRA_AAVE = {
    "Aave V3 Lido instance (Eth)":   ("ethereum", "0x4e033931ad43597d96D6bcc25c280717730B58B1", [
        ("WETH", "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        ("wstETH", "0x7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0"),
    ]),
    "Aave V3 EtherFi instance (Eth)": ("ethereum", "0x0AA97c284e98396202b6A04024F5E2c65026F3c0", [
        ("weETH", "0xCd5fE23C85820F7B72D0926FC9b05b43E359b7ee"),
        ("USDC", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
    ]),
    "ZeroLend Main (Eth)":           ("ethereum", "0x3BC3D34C32cc98bf098D832364Df8A222bBaB4c0", [
        ("WETH", "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        ("WBTC", "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
    ]),
}


def probe_aave_like(table: dict) -> list[dict]:
    out = []
    for name, (chain, pool, assets) in table.items():
        if not assets or int(pool, 16) == 0:
            continue
        prov = addr(call(chain, pool, SEL["ADDRESSES_PROVIDER()"]))
        oracle = addr(call(chain, prov, SEL["getPriceOracle()"])) if prov else None
        if not oracle:
            out.append({"venue": name, "chain": chain, "asset": "-", "aggregator": "-",
                        "typeAndVersion": "UNMEASURED (pool/oracle unreachable)",
                        "desc": "", "verdict": "UNMEASURED", "hops": 0,
                        "note": f"pool {pool}", "path": []})
            print(f"  [aave-fork] {name}: UNREACHABLE (rpc={_stats['calls']})")
            continue
        for sym, asset in assets:
            src = addr(call(chain, oracle,
                            SEL["getSourceOfAsset(address)"] + asset[2:].rjust(64, "0")))
            if not src:
                continue
            out.append(row(name, chain, sym, walk_feed(chain, src), f"oracle {oracle}"))
        print(f"  [aave-fork] {name}: oracle {oracle} (rpc={_stats['calls']})")
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--chain", default=None, help="only check venues on this chain")
    ap.add_argument("--ext", action="store_true",
                    help="run the extended sweep (Comet / Moonwell / Morpho / forks)")
    ap.add_argument("--morpho-top", default=None, help="path to morpho_top8.json")
    ap.add_argument("--json", default=None, help="write rows to this JSON path")
    args = ap.parse_args()

    if args.ext:
        rows: list[dict] = []
        outp = Path(args.json) if args.json else None

        def flush():
            if outp:
                outp.write_text(json.dumps(
                    {"rpc_calls": _stats["calls"], "cache_hits": _stats["cached"],
                     "budget_capped": _stats["capped"], "rows": rows}, indent=1),
                    encoding="utf-8")

        # controls first -- reproduce the known Aave/Seamless/Spark verdicts
        print("[controls] Aave/Seamless/Spark via getSourceOfAsset")
        rows += probe_aave_like(
            {k: v for k, v in VENUES.items()}); flush()
        print("[1/4] Compound III Comet"); rows += probe_comets(); flush()
        print("[2/4] Moonwell (Base)");    rows += probe_moonwell(); flush()
        if args.morpho_top:
            print("[3/4] Morpho Blue top markets")
            rows += probe_morpho(Path(args.morpho_top)); flush()
        print("[4/4] other Aave V3 forks/instances")
        rows += probe_aave_like(EXTRA_AAVE); flush()

        print(f"\nrpc_calls={_stats['calls']} cache_hits={_stats['cached']} "
              f"capped={_stats['capped']}")
        print(f"{'venue':<32}{'chain':<9}{'asset':<24}{'typeAndVersion':<46}verdict")
        for r in rows:
            print(f"{r['venue'][:31]:<32}{r['chain']:<9}{r['asset'][:23]:<24}"
                  f"{r['typeAndVersion'][:45]:<46}{r['verdict']}")
        return 0


    print("Venue openness gate — Chainlink SVR detection\n" + "=" * 78)
    any_closed = False
    for name, (chain, pool, assets) in VENUES.items():
        if args.chain and chain != args.chain:
            continue
        print(f"\n{name}  [{chain}]  pool {pool}")
        prov = addr(call(chain, pool, SEL["ADDRESSES_PROVIDER()"]))
        if not prov:
            print("   ! could not read ADDRESSES_PROVIDER — skipping")
            continue
        oracle = addr(call(chain, prov, SEL["getPriceOracle()"]))
        if not oracle:
            print("   ! could not read getPriceOracle — skipping")
            continue
        print(f"   oracle {oracle}")
        for sym, asset in assets:
            src = addr(call(chain, oracle,
                            SEL["getSourceOfAsset(address)"] + asset[2:].rjust(64, "0")))
            if not src:
                print(f"   {sym:<7} source: <none>")
                continue
            tv = decode_string(call(chain, src, SEL["typeAndVersion()"]))
            desc = decode_string(call(chain, src, SEL["description()"]))
            # one hop deeper: a proxy's underlying aggregator often names itself
            inner = addr(call(chain, src, SEL["aggregator()"]))
            tv_inner = decode_string(call(chain, inner, SEL["typeAndVersion()"])) if inner else ""
            blob = f"{tv} {tv_inner}".lower()
            closed = "dual" in blob or "svr" in blob
            any_closed |= closed
            verdict = "CLOSED (SVR)" if closed else "open"
            label = tv or tv_inner or "(no typeAndVersion)"
            print(f"   {sym:<7} src {src}  {label[:44]:<44} {desc[:18]:<18} -> {verdict}")
    print("\n" + "=" * 78)
    print("Morpho Blue is not an SVR protocol (SVR covers Aave/Compound/Venus),")
    print("so Morpho markets are structurally open on every chain.")
    if any_closed:
        print("\nAt least one venue is SVR-CLOSED: a public-RPC bot cannot see its")
        print("trigger price in time. Do not spend engineering there.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
