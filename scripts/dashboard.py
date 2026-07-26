#!/usr/bin/env python3
"""Chimera ops dashboard — single-file, stdlib-only local monitoring server.

Serves http://127.0.0.1:9553 (override: CHIMERA_DASH_PORT / CHIMERA_DASH_BIND).

Data sources (all read-only w.r.t. the engine):
  - Engine Prometheus text proxied from 127.0.0.1:CHIMERA_ENGINE_PORT (default 9554).
    NOTE: the engine's metrics server answers Prometheus text on ANY path; it has
    no real /health endpoint, so health is synthesized here from reachability
    plus log freshness.
  - Tail of logs/chimera.log.* (tracing format) for blocks, events, oracle price.
  - config/snapshot.json for snapshot block/age.
  - Public ETH/USD ticker: Binance 24h ticker -> Coinbase spot fallback
    (no API keys; cached in a background thread, never blocks requests).

Safety: binds localhost only, never reads .env.live, redacts URLs and drops
sensitive-looking log lines before serving. No third-party dependencies, so it
imports cleanly without web3.py (AGENTS.md invariant).
"""
from __future__ import annotations

import glob
import http.client
import json
import os
import re
import threading
import time
import urllib.request
from collections import deque
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BIND = os.environ.get("CHIMERA_DASH_BIND", "127.0.0.1")
PORT = int(os.environ.get("CHIMERA_DASH_PORT", "9553"))
ENGINE_PORT = int(os.environ.get("CHIMERA_ENGINE_PORT", "9554"))

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LOG_GLOB = os.path.join(REPO, "logs", "chimera.log*")
SNAPSHOT_FILE = os.path.join(REPO, "config", "snapshot.json")
HW_FILE = os.path.join(REPO, "logs", ".dashboard_hw.json")  # logs/ is gitignored
HTML_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "dashboard.html")
ROUTING_FILE = os.path.join(REPO, "config", "routing.yaml")
OUTCOMES_FILE = os.path.join(REPO, "core", "state", "outcomes.jsonl")  # engine audit trail

# --- Swap observability constants -----------------------------------------
# Known DEX *factory* addresses (lowercased). A factory in a router_address
# slot means every swap through it reverts (no swap selector). This is the
# SOFT lint superset: it also lists the canonical Uniswap V3 factory that the
# committed uniswap-v3-arbitrum venue still carries as an inert placeholder
# (Rust config load only hard-fails on the Base factories — see
# core/src/config.rs KNOWN_FACTORY_ROUTERS).
KNOWN_FACTORY_ROUTERS = {
    "0x71524b4f93c58fcbf659783284e38825f0622859": "SushiSwap V2 factory (Base)",
    "0x33128a8fc17869897dce68ed026d694621f6fdfd": "Uniswap V3 factory (Base)",
    "0x1f98431c8ad98523631ae4a59f267346ea31f984": "Uniswap V3 factory (canonical/Arbitrum)",
}
ZERO_ADDR = "0x" + "0" * 40

# Debt/collateral assets appearing in config/routing.yaml pairs, for rough
# USD sizing of flash loans (depth-cap heuristic). Lowercased address ->
# (kind, decimals). "stable" ~ $1; "weth" priced via live/oracle ETH price.
ASSET_INFO = {
    "0x4200000000000000000000000000000000000006": ("weth", 18),    # WETH (Base)
    "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913": ("stable", 6),   # USDC (Base)
    "0xd9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca": ("stable", 6),   # USDbC (Base)
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1": ("weth", 18),    # WETH (Arbitrum)
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831": ("stable", 6),   # USDC (Arbitrum)
    "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9": ("stable", 6),   # USDT (Arbitrum)
}
# Heuristic until Phase Echo lands real depth-based sizing: flag an execution
# as "depth cap binding" when the flash-loan notional exceeds this fraction of
# the venue's declared liquidity floor (liquidity_usd_min).
DEPTH_CAP_FRACTION = 0.10
SWAP_SHADOW_KEEP = 200  # per-execution shadow records retained for joining

TAIL_INITIAL_BYTES = 384 * 1024
TAIL_LINES = 150          # served to client (client renders last ~50 after filter)
EVENT_LINES_KEEP = 200    # rolling event buffer

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
URL_RE = re.compile(r"\b(?:https?|wss?)://[^\s\"'\]]+")
SECRET_RE = re.compile(r"private[_ -]?key|keystore|passphrase|seed phrase|mnemonic", re.I)
TX_RE = re.compile(r"0x[0-9a-fA-F]{64}")
TS_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})")
BLOCK_RE = re.compile(r"NewBlock \{ number: (\d+)")
ORACLE_RE = re.compile(r"eth_usd=([0-9.]+)")
START_RE = re.compile(r"Orchestrator starting.*chain_id=(\d+)\s+execute_mode=(\w+)")
TOPUP_RE = re.compile(r"\[sweep-live\] broadcast")
WEI_RE = re.compile(r"spendable_wei=(\d+)")
PROM_LINE_RE = re.compile(r"^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{[^}]*\})?\s+(-?[0-9.eE+]+)$")

# Swap observability: field extractors for the per-execution SHADOW line
# (orchestrator.rs "SHADOW: would submit Executor.execute(bytes)").
SWAP_ID_RE = re.compile(r"\bid=(\S+)")
SWAP_VENUE_RE = re.compile(r"\bvenue=(\S+)")
SWAP_AOM_RE = re.compile(r"\bamount_out_min=(\d+)")
SWAP_EXP_RE = re.compile(r"\bexpected_profit_usd=(-?[0-9.]+)")
SWAP_FLASH_ASSET_RE = re.compile(r"\bflash_loan_asset=(0x[0-9a-fA-F]{40})")
SWAP_FLASH_AMT_RE = re.compile(r"\bflash_loan_amount=(\d+)")


# --------------------------------------------------------------------------
# Prometheus text parsing (manual — the file is ~15 lines, no client lib).
# --------------------------------------------------------------------------
def parse_prometheus(text: str) -> dict:
    """Flatten Prometheus text to {name: value}. Labeled series also get a
    label-suffixed key, e.g. chimera_sims_run_total{result="success"} ->
    chimera_sims_run_total_success (plus the bare name, last series wins)."""
    out = {}
    label_re = re.compile(r'(\w+)="([^"]*)"')
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        m = PROM_LINE_RE.match(line)
        if not m:
            continue
        name, labels, raw = m.group(1), m.group(2), m.group(3)
        try:
            val = float(raw)
        except ValueError:
            continue
        out[name] = val
        if labels:
            suffix = "_".join(f"{k}_{v}" for k, v in label_re.findall(labels))
            out[f"{name}_{suffix}"] = val
    return out


def _engine_fetch_once(path: str = "/metrics") -> str:
    """Single attempt. The engine's metrics server writes a response without
    ever reading the request, then drops the socket — Windows then races a RST
    at the client (WinError 10053). Salvage whatever body bytes arrived."""
    conn = http.client.HTTPConnection("127.0.0.1", ENGINE_PORT, timeout=2.5)
    try:
        conn.request("GET", path, headers={"Connection": "close"})
        resp = conn.getresponse()
        try:
            body = resp.read()
        except http.client.IncompleteRead as e:
            body = e.partial or b""
        text = body.decode("utf-8", "replace")
        if not text.strip():
            raise ConnectionError("empty body from engine metrics")
        return text
    finally:
        conn.close()


def engine_fetch(path: str = "/metrics"):
    """Return (body_text, error). One immediate retry on failure (10053 race
    is transient). Never raises."""
    try:
        return _engine_fetch_once(path), None
    except Exception:  # noqa: BLE001
        pass
    try:
        return _engine_fetch_once(path), None
    except Exception as e:  # noqa: BLE001 - degrade gracefully by design
        return None, f"{type(e).__name__}: {e}"


# --------------------------------------------------------------------------
# Routing config view: stdlib line-based parse of the venues block only
# (no PyYAML — third-party deps are banned here), plus config lint.
# --------------------------------------------------------------------------
_VENUE_START_RE = re.compile(r"^\s*-\s+name:\s*(.+)$")
_VENUE_FIELD_RE = re.compile(
    r"^\s{2,}(chain|liquidity_usd_min|type|kyc|router_compatibility|router_address):\s*(.+)$"
)
_TOP_KEY_RE = re.compile(r"^[A-Za-z_]+:")


def _yaml_scalar(v: str) -> str:
    v = v.split("#", 1)[0].strip()
    if len(v) >= 2 and v[0] == '"' and v[-1] == '"':
        v = v[1:-1]
    return v.strip()


def parse_routing_venues(text: str) -> list[dict]:
    """Extract venue scalar fields from routing.yaml. Intentionally minimal:
    understands only the committed layout (list of maps under `venues:`,
    nested `pairs:` entries are skipped)."""
    venues, cur, in_venues = [], None, False
    for raw in text.splitlines():
        if _TOP_KEY_RE.match(raw):
            in_venues = raw.startswith("venues:")
            cur = None
            continue
        if not in_venues:
            continue
        m = _VENUE_START_RE.match(raw)
        if m:
            cur = {"name": _yaml_scalar(m.group(1))}
            venues.append(cur)
            continue
        if cur is None or raw.lstrip().startswith("- "):
            continue  # pairs entries ("- token_in: ...") and stray lists
        m = _VENUE_FIELD_RE.match(raw)
        if m:
            cur[m.group(1)] = _yaml_scalar(m.group(2))
    return venues


def routing_lint(venues: list[dict]) -> list[dict]:
    """Config lint over parsed venues. Mirrors (as a SOFT superset) the hard
    checks in core/src/config.rs RoutingConfig::validate: factory-in-router-slot,
    empty/zero router on a selectable venue, same-chain duplicate routers."""
    issues = []
    seen: dict = {}
    for v in venues:
        name = v.get("name", "?")
        chain = v.get("chain", "?")
        router = (v.get("router_address") or "").lower()
        compat = v.get("router_compatibility") or ""
        if router in KNOWN_FACTORY_ROUTERS:
            issues.append({
                "venue": name,
                "kind": "router_is_factory",
                "severity": "critical",
                "detail": f"router_address is {KNOWN_FACTORY_ROUTERS[router]} — "
                          "swaps through it revert",
            })
        selectable = (v.get("type") or "") == "dex" and compat in ("v2", "v3", "custom")
        if selectable and (not router or router == ZERO_ADDR):
            issues.append({
                "venue": name,
                "kind": "router_empty_or_zero",
                "severity": "critical",
                "detail": f"selectable venue ({compat}) has empty/zero router_address",
            })
        if router and router != ZERO_ADDR:
            key = (chain, router)
            if key in seen:
                issues.append({
                    "venue": name,
                    "kind": "duplicate_router",
                    "severity": "warn",
                    "detail": f"shares router_address with '{seen[key]}' on chain {chain}",
                })
            else:
                seen[key] = name
    return issues


class RoutingView:
    """mtime-cached parse + lint of config/routing.yaml."""

    def __init__(self):
        self.lock = threading.Lock()
        self.mtime = None
        self.venues: list[dict] = []
        self.issues: list[dict] = []

    def poll(self):
        try:
            mtime = os.path.getmtime(ROUTING_FILE)
        except OSError:
            with self.lock:
                self.venues, self.issues, self.mtime = [], [], None
            return
        with self.lock:
            if mtime == self.mtime:
                return
            try:
                # Binary + lossy decode: a stray non-UTF-8 byte in a yaml
                # comment must not 500 every /api/state request.
                with open(ROUTING_FILE, "rb") as f:
                    text = f.read().decode("utf-8", "replace")
            except OSError:
                return
            self.venues = parse_routing_venues(text)
            self.issues = routing_lint(self.venues)
            self.mtime = mtime

    def snapshot(self):
        with self.lock:
            return [dict(v) for v in self.venues], [dict(i) for i in self.issues]


# --------------------------------------------------------------------------
# Outcomes tailer: incremental reader of the engine's JSONL audit trail
# (core/state/outcomes.jsonl — OutcomeRecord per line, Decimal as string).
# --------------------------------------------------------------------------
class OutcomesTailer:
    def __init__(self):
        self.lock = threading.Lock()
        self.offset = 0
        self.pending = b""
        self.by_venue: dict = {}
        self.recent = deque(maxlen=100)
        self.total = 0
        self.parse_errors = 0

    def poll(self):
        try:
            size = os.path.getsize(OUTCOMES_FILE)
        except OSError:
            return
        with self.lock:
            if size < self.offset:  # rotation/truncation: rebuild from scratch
                self.offset = 0
                self.pending = b""
                self.by_venue = {}
                self.recent.clear()
                self.total = 0
            if size == self.offset:
                return
            try:
                with open(OUTCOMES_FILE, "rb") as f:
                    f.seek(self.offset)
                    data = f.read()
            except OSError:
                return
            self.offset += len(data)
            buf = self.pending + data
            *lines, self.pending = buf.split(b"\n")
            for raw in lines:
                raw = raw.strip()
                if not raw:
                    continue
                try:
                    rec = json.loads(raw.decode("utf-8", "replace"))
                except ValueError:
                    self.parse_errors += 1
                    continue
                if not isinstance(rec, dict):
                    # Valid JSON but not an OutcomeRecord object (possible from
                    # interleaved partial appends) — must not raise mid-batch.
                    self.parse_errors += 1
                    continue
                self._ingest(rec)

    def _ingest(self, rec: dict):
        venue = rec.get("venue") or "unknown"
        agg = self.by_venue.setdefault(
            venue, {"executions": 0, "reverts": 0, "realized_net_usd": 0.0}
        )
        agg["executions"] += 1
        reverted = bool(rec.get("reverted"))
        if reverted:
            agg["reverts"] += 1
        try:
            realized = float(rec.get("realized_net_usd") or 0.0)
        except (TypeError, ValueError):
            realized = 0.0
        agg["realized_net_usd"] += realized
        self.total += 1
        self.recent.append({
            "id": rec.get("id"),
            "ts": rec.get("timestamp"),
            "venue": venue,
            "realized_usd": realized,
            "reverted": reverted,
        })

    def snapshot(self):
        with self.lock:
            return {
                "by_venue": {k: dict(v) for k, v in self.by_venue.items()},
                "recent": list(self.recent),
                "total": self.total,
                "parse_errors": self.parse_errors,
            }


# --------------------------------------------------------------------------
# Incremental log tailer.
# --------------------------------------------------------------------------
class LogTailer:
    def __init__(self):
        self.lock = threading.Lock()
        self.path = None
        self.offset = 0
        self.pending = b""
        self.lines = deque(maxlen=600)      # redacted display lines
        self.events = deque(maxlen=EVENT_LINES_KEEP)
        self.latest_block = None
        self.latest_block_ts = None
        self.run_start_block = None   # first NewBlock after latest engine start
        self.scan_cycles = 0          # NewBlock lines seen by this tailer
        self.oracle_usd = None
        self.start_iso = None               # last "Orchestrator starting" ts
        self.mode = None
        self.chain_id = None
        self.topup_wei = 0
        self.topup_count = 0
        # Swap observability: per-execution SHADOW records (joined against the
        # outcomes JSONL by opportunity id) + route-resolution counters.
        self.swap_shadow = {}          # id -> shadow execution record
        self.swap_shadow_ids = deque() # insertion order for bounded eviction
        self.route_miss_count = 0      # "No (eligible) V2 route found"
        self.rotation_reroute_count = 0  # resolver fell through to next venue
        self.rotation_denied_count = 0   # pacing denied post-hoc (legacy drop path)

    @staticmethod
    def _current_file():
        files = [f for f in glob.glob(LOG_GLOB) if not f.endswith(".json")]
        if not files:
            return None
        return max(files, key=os.path.getmtime)

    def poll(self):
        path = self._current_file()
        if not path:
            return
        try:
            size = os.path.getsize(path)
        except OSError:
            return
        with self.lock:
            if path != self.path or size < self.offset:
                # rotation or truncation: start from a tail window of the new file
                self.path = path
                self.pending = b""
                self.offset = max(0, size - TAIL_INITIAL_BYTES)
                if self.start_iso is None and self.offset > 0:
                    # One-time backward scan: recover the last engine start (for
                    # uptime/mode) and this run's first block, both of which may
                    # sit before the tail window in a long-grown log file.
                    self._attach_scan(path, self.offset)
            if size == self.offset:
                return
            try:
                with open(path, "rb") as f:
                    f.seek(self.offset)
                    data = f.read()
            except OSError:
                return
            self.offset += len(data)
            self._consume(data)

    def _attach_scan(self, path: str, stop_before: int):
        """One-pass scan of the file portion before the tail window, looking
        only for the last 'Orchestrator starting' line and the first NewBlock
        after it. Feeds those two lines through the normal handler."""
        last_start = None
        first_block = None
        carry = b""
        try:
            with open(path, "rb") as f:
                read_total = 0
                while read_total < stop_before:
                    chunk = f.read(min(1024 * 1024, stop_before - read_total))
                    if not chunk:
                        break
                    read_total += len(chunk)
                    data = carry + chunk
                    *lines, carry = data.split(b"\n")
                    for raw in lines:
                        if b"Orchestrator starting" in raw:
                            last_start = raw
                            first_block = None
                        elif last_start is not None and first_block is None and b"NewBlock { number:" in raw:
                            first_block = raw
        except OSError:
            return
        for raw in (last_start, first_block):
            if raw:
                line = ANSI_RE.sub("", raw.decode("utf-8", "replace")).rstrip()
                self._handle_line(line)

    def _consume(self, data: bytes):
        buf = self.pending + data
        *chunks, tail = buf.split(b"\n")
        self.pending = tail  # incomplete final line (or b"")
        # If we landed mid-line on first read, the first chunk is partial: drop it
        # only when it has no timestamp header (heuristic, cheap).
        for chunk in chunks:
            line = ANSI_RE.sub("", chunk.decode("utf-8", "replace")).rstrip()
            if not line.strip():
                continue
            if SECRET_RE.search(line):
                line = "[line redacted: sensitive material]"
            else:
                line = URL_RE.sub("<rpc-url>", line)
            self._handle_line(line)

    def _handle_line(self, line: str):
        ts = None
        m = TS_RE.match(line)
        if m:
            ts = m.group(1)
        self.lines.append({"ts": ts, "text": line})

        mb = BLOCK_RE.search(line)
        if mb:
            self.latest_block = int(mb.group(1))
            self.latest_block_ts = ts
            self.scan_cycles += 1
            if self.start_iso and self.run_start_block is None:
                self.run_start_block = self.latest_block
        mo = ORACLE_RE.search(line)
        if mo:
            try:
                self.oracle_usd = float(mo.group(1))
            except ValueError:
                pass
        ms = START_RE.search(line)
        if ms:
            self.chain_id = int(ms.group(1))
            self.mode = ms.group(2)
            self.start_iso = ts
            self.run_start_block = None  # reset: next NewBlock marks the new run
        if TOPUP_RE.search(line):
            mw = WEI_RE.search(line)
            if mw:
                self.topup_wei += int(mw.group(1))
                self.topup_count += 1

        # Swap observability hooks.
        if "SHADOW: would submit" in line:
            rec = self._parse_shadow_line(line, ts)
            if rec:
                rid = rec["id"]
                if rid not in self.swap_shadow:
                    self.swap_shadow_ids.append(rid)
                    while len(self.swap_shadow_ids) > SWAP_SHADOW_KEEP:
                        self.swap_shadow.pop(self.swap_shadow_ids.popleft(), None)
                self.swap_shadow[rid] = rec
        elif "route found; skipping candidate" in line:
            # Orchestrator line only ("No (eligible) V2 route found; skipping
            # candidate"). The resolver warns its own "No eligible V2 route
            # found for collateral/debt pair" for the SAME miss — counting both
            # substrings would double-count every miss.
            self.route_miss_count += 1
        elif "rotation-blocked; trying next venue" in line:
            self.rotation_reroute_count += 1
        if "Denied by local pacing gate" in line and "recently used" in line:
            self.rotation_denied_count += 1

        kind = None
        if "Liquidation submitted" in line:
            kind = "success"
        elif "EMERGENCY FLAG ACTIVE" in line:
            kind = "breaker"
        elif " WARN" in line and "breaker" in line.lower():
            kind = "breaker"
        elif "Candidate failed" in line or "Execution failed" in line:
            kind = "failure"
        elif "Denied by local pacing gate" in line:
            kind = "pacing"
        elif "Oracle refresh failed" in line:
            kind = "oracle"
        elif "transaction broadcast" in line:
            kind = "tx"
        elif "[sweep" in line or "Sweep" in line or "refund" in line.lower():
            kind = "treasury"
        if kind:
            tx = TX_RE.search(line)
            self.events.append(
                {
                    "ts": ts,
                    "kind": kind,
                    "tx": tx.group(0) if tx else None,
                    "msg": line[:280],
                }
            )

    @staticmethod
    def _parse_shadow_line(line: str, ts):
        """Extract the per-execution fields from the orchestrator SHADOW line.
        Returns None unless id + venue + expected profit are all present."""
        mid = SWAP_ID_RE.search(line)
        mv = SWAP_VENUE_RE.search(line)
        me = SWAP_EXP_RE.search(line)
        if not (mid and mv and me):
            return None
        try:
            expected = float(me.group(1))
        except ValueError:
            return None
        maom = SWAP_AOM_RE.search(line)
        masset = SWAP_FLASH_ASSET_RE.search(line)
        mamt = SWAP_FLASH_AMT_RE.search(line)
        return {
            "id": mid.group(1),
            "ts": ts,
            "venue": mv.group(1),
            "expected_usd": expected,
            "amount_out_min": maom.group(1) if maom else None,
            "flash_asset": masset.group(1) if masset else None,
            "flash_amount": int(mamt.group(1)) if mamt else None,
        }

    def snapshot(self):
        with self.lock:
            # Collapse consecutive oracle warnings into the latest one + count.
            events = list(self.events)
            collapsed = []
            oracle_count = 0
            for ev in events:
                if ev["kind"] == "oracle":
                    oracle_count += 1
                    if collapsed and collapsed[-1]["kind"] == "oracle":
                        collapsed[-1] = {**ev, "count": oracle_count}
                    else:
                        collapsed.append({**ev, "count": oracle_count})
                else:
                    oracle_count = 0
                    collapsed.append(ev)
            return {
                "lines": list(self.lines)[-TAIL_LINES:],
                "events": collapsed[-40:],
                "latest_block": self.latest_block,
                "latest_block_ts": self.latest_block_ts,
                "run_start_block": self.run_start_block,
                "scan_cycles": self.scan_cycles,
                "oracle_usd": self.oracle_usd,
                "start_iso": self.start_iso,
                "mode": self.mode,
                "chain_id": self.chain_id,
                "topup_wei": self.topup_wei,
                "topup_count": self.topup_count,
                "swap_shadow": dict(self.swap_shadow),
                "route_misses": self.route_miss_count,
                "rotation_reroutes": self.rotation_reroute_count,
                "rotation_denied": self.rotation_denied_count,
            }


# --------------------------------------------------------------------------
# Public ETH/USD ticker (Binance -> Coinbase -> None). Background thread.
# --------------------------------------------------------------------------
class Ticker:
    def __init__(self, interval: float = 8.0):
        self.interval = interval
        self.lock = threading.Lock()
        self.data = None
        self.last_error = None

    def _get_json(self, url: str):
        req = urllib.request.Request(url, headers={"User-Agent": "chimera-dashboard/1.0"})
        with urllib.request.urlopen(req, timeout=4) as r:
            return json.loads(r.read().decode("utf-8", "replace"))

    def _fetch(self):
        try:  # Kraken: full 24h stats, keyless, not geo-blocked
            d = self._get_json("https://api.kraken.com/0/public/Ticker?pair=ETHUSD")
            k = d["result"]["XETHZUSD"]
            last = float(k["c"][0])
            open24 = float(k["o"])
            return {
                "price": last,
                "chg24_pct": (last - open24) / open24 * 100.0 if open24 else None,
                "high24": float(k["h"][1]),
                "low24": float(k["l"][1]),
                "vol24_usd": float(k["v"][1]) * float(k["p"][1]),  # vol x vwap
                "src": "kraken",
            }
        except Exception:  # noqa: BLE001
            pass
        try:  # Binance 24h ticker (geo-blocked in some regions, HTTP 451)
            d = self._get_json("https://api.binance.com/api/v3/ticker/24hr?symbol=ETHUSDT")
            return {
                "price": float(d["lastPrice"]),
                "chg24_pct": float(d["priceChangePercent"]),
                "high24": float(d["highPrice"]),
                "low24": float(d["lowPrice"]),
                "vol24_usd": float(d["quoteVolume"]),
                "src": "binance",
            }
        except Exception:  # noqa: BLE001
            pass
        try:  # Coinbase spot: price only
            d = self._get_json("https://api.coinbase.com/v2/prices/ETH-USD/spot")
            return {
                "price": float(d["data"]["amount"]),
                "chg24_pct": None,
                "high24": None,
                "low24": None,
                "vol24_usd": None,
                "src": "coinbase",
            }
        except Exception as e:  # noqa: BLE001
            self.last_error = f"{type(e).__name__}: {e}"
            return None

    def run(self):
        while True:
            d = self._fetch()
            if d:
                d["fetched_at"] = time.time()
                with self.lock:
                    self.data = d
            time.sleep(self.interval)

    def snapshot(self):
        with self.lock:
            return dict(self.data) if self.data else None


# --------------------------------------------------------------------------
# High-water marks persisted across dashboard restarts (logs/ is gitignored).
# --------------------------------------------------------------------------
def load_hw() -> dict:
    try:
        with open(HW_FILE, "r", encoding="utf-8") as f:
            hw = json.load(f)
        return {
            "max_daily_net_usd": float(hw.get("max_daily_net_usd", 0.0)),
            "max_weekly_net_usd": float(hw.get("max_weekly_net_usd", 0.0)),
            "max_sweep_wei": float(hw.get("max_sweep_wei", 0)),
        }
    except Exception:  # noqa: BLE001
        return {"max_daily_net_usd": 0.0, "max_weekly_net_usd": 0.0, "max_sweep_wei": 0}


def save_hw(hw: dict):
    try:
        tmp = HW_FILE + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(hw, f)
        os.replace(tmp, HW_FILE)
    except Exception:  # noqa: BLE001
        pass


def read_snapshot() -> dict | None:
    try:
        with open(SNAPSHOT_FILE, "r", encoding="utf-8") as f:
            d = json.load(f)
        return {
            "block_number": d.get("block_number"),
            "timestamp": d.get("timestamp"),
            "reserves": len(d.get("reserves", [])),
            "users": len(d.get("users", {})),
        }
    except Exception:  # noqa: BLE001
        return None


def iso_to_epoch(iso: str | None):
    if not iso:
        return None
    try:
        return datetime.strptime(iso, "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc).timestamp()
    except ValueError:
        return None


def _flash_usd(asset: str | None, amount: int | None, eth_px: float | None):
    """Rough USD notional of a flash loan. None when the asset is unknown or
    a WETH-denominated loan has no ETH price available."""
    if not asset or amount is None:
        return None
    info = ASSET_INFO.get(asset.lower())
    if not info:
        return None
    kind, decimals = info
    units = amount / (10 ** decimals)
    if kind == "stable":
        return units
    if kind == "weth" and eth_px:
        return units * eth_px
    return None


def build_swap_state(venues, issues, outcomes, log, eth_px) -> dict:
    """Assemble the swap-layer observability section of /api/state:
    per-venue outcome aggregates, realized-vs-simulated per execution,
    revert counters keyed by route_kind, and routing config lint."""
    kind_by_venue = {v.get("name"): (v.get("router_compatibility") or "unknown") for v in venues}
    liq_by_venue = {}
    for v in venues:
        try:
            liq_by_venue[v.get("name")] = int(v.get("liquidity_usd_min") or 0)
        except ValueError:
            liq_by_venue[v.get("name")] = 0

    # Revert counter keyed by route_kind — the detection net for silent
    # V2/Aerodrome/factory reverts. Venues no longer in config count as unknown.
    reverts_by_kind: dict = {}
    for name, agg in outcomes["by_venue"].items():
        kind = kind_by_venue.get(name, "unknown")
        reverts_by_kind[kind] = reverts_by_kind.get(kind, 0) + agg["reverts"]

    # Join recent outcomes (realized) against SHADOW records (simulated) by id.
    executions = []
    slip_acc: dict = {}  # venue -> [sum_bps, samples]
    for rec in outcomes["recent"]:
        shadow = log["swap_shadow"].get(rec["id"]) if rec.get("id") else None
        expected = shadow["expected_usd"] if shadow else None
        deviation_bps = None
        if expected and expected > 0 and not rec["reverted"]:
            deviation_bps = (expected - rec["realized_usd"]) / expected * 10000.0
            acc = slip_acc.setdefault(rec["venue"], [0.0, 0])
            acc[0] += deviation_bps
            acc[1] += 1
        flash_usd = None
        depth_cap_binding = None
        if shadow:
            flash_usd = _flash_usd(shadow.get("flash_asset"), shadow.get("flash_amount"), eth_px)
            liq = liq_by_venue.get(rec["venue"])
            if flash_usd is not None and liq:
                depth_cap_binding = flash_usd > liq * DEPTH_CAP_FRACTION
        executions.append({
            "id": rec.get("id"),
            "ts": rec.get("ts"),
            "venue": rec["venue"],
            "route_kind": kind_by_venue.get(rec["venue"], "unknown"),
            "expected_usd": expected,
            "realized_usd": rec["realized_usd"],
            "deviation_bps": deviation_bps,
            "reverted": rec["reverted"],
            "flash_usd": flash_usd,
            "depth_cap_binding": depth_cap_binding,
        })

    venue_rows = []
    for v in venues:
        name = v.get("name")
        agg = outcomes["by_venue"].get(
            name, {"executions": 0, "reverts": 0, "realized_net_usd": 0.0}
        )
        acc = slip_acc.get(name)
        venue_rows.append({
            "name": name,
            "chain": v.get("chain"),
            "route_kind": kind_by_venue.get(name, "unknown"),
            "router": v.get("router_address"),
            "liquidity_usd_min": liq_by_venue.get(name, 0),
            "executions": agg["executions"],
            "reverts": agg["reverts"],
            "realized_net_usd": agg["realized_net_usd"],
            "avg_slippage_bps": (acc[0] / acc[1]) if acc else None,
            "slippage_samples": acc[1] if acc else 0,
        })

    return {
        "venues": venue_rows,
        "reverts_by_route_kind": reverts_by_kind,
        "recent_executions": executions[-20:],
        "config_lint": {
            "issues": issues,
            "router_factory_detected": any(i["kind"] == "router_is_factory" for i in issues),
        },
        "route_resolution": {
            "misses": log["route_misses"],
            "rotation_reroutes": log["rotation_reroutes"],
            "rotation_denied": log["rotation_denied"],
        },
        "outcomes_total": outcomes["total"],
        "outcomes_parse_errors": outcomes["parse_errors"],
        "note": "shadow mode records realized==expected, so deviation_bps reads 0 by "
                "construction; deviations become meaningful in live mode",
    }


TAILER = LogTailer()
TICKER = Ticker()
ROUTING = RoutingView()
OUTCOMES = OutcomesTailer()
HW = load_hw()

# Engine metrics health hysteresis: the metrics socket flakes (10053 race,
# see engine_fetch), so a single failed poll must not blank the UI.
# status: "up" (fresh) -> "degraded" (1-2 consecutive fails, cached metrics)
#         -> "down" (3+ fails, or never succeeded)
ENGINE_STATE = {"fails": 0, "metrics": {}, "last_ok": 0.0, "error": None}
FAILS_FOR_DOWN = 3


def build_state() -> dict:
    global HW
    now = time.time()
    TAILER.poll()
    log = TAILER.snapshot()

    metrics_text, metrics_err = engine_fetch("/metrics")
    if metrics_text is not None:
        ENGINE_STATE["fails"] = 0
        ENGINE_STATE["metrics"] = parse_prometheus(metrics_text)
        ENGINE_STATE["last_ok"] = now
        ENGINE_STATE["error"] = None
        engine_status = "up"
    else:
        ENGINE_STATE["fails"] += 1
        ENGINE_STATE["error"] = metrics_err
        if ENGINE_STATE["fails"] >= FAILS_FOR_DOWN or not ENGINE_STATE["metrics"]:
            engine_status = "down"
        else:
            engine_status = "degraded"
    metrics = dict(ENGINE_STATE["metrics"])

    ticker = TICKER.snapshot()
    snap = read_snapshot()
    ROUTING.poll()
    OUTCOMES.poll()
    routing_venues, routing_issues = ROUTING.snapshot()
    outcome_stats = OUTCOMES.snapshot()

    start_epoch = iso_to_epoch(log["start_iso"])
    block_epoch = iso_to_epoch(log["latest_block_ts"])
    uptime_s = (now - start_epoch) if start_epoch else None

    # Activity: 1 NewBlock = 1 scan cycle (oracle refresh ->
    # detector.find_at_risk_positions -> candidates -> sims; orchestrator.rs).
    blocks_processed = None
    if log["run_start_block"] is not None and log["latest_block"] is not None:
        blocks_processed = max(0, log["latest_block"] - log["run_start_block"] + 1)
    blocks_per_min = None
    if blocks_processed is not None and uptime_s and uptime_s > 0:
        blocks_per_min = blocks_processed / (uptime_s / 60.0)
    sims_success = metrics.get("chimera_sims_run_total_result_success", 0.0)
    sims_failure = metrics.get("chimera_sims_run_total_result_failure", 0.0)

    wei_to_eth = 1e18
    sweep_eth = metrics.get("chimera_sweep_amount_wei", 0.0) / wei_to_eth
    l1_fee_eth = metrics.get("chimera_l1_fee_wei", 0.0) / wei_to_eth
    topups_eth = log["topup_wei"] / wei_to_eth

    # High-water tracking (persisted on change)
    daily = metrics.get("chimera_daily_net_usd", 0.0)
    weekly = metrics.get("chimera_weekly_net_usd", 0.0)
    sweep_w = metrics.get("chimera_sweep_amount_wei", 0.0)
    changed = False
    if daily > HW["max_daily_net_usd"]:
        HW["max_daily_net_usd"] = daily
        changed = True
    if weekly > HW["max_weekly_net_usd"]:
        HW["max_weekly_net_usd"] = weekly
        changed = True
    if sweep_w > HW["max_sweep_wei"]:
        HW["max_sweep_wei"] = sweep_w
        changed = True
    if changed:
        save_hw(HW)

    breaker_metric = metrics.get("chimera_breaker_state", 0.0)
    breaker_banner = bool(breaker_metric >= 1) or any(
        e["kind"] == "breaker" for e in log["events"][-10:]
    )

    live_px = ticker["price"] if ticker else None
    oracle_px = log["oracle_usd"]
    divergence_pct = None
    if live_px and oracle_px:
        divergence_pct = (live_px - oracle_px) / oracle_px * 100.0

    spent_eth_est = l1_fee_eth + topups_eth
    spent_usd_est = spent_eth_est * live_px if live_px else None

    return {
        "now": now,
        "dashboard": {"port": PORT, "engine_port": ENGINE_PORT},
        "engine": {
            "reachable": engine_status != "down",
            "status": engine_status,
            "error": ENGINE_STATE["error"],
            "consecutive_fails": ENGINE_STATE["fails"],
            "last_ok_age_s": (now - ENGINE_STATE["last_ok"]) if ENGINE_STATE["last_ok"] else None,
            "metrics": metrics,
        },
        "mode": {
            "execute_mode": log["mode"] or "unknown",
            "chain_id": log["chain_id"] or 8453,
            "started_at": log["start_iso"],
            "uptime_s": uptime_s,
        },
        "activity": {
            "blocks_processed": blocks_processed,
            "blocks_per_min": blocks_per_min,
            "run_start_block": log["run_start_block"],
            "scan_cycles_tail": log["scan_cycles"],
            "reserves_tracked": snap["reserves"] if snap else None,
            "positions_tracked": snap["users"] if snap else None,
            "candidates_seen": metrics.get("chimera_candidates_seen_total", 0.0),
            "sims_success": sims_success,
            "sims_failure": sims_failure,
        },
        "block": {
            "latest": log["latest_block"],
            "at": log["latest_block_ts"],
            "age_s": (now - block_epoch) if block_epoch else None,
        },
        "snapshot": snap,
        "ticker": {
            "live": ticker,
            "oracle_usd": oracle_px,
            "divergence_pct": divergence_pct,
        },
        "financials": {
            "daily_net_usd": daily,
            "weekly_net_usd": weekly,
            "sweep_eth": sweep_eth,
            "l1_fee_eth": l1_fee_eth,
            "topups_eth_recent": topups_eth,
            "topups_count_recent": log["topup_count"],
            "spent_eth_est": spent_eth_est,
            "spent_usd_est": spent_usd_est,
            "gained_usd": daily,
            "net_est_usd": (daily - spent_usd_est) if spent_usd_est is not None else None,
            "high_water": HW,
        },
        "tranche": {
            "enabled": bool(metrics.get("chimera_tranche_enabled", 0.0)),
            "bundles_total": int(metrics.get("chimera_tranche_bundles_total", 0.0)),
            "bundles_confirmed": int(metrics.get("chimera_tranche_bundles_confirmed_total", 0.0)),
            "bundles_reverted": int(metrics.get("chimera_tranche_bundles_reverted_total", 0.0)),
            "profit_wei": metrics.get("chimera_tranche_profit_wei", 0.0),
            "gas_wei": metrics.get("chimera_tranche_gas_spent_wei", 0.0),
            "relay": "https://rpc.flashbots.net",
        },
        "swap": build_swap_state(
            routing_venues, routing_issues, outcome_stats, log, live_px or oracle_px
        ),
        "events": log["events"],
        "tail": log["lines"],
        "breaker_banner": breaker_banner,
    }


# --------------------------------------------------------------------------
# HTTP layer
# --------------------------------------------------------------------------
class Handler(BaseHTTPRequestHandler):
    server_version = "ChimeraDashboard/1.0"

    def log_message(self, fmt, *args):  # quiet access log
        return

    def _send(self, code: int, body: bytes, ctype: str):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def _json(self, obj, code: int = 200):
        self._send(code, json.dumps(obj).encode("utf-8"), "application/json")

    def do_GET(self):  # noqa: N802 - stdlib naming
        path = self.path.split("?", 1)[0]
        if path in ("/", "/index.html"):
            try:
                with open(HTML_FILE, "rb") as f:
                    self._send(200, f.read(), "text/html; charset=utf-8")
            except OSError:
                self._send(500, b"dashboard.html missing", "text/plain")
        elif path == "/api/state":
            try:
                self._json(build_state())
            except Exception as e:  # noqa: BLE001 - the page must stay alive
                self._json({"error": f"{type(e).__name__}: {e}"}, code=500)
        elif path == "/metrics":
            body, err = engine_fetch("/metrics")
            if body is None:
                self._send(502, f"engine unreachable: {err}\n".encode(), "text/plain")
            else:
                self._send(200, body.encode("utf-8"), "text/plain; version=0.0.4")
        elif path == "/health":
            _, err = engine_fetch("/metrics")
            self._json(
                {
                    "dashboard": "up",
                    "engine_reachable": err is None,
                    "engine_status": (
                        "up" if err is None
                        else ("down" if ENGINE_STATE["fails"] >= FAILS_FOR_DOWN else "degraded")
                    ),
                    "engine_error": err,
                    "engine_port": ENGINE_PORT,
                    "time": time.time(),
                }
            )
        else:
            self._send(404, b"not found", "text/plain")


def main():
    t = threading.Thread(target=TICKER.run, name="ticker", daemon=True)
    t.start()
    srv = ThreadingHTTPServer((BIND, PORT), Handler)
    print(f"[dashboard] http://{BIND}:{PORT}  (proxying engine 127.0.0.1:{ENGINE_PORT})")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
