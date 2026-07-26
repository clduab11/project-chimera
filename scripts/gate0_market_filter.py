#!/usr/bin/env python3
"""
gate0_market_filter.py
Project Chimera - Gate-0: three cheap checks per lending market, designed to run
across hundreds of markets at <=5 network calls each.

The three checks (in the order they kill things):

  (ii) ORACLE PREMIUM   |oraclePrice / tradedPrice - 1| > 1%  -> FAIL.
       This is the discriminator. Morpho's seizedAssets is denominated at the
       market oracle price (verified in Morpho.sol::liquidate), so an oracle that
       sits above market converts the whole liquidation bonus into phantom profit.
       AVLT: LIF +4.38% vs premium +4-6% -> negative round trip.

  (i)  ROUTABLE EXIT    measured with AGGREGATOR quotes, not factory enumeration.
       An aggregator will not route through a 99%-fee Uniswap V4 pool; a factory
       scan cannot tell the difference (AVLT has 9 V4 pools, fees 90-99.99%).
       Quote collateral -> debtAsset at three sizes (median clip, 3x median, p90)
       ON THE SAME CHAIN as the debt asset. FAIL if <3x median clip routable at
       <2% price impact. All aggregators refusing to quote = NO_ROUTE (a fail,
       not an error).

  (iii) EXIT TYPE       not all collateral exits via a DEX:
       DEX              normal token; judged on (i)+(ii)
       PT_REDEEMABLE    Pendle Principal Token; converges to par at maturity and
                        is redeemable -> thin DEX depth is NOT disqualifying.
                        Judged on discount-vs-time-to-maturity => REVIEW_PT.
       VAULT_REDEEMABLE ERC-4626-like (asset() answers) -> REVIEW_VAULT.
       BRIDGE_ONLY      only real venue on another chain (AVLT) -> FAIL.
       NONE             no exit found -> FAIL.

Deliberate non-checks, learned the hard way:
  - No roundId-never-increments filter: roundId=1 is structural for RedStone
    PriceFeedWithoutRoundsForMultiFeedAdapter; the AVLT feed is live (146 updates
    in 12d), only its VALUE is pinned.
  - Transfer-restriction probing is not the gate; AVLT is a plain OFT with no
    allowlist/pause/hook and it still fails Gate-0 on premium + exit.

Stdlib only. Keys read from .env.live by regex, never printed. Checkpoints to a
.partial JSONL next to --out so a mid-run network death loses nothing.

Usage:
  python scripts/gate0_market_filter.py --from-venue-report <venue_report.json> --out gate0.json
  python scripts/gate0_market_filter.py --markets <rows.json> --out gate0.json
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

SEL_DECIMALS = "0x313ce567"
SEL_SYMBOL = "0x95d89b41"
SEL_PRICE = "0xa035b1fe"                # Morpho IOracle.price()
SEL_ASSET = "0x38d52e0f"                # ERC-4626 asset()
SEL_CONVERT_TO_ASSETS = "0x07a2d13a"    # ERC-4626 convertToAssets(uint256)
SEL_GET_ASSET_PRICE = "0xb3596f07"      # AaveOracle.getAssetPrice(address)
SEL_ID_TO_MARKET_PARAMS = "0x2c3c9157"

LLAMA_CHAIN = {
    "ethereum": "ethereum", "base": "base", "arbitrum": "arbitrum",
    "optimism": "optimism", "polygon": "polygon", "avalanche": "avax",
    "bsc": "bsc", "linea": "linea", "scroll": "scroll", "gnosis": "xdai",
    "celo": "celo", "metis": "metis", "sonic": "sonic", "unichain": "unichain",
    "katana": "katana", "zksync": "era",
}
KYBER_CHAIN = {
    "ethereum": "ethereum", "base": "base", "arbitrum": "arbitrum",
    "optimism": "optimism", "polygon": "polygon", "avalanche": "avalanche",
    "bsc": "bsc", "linea": "linea", "scroll": "scroll", "sonic": "sonic",
    "unichain": "unichain",
}
PARASWAP_NET = {
    "ethereum": 1, "base": 8453, "arbitrum": 42161, "optimism": 10,
    "polygon": 137, "avalanche": 43114, "bsc": 56, "gnosis": 100,
}

# Tokens whose only real venue is on another chain behind a non-atomic bridge.
# Annotation for exit_type when the mechanical route check fails anyway.
KNOWN_BRIDGE_ONLY = {
    "0x74db7a52773a52699dbc0c01b1254e5301e3e119": "AVLT: real venue HyperEVM, LayerZero OFT",
}

PREMIUM_FAIL_PCT = 1.0
IMPACT_FAIL_PCT = 2.0


def build_rpc_map() -> dict[str, str]:
    raw = (REPO / ".env.live").read_text(encoding="utf-8")
    ak = re.findall(r"alchemy\.com/v2/([A-Za-z0-9_-]{20,})", raw)
    ik = re.findall(r"infura\.io/(?:v3|ws/v3)/([A-Za-z0-9]{20,})", raw)
    m: dict[str, str] = {}
    if ak:
        m["base"] = f"https://base-mainnet.g.alchemy.com/v2/{ak[0]}"
        m["ethereum"] = f"https://eth-mainnet.g.alchemy.com/v2/{ak[0]}"
    if ik:
        i = ik[0]
        m.setdefault("ethereum", f"https://mainnet.infura.io/v3/{i}")
        m.setdefault("base", f"https://base-mainnet.infura.io/v3/{i}")
        m["arbitrum"] = f"https://arbitrum-mainnet.infura.io/v3/{i}"
        m["optimism"] = f"https://optimism-mainnet.infura.io/v3/{i}"
        m["polygon"] = f"https://polygon-mainnet.infura.io/v3/{i}"
        m["avalanche"] = f"https://avalanche-mainnet.infura.io/v3/{i}"
        m["bsc"] = f"https://bsc-mainnet.infura.io/v3/{i}"
        m["linea"] = f"https://linea-mainnet.infura.io/v3/{i}"
    if not m:
        raise SystemExit("no provider key found in .env.live")
    return m


RPC = build_rpc_map()
_last = {"rpc": 0.0, "agg": 0.0, "llama": 0.0}
STATS = {"rpc": 0, "agg": 0, "llama": 0}


def _gap(kind: str, gap_s: float):
    dt = time.time() - _last[kind]
    if dt < gap_s:
        time.sleep(gap_s - dt)
    _last[kind] = time.time()


def rpc(chain: str, method: str, params: list, retries: int = 5):
    if chain not in RPC:
        raise ConnectionError(f"no RPC endpoint for chain {chain}")
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method,
                       "params": params}).encode()
    last = None
    for attempt in range(retries):
        _gap("rpc", 0.1)
        STATS["rpc"] += 1
        try:
            req = urllib.request.Request(
                RPC[chain], data=body, headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=45) as r:
                out = json.loads(r.read())
            if "error" in out:
                raise ValueError(str(out["error"])[:150])
            return out["result"]
        except ValueError:
            raise
        except Exception as exc:
            last = exc
            time.sleep(min(2 * (2 ** attempt), 30))
    raise ConnectionError(f"{chain}.{method}: {last}")


def http_json(url: str, data: bytes | None = None, kind: str = "agg",
              gap_s: float = 0.7, retries: int = 3, timeout: int = 30):
    last = None
    for attempt in range(retries):
        _gap(kind, gap_s)
        STATS[kind] += 1
        try:
            req = urllib.request.Request(
                url, data=data,
                headers={"Content-Type": "application/json", "User-Agent": "chimera-gate0"})
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            last = e
            if e.code in (400, 404, 422):     # no-route style answers, not transient
                try:
                    return json.loads(e.read())
                except Exception:
                    return None
            time.sleep(min(3 * (2 ** attempt), 20))
        except Exception as e:
            last = e
            time.sleep(min(2 * (2 ** attempt), 15))
    return None


_meta_cache: dict = {}


def token_meta(chain: str, addr: str) -> dict:
    key = (chain, addr)
    if key in _meta_cache:
        return _meta_cache[key]
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
    _meta_cache[key] = {"decimals": dec, "symbol": sym}
    return _meta_cache[key]


_px_cache: dict = {}


def llama_price(chain: str, addr: str) -> float | None:
    key = (chain, addr)
    if key in _px_cache:
        return _px_cache[key]
    out = None
    ck = LLAMA_CHAIN.get(chain)
    if ck:
        _gap("llama", 0.35)
        STATS["llama"] += 1
        try:
            with urllib.request.urlopen(
                    f"https://coins.llama.fi/prices/current/{ck}:{addr}", timeout=30) as r:
                data = json.loads(r.read()).get("coins", {})
            for v in data.values():
                out = v.get("price")
        except Exception:
            out = None
    _px_cache[key] = out
    return out


# ---------------------------------------------------------------------------
# aggregator quotes: KyberSwap first (keyless, widest chain coverage), ParaSwap
# fallback. Returns (amount_out_raw, source) or (None, reason).
# ---------------------------------------------------------------------------

def kyber_quote(chain: str, tin: str, tout: str, amount_in: int):
    slug = KYBER_CHAIN.get(chain)
    if not slug:
        return None, "kyber:unsupported-chain"
    url = (f"https://aggregator-api.kyberswap.com/{slug}/api/v1/routes"
           f"?tokenIn={tin}&tokenOut={tout}&amountIn={amount_in}")
    o = http_json(url)
    if not o:
        return None, "kyber:unreachable"
    if o.get("code") != 0:
        return None, f"kyber:no-route({str(o.get('message'))[:40]})"
    rs = (o.get("data") or {}).get("routeSummary") or {}
    try:
        return int(rs["amountOut"]), "kyber"
    except Exception:
        return None, "kyber:bad-response"


def paraswap_quote(chain: str, tin: str, tout: str, amount_in: int,
                   din: int, dout: int):
    net = PARASWAP_NET.get(chain)
    if not net:
        return None, "paraswap:unsupported-chain"
    url = (f"https://api.paraswap.io/prices?srcToken={tin}&destToken={tout}"
           f"&amount={amount_in}&srcDecimals={din}&destDecimals={dout}"
           f"&side=SELL&network={net}")
    o = http_json(url)
    if not o:
        return None, "paraswap:unreachable"
    pr = o.get("priceRoute")
    if not pr:
        return None, f"paraswap:no-route({str(o.get('error'))[:40]})"
    try:
        return int(pr["destAmount"]), "paraswap"
    except Exception:
        return None, "paraswap:bad-response"


def quote(chain: str, tin: str, tout: str, amount_in: int, din: int, dout: int):
    out, src = kyber_quote(chain, tin, tout, amount_in)
    if out is not None:
        return out, src
    out2, src2 = paraswap_quote(chain, tin, tout, amount_in, din, dout)
    if out2 is not None:
        return out2, src2
    return None, f"{src}|{src2}"


# ---------------------------------------------------------------------------
# per-market gate
# ---------------------------------------------------------------------------

PT_RE = re.compile(r"^PT[- ]", re.IGNORECASE)
PT_DATE_RE = re.compile(r"(\d{1,2})([A-Z]{3})(\d{4})", re.IGNORECASE)
MONTHS = {m: i + 1 for i, m in enumerate(
    ["JAN", "FEB", "MAR", "APR", "MAY", "JUN",
     "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"])}


def pt_maturity_days(symbol: str) -> float | None:
    m = PT_DATE_RE.search(symbol.upper())
    if not m:
        return None
    try:
        d = datetime(int(m.group(3)), MONTHS[m.group(2)], int(m.group(1)),
                     tzinfo=timezone.utc)
        return (d - datetime.now(timezone.utc)).total_seconds() / 86400
    except Exception:
        return None


_aave_oracle_cache: dict = {}


def _aave_oracle(chain: str, pool: str) -> str:
    """Pool -> ADDRESSES_PROVIDER() -> getPriceOracle(), cached per pool."""
    key = (chain, pool)
    if key not in _aave_oracle_cache:
        prov = "0x" + rpc(chain, "eth_call",
                          [{"to": pool, "data": "0x0542975c"}, "latest"])[-40:]
        _aave_oracle_cache[key] = "0x" + rpc(
            chain, "eth_call", [{"to": prov, "data": "0xfca513a8"}, "latest"])[-40:]
    return _aave_oracle_cache[key]


def is_erc4626(chain: str, token: str) -> str | None:
    """Return the underlying asset address if token answers asset(), else None."""
    try:
        r = rpc(chain, "eth_call", [{"to": token, "data": SEL_ASSET}, "latest"])
        if r and len(r) >= 66:
            a = "0x" + r[-40:]
            if int(a, 16) != 0:
                return a
    except Exception:
        pass
    return None


def gate_market(row: dict) -> dict:
    """row: chain, protocol, venue, market_id?, collateral, debt, lif?, lltv?,
    oracle? (morpho), pool? (aave), open_or_gated?, bonus_oracle_usd?,
    median_clip_usd?, p90_clip_usd?, seized_usd?, n?"""
    chain = row["chain"]
    coll, debt = row["collateral"].lower(), row["debt"].lower()
    out = {
        "chain": chain, "protocol": row.get("protocol"), "venue": row.get("venue"),
        "market_id": row.get("market_id"), "collateral": coll, "debt": debt,
        "open_or_gated": row.get("open_or_gated", "OPEN"),
        "lif": row.get("lif"), "lltv": row.get("lltv"),
        "bonus_oracle": row.get("bonus_oracle_usd"),
        "bonus_exit": None, "oracle_premium_pct": None,
        "exit_type": None, "routable_at_2pct": None,
        "gate0_pass": None, "notes": [], "calls_used": 0,
    }
    calls0 = STATS["rpc"] + STATS["agg"]

    try:
        mc = token_meta(chain, coll)
        md = token_meta(chain, debt)
        out["pair"] = f"{mc['symbol']}/{md['symbol']}"

        # resolve morpho params if the row only carries a market id
        if row.get("protocol") == "morpho" and not row.get("oracle") and row.get("market_id"):
            r = rpc(chain, "eth_call", [{
                "to": "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb",
                "data": SEL_ID_TO_MARKET_PARAMS + row["market_id"][2:]}, "latest"])
            row["oracle"] = "0x" + r[2 + 64 * 2:2 + 64 * 3][-40:]
            lltv = int(r[2 + 64 * 4:2 + 64 * 5], 16) / 1e18
            row["lltv"] = lltv
            out["lltv"] = lltv
        if out["lif"] is None and out["lltv"]:
            out["lif"] = min(1.15, 1.0 / (1.0 - 0.3 * (1.0 - out["lltv"])))

        # ---- sizes ---------------------------------------------------------
        med = row.get("median_clip_usd") or 5_000.0
        p90 = row.get("p90_clip_usd") or max(10 * med, 50_000.0)
        med = max(med, 100.0)          # a $2 dust clip quotes as noise
        sizes_usd = [med, 3 * med, max(p90, 3 * med)]

        ref_px = llama_price(chain, coll)
        debt_px = llama_price(chain, debt)

        # collateral units for each USD size; if llama has no price, fall back to
        # the oracle price to size the probe (the quote itself gives traded price)
        oracle_usd = None
        if row.get("protocol") == "morpho" and row.get("oracle") and int(row["oracle"], 16) != 0:
            try:
                pr = rpc(chain, "eth_call", [{"to": row["oracle"], "data": SEL_PRICE}, "latest"])
                px_loan = int(pr, 16) / 10 ** (36 + md["decimals"] - mc["decimals"])
                oracle_usd = px_loan * (debt_px if debt_px else 1.0)
            except Exception:
                out["notes"].append("oracle price() unreadable")
        elif row.get("protocol") == "morpho":
            oracle_usd = 1.0 * (debt_px if debt_px else 1.0)   # hardcoded-1 markets
            out["notes"].append("no-oracle (hardcoded 1.0)")
        elif row.get("pool"):
            try:
                orc = _aave_oracle(chain, row["pool"])
                pr = rpc(chain, "eth_call", [{"to": orc,
                                              "data": SEL_GET_ASSET_PRICE + coll[2:].rjust(64, "0")},
                                             "latest"])
                oracle_usd = int(pr, 16) / 1e8
            except Exception:
                out["notes"].append("venue oracle unreadable")

        size_px = ref_px or oracle_usd
        if not size_px:
            out["exit_type"] = "NONE"
            out["gate0_pass"] = "FAIL"
            out["notes"].append("no price anywhere to even size a probe")
            return out

        # ---- check (i): aggregator quotes at three sizes -------------------
        unit_px: list[float | None] = []
        srcs = set()
        for usd in sizes_usd:
            units = usd / size_px
            amt = int(units * 10 ** mc["decimals"])
            if amt <= 0:
                unit_px.append(None)
                continue
            got, src = quote(chain, coll, debt, amt, mc["decimals"], md["decimals"])
            srcs.add(src)
            if got is None or got == 0:
                unit_px.append(None)
            else:
                unit_px.append((got / 10 ** md["decimals"]) / units)  # debt per coll
        out["quote_sources"] = sorted(srcs)

        valid = [p for p in unit_px if p]
        if not valid:
            in_known = coll in KNOWN_BRIDGE_ONLY
            out["exit_type"] = "BRIDGE_ONLY" if in_known else "NONE"
            if in_known:
                out["notes"].append(KNOWN_BRIDGE_ONLY[coll])
            out["notes"].append(f"NO_ROUTE on {chain} ({'|'.join(sorted(srcs))})")
            out["routable_at_2pct"] = False
        else:
            best = max(valid)
            traded_usd = best * (debt_px if debt_px else 1.0)
            impact = [None if p is None else (1 - p / best) * 100 for p in unit_px]
            out["impact_pct_at_sizes"] = [
                None if i is None else round(i, 3) for i in impact]
            out["sizes_usd"] = [round(s) for s in sizes_usd]
            ok3x = unit_px[1] is not None and impact[1] is not None and impact[1] < IMPACT_FAIL_PCT
            out["routable_at_2pct"] = bool(ok3x)
            # Junk-route guard: an aggregator can "route" through a 99%-fee pool
            # and return uniformly terrible prices with low RELATIVE impact. If
            # the quote-implied price diverges >30% from DefiLlama's traded
            # price, the route is not a real exit: score it unroutable and use
            # DefiLlama as the traded price for the premium check.
            if ref_px and abs(traded_usd / ref_px - 1) > 0.30:
                out["notes"].append(
                    f"quote-implied ${traded_usd:,.4f} diverges "
                    f"{100*(traded_usd/ref_px-1):+.0f}% from llama ${ref_px:,.4f}"
                    " -> route treated as fake, llama used as traded price")
                out["routable_at_2pct"] = False
                traded_usd = ref_px
                if coll in KNOWN_BRIDGE_ONLY:
                    out["exit_type"] = "BRIDGE_ONLY"
                    out["notes"].append(KNOWN_BRIDGE_ONLY[coll])
            out["traded_price_usd"] = traded_usd

            # ---- check (ii): oracle premium against the traded price -------
            if oracle_usd and traded_usd:
                out["oracle_premium_pct"] = round((oracle_usd / traded_usd - 1) * 100, 3)

            # ---- bonus_exit at this premium --------------------------------
            if out["lif"] and out["oracle_premium_pct"] is not None and row.get("repaid_usd"):
                prem = out["oracle_premium_pct"] / 100
                out["bonus_exit"] = row["repaid_usd"] * (out["lif"] / (1 + prem) - 1)

        # ---- check (iii): exit classification ------------------------------
        sym = mc["symbol"] or ""
        if out["exit_type"] is None:
            if PT_RE.match(sym):
                out["exit_type"] = "PT_REDEEMABLE"
                out["pt_days_to_maturity"] = pt_maturity_days(sym)
            else:
                under = is_erc4626(chain, coll)
                if out["routable_at_2pct"]:
                    out["exit_type"] = "DEX"
                    if under:
                        out["notes"].append(f"also ERC-4626 (asset {under})")
                elif under:
                    out["exit_type"] = "VAULT_REDEEMABLE"
                    out["vault_asset"] = under
                else:
                    out["exit_type"] = "DEX"   # routable but thin
        elif PT_RE.match(sym):
            out["exit_type"] = "PT_REDEEMABLE"
            out["pt_days_to_maturity"] = pt_maturity_days(sym)

        # ---- verdict -------------------------------------------------------
        et = out["exit_type"]
        prem = out["oracle_premium_pct"]
        if et in ("BRIDGE_ONLY", "NONE"):
            out["gate0_pass"] = "FAIL"
        elif et == "PT_REDEEMABLE":
            out["gate0_pass"] = "REVIEW_PT"
            out["notes"].append("time-based: judge discount vs maturity, not DEX depth")
        elif et == "VAULT_REDEEMABLE":
            out["gate0_pass"] = "REVIEW_VAULT"
        else:  # DEX
            if prem is not None and abs(prem) > PREMIUM_FAIL_PCT:
                out["gate0_pass"] = "FAIL"
                out["notes"].append(f"oracle premium {prem:+.2f}% > {PREMIUM_FAIL_PCT}%")
            elif not out["routable_at_2pct"]:
                out["gate0_pass"] = "FAIL"
                out["notes"].append("not routable at 3x median clip under 2% impact")
            elif prem is None:
                out["gate0_pass"] = "REVIEW"
                out["notes"].append("premium unmeasurable (no oracle read)")
            else:
                out["gate0_pass"] = "PASS"
    except Exception as e:
        out["gate0_pass"] = "ERROR"
        out["notes"].append(f"error: {str(e)[:120]}")
    out["calls_used"] = STATS["rpc"] + STATS["agg"] - calls0
    return out


# ---------------------------------------------------------------------------

def rows_from_venue_report(path: Path, min_bonus: float) -> list[dict]:
    rep = json.loads(path.read_text(encoding="utf-8"))
    rows = []
    for v in rep:
        if "error" in v:
            continue
        proto = "morpho" if "Morpho" in v["label"] else "aave"
        for m in v.get("markets", []):
            if m["bonus_oracle_usd"] < min_bonus:
                continue
            rows.append({
                "chain": v["chain"], "protocol": proto, "venue": v["venue"],
                "market_id": m.get("market_id"), "collateral": m["collateral"],
                "debt": m["debt"], "lif": m.get("lif"), "lltv": m.get("lltv"),
                "oracle": m.get("oracle"),
                "pool": v["address"] if proto == "aave" else None,
                "open_or_gated": "OPEN" if proto == "morpho" else row_gate(v),
                "bonus_oracle_usd": m["bonus_oracle_usd"],
                "repaid_usd": m["repaid_usd"],
                "median_clip_usd": m.get("median_clip_usd"),
                "p90_clip_usd": m.get("p90_clip_usd"),
                "n": m["n"], "seized_usd": m["seized_usd"],
            })
    return rows


def row_gate(v: dict) -> str:
    # Aave on Base/Ethereum/Arbitrum/BNB is SVR-gated [MEASURED 2026-07-25];
    # other chains must be probed with check_venue_open.py and passed in.
    return "GATED" if (v["chain"], v["label"]) in (
        ("base", "Aave V3"), ("ethereum", "Aave V3")) else "UNKNOWN"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--markets", help="JSON list of market rows")
    ap.add_argument("--from-venue-report", help="venue_report.json from the scanner")
    ap.add_argument("--min-bonus", type=float, default=50.0,
                    help="skip markets under this 30d bonus_oracle (still listed as skipped)")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    rows: list[dict] = []
    if args.markets:
        rows += json.loads(Path(args.markets).read_text(encoding="utf-8"))
    if args.from_venue_report:
        rows += rows_from_venue_report(Path(args.from_venue_report), args.min_bonus)
    if not rows:
        raise SystemExit("no market rows given")

    outp = Path(args.out)
    part = outp.with_suffix(".partial.jsonl")
    done: dict[str, dict] = {}
    if part.exists():
        for line in part.read_text(encoding="utf-8").splitlines():
            if line.strip():
                r = json.loads(line)
                done[r["_key"]] = r
        print(f"resuming: {len(done)} markets already gated")

    results = []
    with part.open("a", encoding="utf-8") as fh:
        for i, row in enumerate(rows):
            key = f"{row['chain']}:{row.get('market_id') or row['collateral']+'/'+row['debt']}"
            if key in done:
                results.append(done[key])
                continue
            r = gate_market(row)
            r["_key"] = key
            results.append(r)
            fh.write(json.dumps(r) + "\n")
            fh.flush()
            print(f"[{i+1}/{len(rows)}] {r.get('pair','?'):<28} {r['chain']:<10}"
                  f" {str(r['gate0_pass']):<12} premium={r['oracle_premium_pct']}"
                  f" exit={r['exit_type']} calls={r['calls_used']}", flush=True)

    outp.write_text(json.dumps({
        "generated": datetime.now(timezone.utc).isoformat(),
        "thresholds": {"premium_fail_pct": PREMIUM_FAIL_PCT,
                       "impact_fail_pct": IMPACT_FAIL_PCT},
        "stats": STATS, "rows": results}, indent=1), encoding="utf-8")
    n = len(results)
    for verdict in ("PASS", "FAIL", "REVIEW_PT", "REVIEW_VAULT", "REVIEW", "ERROR"):
        k = sum(1 for r in results if r["gate0_pass"] == verdict)
        if k:
            print(f"  {verdict:<14}{k:>5} / {n}")
    print(f"network calls: rpc={STATS['rpc']} agg={STATS['agg']} llama={STATS['llama']}")
    print(f"wrote {outp}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
