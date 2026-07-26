#!/usr/bin/env python3
"""
enumerate_morpho_markets.py
Project Chimera - enumerate EVERY Morpho Blue market on a chain (CreateMarket
logs), read live state through Multicall3, and emit Gate-0 input rows.

Answers the standing question "are the four known accrual markets the whole
niche or a sample?" by covering markets that have NOT liquidated in the last
30 days but carry live borrow that will accrue into liquidations.

Stdlib only; keys from .env.live by regex, never printed; checkpoints to disk.

Usage:
  python scripts/enumerate_morpho_markets.py --chain ethereum --workdir <dir> \
      [--min-borrow-usd 10000] [--venue-report <venue_report.json>]
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

MORPHO = "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb"
MULTICALL3 = "0xcA11bde05977b3631167028862bE2a173976CA11"
TOPIC_CREATE_MARKET = "0xac4b2400f169220b0c0afdde7a0b32e775ba727ea1cb30b35f935cdaab8683ac"
SEL_AGGREGATE3 = "0x82ad56cb"
SEL_MARKET = "0x5c60e39a"           # market(bytes32) -> (supplyA,supplyS,borrowA,borrowS,lastUpdate,fee)
SEL_DECIMALS = "0x313ce567"
SEL_SYMBOL = "0x95d89b41"

LLAMA_CHAIN = {"ethereum": "ethereum", "base": "base", "arbitrum": "arbitrum",
               "optimism": "optimism", "polygon": "polygon"}
# Morpho Blue deployment blocks (block of first possible CreateMarket)
DEPLOY_BLOCK = {"ethereum": 18_883_124, "base": 13_977_148}


def build_rpc(chain: str) -> str:
    raw = (REPO / ".env.live").read_text(encoding="utf-8")
    ak = re.findall(r"alchemy\.com/v2/([A-Za-z0-9_-]{20,})", raw)
    ik = re.findall(r"infura\.io/(?:v3|ws/v3)/([A-Za-z0-9]{20,})", raw)
    if chain in ("ethereum", "base") and ak:
        sub = "eth-mainnet" if chain == "ethereum" else "base-mainnet"
        return f"https://{sub}.g.alchemy.com/v2/{ak[0]}"
    if ik:
        sub = {"ethereum": "mainnet", "arbitrum": "arbitrum-mainnet",
               "optimism": "optimism-mainnet", "polygon": "polygon-mainnet"}.get(chain)
        if sub:
            return f"https://{sub}.infura.io/v3/{ik[0]}"
    raise SystemExit(f"no RPC for {chain}")


_last = [0.0]


def rpc(url: str, method: str, params: list, retries: int = 6):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method,
                       "params": params}).encode()
    last = None
    for attempt in range(retries):
        dt = time.time() - _last[0]
        if dt < 0.08:
            time.sleep(0.08 - dt)
        _last[0] = time.time()
        try:
            req = urllib.request.Request(url, data=body,
                                         headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=90) as r:
                out = json.loads(r.read())
            if "error" in out:
                raise ValueError(str(out["error"])[:200])
            return out["result"]
        except ValueError:
            raise
        except Exception as e:
            last = e
            time.sleep(min(2 * 2 ** attempt, 40))
    raise ConnectionError(f"{method}: {last}")


def fetch_create_markets(url: str, frm: int, to: int, ckpt: Path) -> list:
    if ckpt.exists():
        lines = [json.loads(x) for x in ckpt.read_text(encoding="utf-8").splitlines() if x]
        if lines and lines[-1].get("_done"):
            return [x for x in lines if not x.get("_done")]
        done_to = max((x["_to"] for x in lines if "_to" in x), default=frm - 1)
        logs = [x for x in lines if "_to" not in x and not x.get("_done")]
        frm = done_to + 1
    else:
        logs = []
    span = 500_000
    cur = frm
    with ckpt.open("a", encoding="utf-8") as fh:
        while cur <= to:
            end = min(cur + span - 1, to)
            try:
                batch = rpc(url, "eth_getLogs", [{
                    "address": MORPHO, "topics": [TOPIC_CREATE_MARKET],
                    "fromBlock": hex(cur), "toBlock": hex(end)}])
            except ValueError as e:
                m = re.search(r"limit of (\d+)|(\d+) block", str(e))
                span = max(10_000, span // 4)
                print(f"  span -> {span:,} ({str(e)[:60]})", flush=True)
                continue
            for lg in batch:
                fh.write(json.dumps(lg) + "\n")
            fh.write(json.dumps({"_to": end}) + "\n")
            fh.flush()
            logs.extend(batch)
            cur = end + 1
        fh.write(json.dumps({"_done": True}) + "\n")
    return logs


def multicall(url: str, calls: list[tuple[str, str]], batch: int = 150) -> list[str | None]:
    """calls: [(to, data)] -> list of raw return data (None on failure)."""
    out: list[str | None] = []
    for i in range(0, len(calls), batch):
        chunk = calls[i:i + batch]
        # encode aggregate3((address,bool,bytes)[])
        head = "0x" + SEL_AGGREGATE3[2:] + f"{32:064x}" + f"{len(chunk):064x}"
        offs, bodies = "", ""
        # tuple offsets are relative to the start of the array elements region
        cursor = 32 * len(chunk)
        enc = []
        for to, data in chunk:
            d = data[2:]
            dlen = len(d) // 2
            padded = d.ljust(((dlen + 31) // 32) * 32 * 2, "0")
            tup = (to[2:].rjust(64, "0") + f"{1:064x}" + f"{0x60:064x}"
                   + f"{dlen:064x}" + padded)
            enc.append(tup)
        for tup in enc:
            offs += f"{cursor:064x}"
            cursor += len(tup) // 2
        payload = head + offs + "".join(enc)
        res = rpc(url, "eth_call", [{"to": MULTICALL3, "data": payload}, "latest"])
        raw = res[2:]
        # decode (bool,bytes)[]
        arr_off = int(raw[0:64], 16) * 2
        n = int(raw[arr_off:arr_off + 64], 16)
        base = arr_off + 64
        for k in range(n):
            toff = int(raw[base + 64 * k: base + 64 * (k + 1)], 16) * 2
            tbase = base + toff
            ok = int(raw[tbase:tbase + 64], 16) == 1
            boff = int(raw[tbase + 64:tbase + 128], 16) * 2
            blen = int(raw[tbase + boff:tbase + boff + 64], 16)
            data = "0x" + raw[tbase + boff + 64: tbase + boff + 64 + blen * 2]
            out.append(data if ok and blen else None)
        print(f"  multicall {i + len(chunk)}/{len(calls)}", flush=True)
    return out


def llama_prices(chain: str, addrs: list[str]) -> dict[str, float]:
    out: dict[str, float] = {}
    ck = LLAMA_CHAIN[chain]
    for i in range(0, len(addrs), 40):
        chunk = addrs[i:i + 40]
        url = ("https://coins.llama.fi/prices/current/"
               + ",".join(f"{ck}:{a}" for a in chunk))
        try:
            with urllib.request.urlopen(url, timeout=45) as r:
                data = json.loads(r.read()).get("coins", {})
            for k, v in data.items():
                out[k.split(":", 1)[1].lower()] = v.get("price")
        except Exception as e:
            print(f"  llama batch failed: {str(e)[:60]}")
        time.sleep(0.3)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--chain", default="ethereum")
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--min-borrow-usd", type=float, default=10_000.0)
    ap.add_argument("--venue-report", default=None,
                    help="merge 30d liquidation stats (median clips, bonus) per market")
    args = ap.parse_args()

    wd = Path(args.workdir)
    wd.mkdir(parents=True, exist_ok=True)
    url = build_rpc(args.chain)

    tip = int(rpc(url, "eth_blockNumber", []), 16)
    frm = DEPLOY_BLOCK.get(args.chain, 1)
    print(f"[{args.chain}] CreateMarket sweep blocks {frm:,}-{tip:,}")
    logs = fetch_create_markets(url, frm, tip, wd / f"createmarket_{args.chain}.jsonl")
    print(f"  {len(logs)} markets created")

    # decode market params straight from the CreateMarket event data
    mkts = {}
    for lg in logs:
        mid = lg["topics"][1]
        d = lg["data"][2:]
        mkts[mid] = {
            "market_id": mid,
            "loan": "0x" + d[0:64][-40:].lower(),
            "collateral": "0x" + d[64:128][-40:].lower(),
            "oracle": "0x" + d[128:192][-40:].lower(),
            "lltv": int(d[256:320], 16) / 1e18,
        }

    mids = sorted(mkts)
    state = multicall(url, [(MORPHO, SEL_MARKET + m[2:]) for m in mids])
    live = []
    for mid, st in zip(mids, state):
        if not st or len(st) < 2 + 64 * 4:
            continue
        borrow = int(st[2 + 64 * 2: 2 + 64 * 3], 16)
        if borrow > 0:
            mkts[mid]["borrow_raw"] = borrow
            live.append(mid)
    print(f"  {len(live)} markets with totalBorrowAssets > 0")

    # token meta for every distinct loan/collateral among live markets
    toks = sorted({mkts[m]["loan"] for m in live} | {mkts[m]["collateral"] for m in live})
    metas = multicall(url, [(t, SEL_DECIMALS) for t in toks])
    syms = multicall(url, [(t, SEL_SYMBOL) for t in toks])
    meta = {}
    for t, dres, sres in zip(toks, metas, syms):
        dec = int(dres, 16) if dres else 18
        sym = "?"
        if sres:
            raw = sres[2:]
            try:
                if len(raw) == 64:
                    sym = bytes.fromhex(raw).rstrip(b"\x00").decode("utf-8", "replace")
                elif len(raw) > 128:
                    n = int(raw[64:128], 16)
                    sym = bytes.fromhex(raw[128:128 + 2 * n]).decode("utf-8", "replace")
            except Exception:
                pass
        meta[t] = {"decimals": dec, "symbol": sym}
    px = llama_prices(args.chain, toks)

    vr_markets = {}
    if args.venue_report:
        rep = json.loads(Path(args.venue_report).read_text(encoding="utf-8"))
        for v in rep:
            if v.get("chain") == args.chain and "Morpho" in v.get("label", ""):
                for m in v.get("markets", []):
                    if m.get("market_id"):
                        vr_markets[m["market_id"].lower()] = m

    rows, skipped = [], 0
    for mid in live:
        m = mkts[mid]
        lmeta = meta[m["loan"]]
        borrow_usd = None
        lp = px.get(m["loan"])
        if lp is not None:
            borrow_usd = m["borrow_raw"] / 10 ** lmeta["decimals"] * lp
        vr = vr_markets.get(mid.lower(), {})
        row = {
            "chain": args.chain, "protocol": "morpho",
            "venue": f"{args.chain}/Morpho Blue", "market_id": mid,
            "collateral": m["collateral"], "debt": m["loan"],
            "pair": f"{meta[m['collateral']]['symbol']}/{lmeta['symbol']}",
            "lltv": m["lltv"], "oracle": m["oracle"],
            "borrow_usd": borrow_usd, "open_or_gated": "OPEN",
            "liq_events_30d": vr.get("n", 0),
            "bonus_oracle_usd": vr.get("bonus_oracle_usd", 0.0),
            "repaid_usd": vr.get("repaid_usd", 0.0),
            "median_clip_usd": vr.get("median_clip_usd"),
            "p90_clip_usd": vr.get("p90_clip_usd"),
        }
        # gate0 quote budget goes to markets that matter: live borrow above the
        # floor OR any 30d liquidation history. The rest are recorded as skipped.
        if (borrow_usd or 0) >= args.min_borrow_usd or vr.get("n"):
            rows.append(row)
        else:
            skipped += 1
            row["gate0_skipped"] = ("borrow below floor" if borrow_usd is not None
                                    else "no loan-token price; borrow unvalued")
            rows.append(row)

    out = wd / f"morpho_markets_{args.chain}.json"
    out.write_text(json.dumps({
        "chain": args.chain, "tip_block": tip,
        "markets_created": len(logs), "markets_live": len(live),
        "gate0_candidates": sum(1 for r in rows if "gate0_skipped" not in r),
        "skipped_below_floor": skipped,
        "min_borrow_usd": args.min_borrow_usd,
        "rows": rows}, indent=1), encoding="utf-8")
    print(f"wrote {out}: {len(rows)} live rows, "
          f"{sum(1 for r in rows if 'gate0_skipped' not in r)} gate0 candidates, "
          f"{skipped} skipped below ${args.min_borrow_usd:,.0f} floor")
    return 0


if __name__ == "__main__":
    sys.exit(main())
