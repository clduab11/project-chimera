#!/usr/bin/env python3
"""
measure_liquidation_flow.py
Project Chimera - 30-day Aave V3 liquidation FLOW measurement (Base).

Scans LiquidationCall events on the Aave V3 Pool over a trailing window,
prices seized collateral and repaid debt in USD via the on-chain AaveOracle
(current prices), and reports totals, distributions, and winner concentration.

This measures the revenue POOL that liquidators competed for -- the number
that decides whether the latency race is worth entering.

Stdlib only (no web3.py). Checkpointed: safe to re-run, resumes from where
it stopped as long as --workdir is the same and the chain tip hasn't moved
past the staleness bound.

Usage:
  python scripts/measure_liquidation_flow.py --days 30 --workdir <dir>

Env: BASE_RPC_URL (falls back to parsing .env.live in repo root).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.request
import urllib.error
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

POOL = "0xa238dd80c259a72e81d7e4664a9801593f98d1c5"
# cast sig-event 'LiquidationCall(address indexed,address indexed,address indexed,uint256,uint256,address,bool)'
TOPIC0 = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"
SEL_ADDRESSES_PROVIDER = "0x0542975c"
SEL_GET_PRICE_ORACLE = "0xfca513a8"
SEL_GET_ASSET_PRICE = "0xb3596f07"
SEL_BASE_CURRENCY_UNIT = "0x8c89b64f"
SEL_DECIMALS = "0x313ce567"
SEL_SYMBOL = "0x95d89b41"

BASE_BLOCK_TIME_S = 2.0  # Base mainnet: fixed 2-second blocks

_rpc_id = 0


def load_rpc_url(repo_root: Path) -> str:
    url = os.getenv("BASE_RPC_URL")
    if url:
        return url
    env_file = repo_root / ".env.live"
    if env_file.exists():
        for line in env_file.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line.startswith("BASE_RPC_URL="):
                return line.split("=", 1)[1].strip()
    raise SystemExit("BASE_RPC_URL not set and .env.live not found")


def rpc(url: str, method: str, params: list, retries: int = 6):
    global _rpc_id
    _rpc_id += 1
    payload = json.dumps(
        {"jsonrpc": "2.0", "id": _rpc_id, "method": method, "params": params}
    ).encode()
    last_err = None
    for attempt in range(retries):
        try:
            req = urllib.request.Request(
                url, data=payload, headers={"Content-Type": "application/json"}
            )
            with urllib.request.urlopen(req, timeout=90) as resp:
                body = json.loads(resp.read())
            if "error" in body:
                # Range/size errors are handled by the caller (chunk splitting);
                # raise a typed error so it can distinguish.
                raise RpcLogicError(body["error"])
            return body["result"]
        except RpcLogicError:
            raise
        except Exception as exc:  # network hiccup, 429, 5xx
            last_err = exc
            time.sleep(min(2**attempt, 20))
    raise ConnectionError(f"RPC {method} failed after {retries} tries: {last_err}")


class RpcLogicError(Exception):
    def __init__(self, err: dict):
        self.code = err.get("code")
        self.message = err.get("message", "")
        super().__init__(f"{self.code}: {self.message}")


def eth_call(url: str, to: str, data: str) -> str:
    return rpc(url, "eth_call", [{"to": to, "data": data}, "latest"])


def word(hexstr: str, i: int) -> str:
    """i-th 32-byte word of a 0x-prefixed hex string."""
    return hexstr[2 + 64 * i : 2 + 64 * (i + 1)]


def addr_from_word(w: str) -> str:
    return "0x" + w[24:]


def decode_symbol(result: str) -> str:
    if not result or result == "0x":
        return "?"
    raw = result[2:]
    if len(raw) == 64:  # bytes32-style symbol
        return bytes.fromhex(raw).rstrip(b"\x00").decode("utf-8", "replace")
    try:
        length = int(raw[64:128], 16)
        return bytes.fromhex(raw[128 : 128 + 2 * length]).decode("utf-8", "replace")
    except Exception:
        return "?"


def get_logs_range(url: str, frm: int, to: int, out_fh, state: dict) -> int:
    """Fetch logs [frm, to] with adaptive splitting. Returns events appended."""
    chunk = state.get("chunk_size", 50_000)
    total = 0
    cur = frm
    while cur <= to:
        end = min(cur + chunk - 1, to)
        try:
            logs = rpc(
                url,
                "eth_getLogs",
                [
                    {
                        "address": POOL,
                        "topics": [TOPIC0],
                        "fromBlock": hex(cur),
                        "toBlock": hex(end),
                    }
                ],
            )
        except RpcLogicError as exc:
            # Provider says the range is too large -> halve and retry.
            if chunk <= 500:
                raise
            chunk = max(500, chunk // 2)
            state["chunk_size"] = chunk
            print(f"  range {cur}-{end} rejected ({exc.message[:80]}); chunk -> {chunk}")
            continue
        for lg in logs:
            out_fh.write(json.dumps(lg) + "\n")
        out_fh.flush()
        total += len(logs)
        state["next_start"] = end + 1
        state["chunk_size"] = chunk
        save_state(state)
        done_pct = 100.0 * (end - state["from_block"] + 1) / (
            state["to_block"] - state["from_block"] + 1
        )
        print(
            f"  blocks {cur}-{end}: {len(logs)} events "
            f"(cum {state.get('cum', 0) + total}, {done_pct:.1f}%)"
        )
        cur = end + 1
        # Grow chunk back slowly after successes.
        if chunk < 50_000:
            chunk = min(50_000, int(chunk * 1.5))
    return total


STATE_PATH: Path | None = None


def save_state(state: dict) -> None:
    assert STATE_PATH is not None
    STATE_PATH.write_text(json.dumps(state))


def main() -> int:
    global STATE_PATH
    ap = argparse.ArgumentParser()
    ap.add_argument("--days", type=int, default=30)
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--rpc-url", default=None)
    args = ap.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    url = args.rpc_url or load_rpc_url(repo_root)
    workdir = Path(args.workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    raw_path = workdir / "liquidation_logs.jsonl"
    STATE_PATH = workdir / "scan_state.json"

    # --- Window ---------------------------------------------------------
    if STATE_PATH.exists():
        state = json.loads(STATE_PATH.read_text())
        print(
            f"Resuming: window {state['from_block']}-{state['to_block']}, "
            f"next_start={state['next_start']}"
        )
    else:
        latest = int(rpc(url, "eth_blockNumber", []), 16)
        span = int(args.days * 86400 / BASE_BLOCK_TIME_S)
        state = {
            "from_block": latest - span,
            "to_block": latest,
            "next_start": latest - span,
            "chunk_size": 50_000,
            "cum": 0,
        }
        save_state(state)
        print(f"Window: blocks {state['from_block']}-{state['to_block']} ({args.days}d)")

    # Anchor timestamps for interpolation (Base blocks are a fixed 2 s).
    b0 = rpc(url, "eth_getBlockByNumber", [hex(state["from_block"]), False])
    b1 = rpc(url, "eth_getBlockByNumber", [hex(state["to_block"]), False])
    t0, t1 = int(b0["timestamp"], 16), int(b1["timestamp"], 16)
    n0, n1 = state["from_block"], state["to_block"]

    def ts_of(block: int) -> int:
        return t0 + round((block - n0) * (t1 - t0) / (n1 - n0))

    # --- Scan -----------------------------------------------------------
    if state["next_start"] <= state["to_block"]:
        mode = "a" if raw_path.exists() else "w"
        with open(raw_path, mode, encoding="utf-8") as fh:
            n = get_logs_range(url, state["next_start"], state["to_block"], fh, state)
        state["cum"] = state.get("cum", 0) + n
        save_state(state)
    print(f"Scan complete: raw log lines in {raw_path}")

    # --- Decode ----------------------------------------------------------
    events = []
    seen = set()  # (tx, logIndex) dedupe in case of resume overlap
    with open(raw_path, encoding="utf-8") as fh:
        for line in fh:
            lg = json.loads(line)
            key = (lg["transactionHash"], lg["logIndex"])
            if key in seen:
                continue
            seen.add(key)
            data = lg["data"]
            events.append(
                {
                    "block": int(lg["blockNumber"], 16),
                    "tx": lg["transactionHash"],
                    "collateral": addr_from_word(lg["topics"][1][2:]).lower(),
                    "debt": addr_from_word(lg["topics"][2][2:]).lower(),
                    "user": addr_from_word(lg["topics"][3][2:]).lower(),
                    "debt_to_cover": int(word(data, 0), 16),
                    "seized": int(word(data, 1), 16),
                    "liquidator": addr_from_word(word(data, 2)).lower(),
                    "receive_atoken": int(word(data, 3), 16) != 0,
                }
            )
    events.sort(key=lambda e: e["block"])
    print(f"Decoded {len(events)} unique LiquidationCall events")

    # --- Asset metadata + prices (current, via AaveOracle) ----------------
    provider = addr_from_word(word(eth_call(url, POOL, SEL_ADDRESSES_PROVIDER), 0))
    oracle = addr_from_word(word(eth_call(url, provider, SEL_GET_PRICE_ORACLE), 0))
    base_unit = int(eth_call(url, oracle, SEL_BASE_CURRENCY_UNIT), 16)
    print(f"AaveOracle {oracle}, BASE_CURRENCY_UNIT={base_unit}")

    assets = sorted({e["collateral"] for e in events} | {e["debt"] for e in events})
    meta = {}
    for a in assets:
        dec = int(eth_call(url, a, SEL_DECIMALS), 16)
        sym = decode_symbol(eth_call(url, a, SEL_SYMBOL))
        px = int(eth_call(url, oracle, SEL_GET_ASSET_PRICE + a[2:].rjust(64, "0")), 16)
        meta[a] = {"symbol": sym, "decimals": dec, "price_usd": px / base_unit}
        print(f"  {sym:12s} {a} dec={dec} px=${px / base_unit:,.6f}")

    # --- Enrich ------------------------------------------------------------
    for e in events:
        mc, md = meta[e["collateral"]], meta[e["debt"]]
        e["seized_usd"] = e["seized"] / 10 ** mc["decimals"] * mc["price_usd"]
        e["debt_usd"] = e["debt_to_cover"] / 10 ** md["decimals"] * md["price_usd"]
        e["bonus_usd"] = e["seized_usd"] - e["debt_usd"]
        e["ts"] = ts_of(e["block"])
        e["day"] = datetime.fromtimestamp(e["ts"], tz=timezone.utc).strftime("%Y-%m-%d")
        e["same_asset"] = e["collateral"] == e["debt"]

    # --- Aggregate -----------------------------------------------------------
    tot_seized = sum(e["seized_usd"] for e in events)
    tot_debt = sum(e["debt_usd"] for e in events)
    tot_bonus = sum(e["bonus_usd"] for e in events)

    days = defaultdict(lambda: {"n": 0, "seized_usd": 0.0, "bonus_usd": 0.0})
    for e in events:
        d = days[e["day"]]
        d["n"] += 1
        d["seized_usd"] += e["seized_usd"]
        d["bonus_usd"] += e["bonus_usd"]

    by_coll = defaultdict(lambda: {"n": 0, "seized_usd": 0.0})
    by_debtasset = defaultdict(lambda: {"n": 0, "debt_usd": 0.0})
    by_liq = defaultdict(lambda: {"n": 0, "seized_usd": 0.0, "bonus_usd": 0.0})
    for e in events:
        c = by_coll[meta[e["collateral"]]["symbol"]]
        c["n"] += 1
        c["seized_usd"] += e["seized_usd"]
        d = by_debtasset[meta[e["debt"]]["symbol"]]
        d["n"] += 1
        d["debt_usd"] += e["debt_usd"]
        l = by_liq[e["liquidator"]]
        l["n"] += 1
        l["seized_usd"] += e["seized_usd"]
        l["bonus_usd"] += e["bonus_usd"]

    buckets = [(0, 1), (1, 10), (10, 100), (100, 1_000), (1_000, 10_000),
               (10_000, 100_000), (100_000, float("inf"))]
    dist = []
    for lo, hi in buckets:
        sel = [e for e in events if lo <= e["seized_usd"] < hi]
        dist.append(
            {
                "bucket": f"${lo:,.0f}-{'inf' if hi == float('inf') else f'${hi:,.0f}'}",
                "n": len(sel),
                "seized_usd": sum(x["seized_usd"] for x in sel),
                "bonus_usd": sum(x["bonus_usd"] for x in sel),
            }
        )

    sizes = sorted(e["seized_usd"] for e in events)

    def pct(p: float) -> float:
        if not sizes:
            return 0.0
        return sizes[min(len(sizes) - 1, int(p * len(sizes)))]

    cutoff_7d = t1 - 7 * 86400
    last7 = [e for e in events if e["ts"] >= cutoff_7d]

    summary = {
        "window": {
            "days": args.days,
            "from_block": n0,
            "to_block": n1,
            "from_utc": datetime.fromtimestamp(t0, tz=timezone.utc).isoformat(),
            "to_utc": datetime.fromtimestamp(t1, tz=timezone.utc).isoformat(),
        },
        "pricing": "current AaveOracle prices (not event-time)",
        "totals": {
            "events": len(events),
            "unique_txs": len({e["tx"] for e in events}),
            "unique_liquidators": len(by_liq),
            "unique_users": len({e["user"] for e in events}),
            "seized_usd": tot_seized,
            "debt_repaid_usd": tot_debt,
            "gross_bonus_usd": tot_bonus,
            "receive_atoken_events": sum(1 for e in events if e["receive_atoken"]),
            "same_asset_events": sum(1 for e in events if e["same_asset"]),
        },
        "last_7d": {
            "events": len(last7),
            "seized_usd": sum(e["seized_usd"] for e in last7),
            "gross_bonus_usd": sum(e["bonus_usd"] for e in last7),
        },
        "event_size_usd": {
            "p50": pct(0.50), "p90": pct(0.90), "p99": pct(0.99),
            "max": sizes[-1] if sizes else 0.0,
        },
        "size_distribution": dist,
        "by_collateral": dict(
            sorted(by_coll.items(), key=lambda kv: -kv[1]["seized_usd"])
        ),
        "by_debt_asset": dict(
            sorted(by_debtasset.items(), key=lambda kv: -kv[1]["debt_usd"])
        ),
        "top_liquidators": dict(
            sorted(by_liq.items(), key=lambda kv: -kv[1]["seized_usd"])[:15]
        ),
        "daily": dict(sorted(days.items())),
        "asset_meta": meta,
    }

    (workdir / "flow_summary.json").write_text(json.dumps(summary, indent=2))
    with open(workdir / "events_enriched.csv", "w", encoding="utf-8") as fh:
        fh.write(
            "block,ts_utc,tx,collateral,debt,seized_usd,debt_usd,bonus_usd,"
            "liquidator,receive_atoken,same_asset\n"
        )
        for e in events:
            fh.write(
                f"{e['block']},{datetime.fromtimestamp(e['ts'], tz=timezone.utc).isoformat()},"
                f"{e['tx']},{meta[e['collateral']]['symbol']},{meta[e['debt']]['symbol']},"
                f"{e['seized_usd']:.2f},{e['debt_usd']:.2f},{e['bonus_usd']:.2f},"
                f"{e['liquidator']},{int(e['receive_atoken'])},{int(e['same_asset'])}\n"
            )

    # --- Report ----------------------------------------------------------
    print("\n================ 30-DAY LIQUIDATION FLOW (Base Aave V3) ================")
    print(f"Window   : {summary['window']['from_utc']} -> {summary['window']['to_utc']}")
    print(f"Events   : {len(events)}  (txs {summary['totals']['unique_txs']}, "
          f"liquidators {len(by_liq)}, users {summary['totals']['unique_users']})")
    print(f"Seized   : ${tot_seized:,.0f}")
    print(f"Repaid   : ${tot_debt:,.0f}")
    print(f"Bonus    : ${tot_bonus:,.0f}   <-- gross revenue pool (pre gas/swap)")
    print(f"Last 7d  : {len(last7)} events, ${summary['last_7d']['seized_usd']:,.0f} seized, "
          f"${summary['last_7d']['gross_bonus_usd']:,.0f} bonus")
    print(f"Sizes    : p50 ${pct(0.5):,.2f}  p90 ${pct(0.9):,.2f}  "
          f"p99 ${pct(0.99):,.2f}  max ${sizes[-1] if sizes else 0:,.2f}")
    print("\nSize distribution (seized USD):")
    for d in dist:
        print(f"  {d['bucket']:>22s}: {d['n']:5d} events  ${d['seized_usd']:>14,.0f} seized"
              f"  ${d['bonus_usd']:>12,.0f} bonus")
    print("\nTop collateral:")
    for sym, v in list(summary["by_collateral"].items())[:10]:
        print(f"  {sym:12s} {v['n']:5d} events  ${v['seized_usd']:>14,.0f}")
    print("\nTop liquidators (by seized USD):")
    for adr, v in list(summary["top_liquidators"].items())[:10]:
        share = 100 * v["seized_usd"] / tot_seized if tot_seized else 0
        print(f"  {adr} {v['n']:5d} events  ${v['seized_usd']:>14,.0f}  ({share:.1f}%)")
    print(f"\nArtifacts: {workdir / 'flow_summary.json'}, {workdir / 'events_enriched.csv'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
