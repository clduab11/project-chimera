# Project Chimera — Operator Manual

**Version**: 1.1
**Last Updated**: 2026-07-05
**Audience**: Solo operator running the sovereign L2 MEV engine.
**Time Commitment**: 10–20 minutes daily.

> **Note:** This document reflects a pre-implementation operator workflow.
> The current binary uses host-run deployment with metrics on port
> `9100 + (chain_id % 1000)` (e.g. `:9553` for Base, chain_id=8453).
> See `docs/runbook-7day-soak.md` and `docs/runbook-keystore-multisig-go-live.md`
> for updated operator procedures.

---

## 1. Pre-Flight Checklist (Before Every Session)

Run this checklist before starting the engine or resuming from any pause.

### 1.1 Configuration Validation

```bash
# In WSL2 Ubuntu
cd ~/chimera

# 1. Verify pacing config loads without error
cargo test -p chimera-core test_load_valid_config
# Expected: OK, with values: daily=$2000, weekly=$7500, single=$1000, mode=shadow

# 2. Confirm execute_mode is what you expect
cat config/pacing.yaml | grep execute_mode
# Expected: shadow (unless you have explicitly completed the 7-day live transition)
```

**Critical**: If `execute_mode` reads `live` and you did not intentionally change it, stop immediately. See [Emergency Procedures](emergency-procedures.md).

### 1.2 RPC Health Check

```bash
# Test Base RPC connectivity
curl -X POST https://mainnet.base.org \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'
# Expected: {"jsonrpc":"2.0","id":1,"result":"0x..."} (non-null hex block number)

# Test Arbitrum RPC connectivity
curl -X POST https://arb1.arbitrum.io/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'
```

**Action if failing**: Switch to fallback RPC in `config/routing.yaml` or pause the engine until connectivity is restored.

### 1.3 Wallet Balance Check

```bash
# Check EOA pool balances. Default --min-balance is 0.0; pass an explicit threshold:
python scripts/check_balances.py --chain base --min-balance 0.01
# Expected: All 10 addresses show ≥ 0.01 ETH
```

**Minimums**:
- **Base**: Each EOA ≥ 0.01 ETH (~$35 at current prices). Total pool: ≥ 0.1 ETH.
- **Arbitrum**: Each EOA ≥ 0.005 ETH (~$18). Total pool: ≥ 0.05 ETH.

**Action if low**: Fund from your cold wallet. Never fund from a CEX directly — use an intermediate wallet.

### 1.4 Keystore Integrity

```bash
# Verify encrypted keystore is readable and contains 10 keys
ls -la encrypted_keystore/
# Expected: 10 .json files + keystore.meta

# Check keystore decryption (test-only, does not expose keys)
cargo test --test keystore_integrity
# Expected: OK, 10 keys loaded
```

### 1.5 State File Check

```bash
# Ensure the outcomes audit trail exists and is recent
ls -la core/state/outcomes.jsonl
# Expected: File exists, modified within last 24h

# Verify JSON is valid (first line parsed as sample)
head -n1 core/state/outcomes.jsonl | python -m json.tool
# Expected: valid JSON object
```

---

## 2. Daily Operator Checklist (10–20 Minutes)

Perform this routine once per day, ideally at the same local time to establish operator pattern regularity.

### 2.1 Grafana Dashboard Review (5 min)

Open `http://localhost:3002` in your browser.

**Panels to check**:

| Panel | Expected Value | Action if Abnormal |
|-------|---------------|-------------------|
| `chimera_breaker_state` | `0` (green) | If `1` (red), see [Emergency Procedures §3](emergency-procedures.md) |
| `chimera_candidates_seen_total` | >0 per day | If `0` for >24h, check RPC + snapshot freshness |
| `chimera_sims_run_total` (success) | Increasing slowly | If flat, check detector output |
| `chimera_sim_latency_seconds` (p99) | <100ms | If >500ms, REVM DB may be cold; run pre-warm |
| `chimera_profit_usd` | $0–$800 per execution | If negative, breaker should have tripped; investigate |
| `chimera_revert_total` | <3 consecutive | If approaching 3, pause and investigate calldata |
| `chimera_gas_used` | 150k–500k per tx | If >1M, contract path may be inefficient |
| `chimera_l1_fee_wei` | Stable, low variance | If spiking 3–5x, blob congestion; consider pausing |

**Screenshot**: Take a screenshot of the main dashboard and save to `logs/screenshots/YYYY-MM-DD_grafana.png`. Useful for post-mortems.

### 2.2 Log Review (5 min)

```bash
# Tail recent logs (tracing_appender convention: logs/chimera.log.YYYY-MM-DD)
tail -n 100 logs/chimera.log.$(date +%Y-%m-%d)
```

**Keywords to scan for**:

| Keyword | Meaning | Action |
|---------|---------|--------|
| `BREAKER:` | Circuit breaker tripped | Immediate: see Emergency Procedures |
| `PACING: DENIED` | Opportunity blocked by cap/gate | Normal if daily cap approaching limit |
| `PACING + SIM PATH: ALLOWED` | Execution authorized | Confirm outcome was recorded |
| `Using heuristic profit estimate` | Simulator fell back to heuristic | Review: oracle may be stale |
| `OPERATOR: Manually clearing breaker` | Human intervention logged | Ensure root cause was resolved |
| `SimulationFailed` | REVM reverted | Check calldata + block env |
| `FeeQueryError` | L1 fee query failed | RPC issue; switch provider |

**Log Location**:
- Default: `logs/chimera.log.YYYY-MM-DD` (tracing_appender daily rotation convention)
- Format: Structured JSON, one event per line
- Retention: Rotate weekly; archive to cold storage monthly

### 2.3 Pools & Venues Update (5 min)

```bash
# Review current venue config
cat config/routing.yaml | grep -A5 venues:

# Update if any venue has < $50k liquidity or added KYC
# Edit manually (v1) or run:
python scripts/update_venues.py --check-liquidity
```

**Weekly task**: Update `config/pools.toml` (or regenerate via script) to reflect:
- New Aave V3 reserves with sufficient liquidity
- Removed or paused markets
- Changed liquidation bonus / threshold parameters

**Never** add a venue without:
1. Liquidity verification (> $50k USD)
2. Non-KYC confirmation
3. A test simulation in shadow mode

### 2.4 Metrics Server Health

```bash
# Verify Prometheus endpoint is responding (port is 9100 + (chain_id % 1000))
# Example for Base (chain_id 8453 → port 9553):
curl http://localhost:9553/metrics | head -n 20
# Expected: HTTP 200, lines starting with `# HELP` and metric names
```

If the metrics server is down, the Grafana dashboard will show stale data. Restart with:

```bash
cargo run --bin chimera
```

### 2.5 Disk & System Health

```bash
# Check disk space (snapshots + logs grow quickly)
df -h
# Expected: >5GB free on the partition containing logs/ and snapshots/

# Check memory (REVM simulations are RAM-intensive)
free -h
# Expected: >4GB available during normal operation
```

---

## 3. Shadow Mode vs Live Mode

### 3.1 Shadow Mode (Default — Required for First 7 Days)

In shadow mode, the engine runs the full pipeline **except** the final on-chain submission.

**What happens**:
- Detector finds at-risk positions.
- Simulator runs full REVM replay.
- Pacing engine evaluates and logs decisions.
- **No transaction is submitted.**
- Outcomes are compared against actual on-chain liquidations (if any).

**Why 7 days minimum**:
- Validates simulation accuracy against real market conditions.
- Calibrates profit multiplier and gas heuristics.
- Builds confidence in breaker behavior.
- Establishes baseline metrics for your specific RPC latency and block times.

**Shadow mode validation checklist** (must pass before going live):

- [ ] ≥50 simulation runs completed with plausible candidates
- [ ] Zero unexpected breaker trips (trips should only occur on manual test)
- [ ] Simulated profit within 10% of actual on-chain liquidations (tracked manually or via script)
- [ ] All 10 EOA wallets funded and balance-checked
- [ ] Operator has manually tripped and cleared a test breaker at least once
- [ ] Grafana dashboard shows clean, readable metrics for 7 consecutive days
- [ ] `chimera_revert_total` remains at 0 (no on-chain reverts possible in shadow)

### 3.2 Transitioning to Live Mode

**Step 1**: Use `toggle_shadow.py` to set live mode:

```bash
python scripts/toggle_shadow.py --set-live
```

This enforces the 7-day shadow rule and writes the mode transition to `core/state/mode.json`.

**Step 2**: Edit `config/pacing.yaml`

```yaml
execute_mode: live
```

**Step 3**: Reload config (requires restart in v1)

```bash
# Stop current instance
pkill chimera

# Restart with live mode
cargo run --release --bin chimera
```

**Step 4**: Verify live mode is active

```bash
tail -f logs/chimera.log.$(date +%Y-%m-%d) | grep execute_mode
# Expected: "execute_mode: live"
```

**Step 5**: First live execution watch

- Stay at the terminal for the first 1–2 hours.
- Monitor Grafana and logs in real time.
- Confirm the first execution produces the expected `LiquidationCall` event on BaseScan/Arbiscan.
- Verify profit landed in the expected EOA.

**Rollback to shadow** (immediate, no restart required in future versions; v1 requires restart):

```bash
# Set shadow via toggle script
python scripts/toggle_shadow.py --set-shadow
# Edit config back to shadow
echo "execute_mode: shadow" > config/pacing.yaml
# Restart engine
pkill chimera && cargo run --release --bin chimera
```

---

## 4. Monitoring Endpoints & What They Mean

### 4.1 Prometheus Metrics (port = `9100 + (chain_id % 1000)`)

| Endpoint | Method | Description |
|----------|--------|-------------|
| `http://localhost:<PORT>/metrics` | GET | Full Prometheus text dump |

**Key metric families** (all prefixed `chimera_`):

- `chimera_candidates_seen_total{chain="base"}` — How many at-risk positions the detector identified. A sustained drop to zero suggests RPC or snapshot issues.
- `chimera_sims_run_total{result="success"}` — Successful simulations. Should increase slowly.
- `chimera_sims_run_total{result="denied"}` — Opportunities blocked by pacing. High values are normal and desired (means caps are working).
- `chimera_sim_latency_seconds_bucket{le="0.05"}` — Percentage of simulations completing under 50ms. Target: >90% under 100ms.
- `chimera_profit_usd_bucket{le="50.0"}` — Distribution of estimated profits. Most profitable liquidations on L2 are $10–$200.
- `chimera_revert_total{reason="<value>"}` — Revert counter with reason label. Verify actual reason values at runtime; the `insufficient_profit` and `execution_revert` values may vary from spec.
- `chimera_breaker_state{chain="base"}` — `0` = safe, `1` = tripped. This is your most important gauge.
- `chimera_gas_used_bucket{le="200000.0"}` — Most liquidations consume 150k–300k gas. Spikes indicate complex multi-asset positions.
- `chimera_l1_fee_wei` — Current L1 data fee estimate. Multiply by 1.15x buffer before profit check.
- `chimera_daily_net_usd{chain="base"}` — Rolling 24h net USD.
- `chimera_weekly_net_usd{chain="base"}` — Rolling 7d net USD.
- `chimera_sweep_total{type="<type>"}` — Total sweep/refund operations.
- `chimera_sweep_skipped_breaker{type="<type>"}` — Sweeps skipped due to breaker.
- `chimera_sweep_amount_wei` — Last sweep amount in wei.

**Metrics that do NOT exist** (remove from any dashboard or alert rule):
- `chimera_txs_submitted_total` — not exported
- `chimera_txs_confirmed_total` — not exported

### 4.2 Grafana Dashboard (`:3002`)

**Default panels** (import from `grafana/dashboard.json` if available):

1. **System Health** — Breaker state, RPC lag, sequencer stall flags.
2. **Throughput** — Candidates/hour, simulations/hour, executions/hour.
3. **Financial** — Daily net, weekly net, daily loss (ETH), profit distribution.
4. **Gas & Fees** — L2 gas price, L1 fee, total cost per tx.
5. **Latency** — p50/p99 simulation time, RPC round-trip time.

**Alert rules** (configure in Grafana, not yet automated in v1):

| Condition | Severity | Action |
|-----------|----------|--------|
| `chimera_breaker_state == 1` | Critical | Investigate immediately; do not clear until root cause found |
| `chimera_revert_total` increases by 1 in 10 min | Warning | Review logs; prepare to pause |
| `chimera_l1_fee_wei` > 5x baseline | Warning | Consider pausing; blob congestion may make liquidations unprofitable |
| `chimera_candidates_seen_total` == 0 for 1h | Info | Check snapshot freshness and RPC connectivity |
| Disk usage > 90% | Warning | Rotate logs; snapshots may be filling disk |

### 4.3 Log Locations & How to Read Them

**Primary log**: `logs/chimera.log.YYYY-MM-DD`

Format (one JSON object per line):

```json
{
  "timestamp": "2026-07-05T12:34:56Z",
  "level": "INFO",
  "target": "chimera::pacing",
  "fields": {
    "id": "liq-demo-001",
    "realized": "62.00",
    "daily": "124.50",
    "weekly": "124.50",
    "reverts": 0,
    "breaker": null
  },
  "message": "Outcome recorded"
}
```

**Outcomes audit trail**: `core/state/outcomes.jsonl`

Format (JSONL, one JSON object per line, appended on each outcome).

**Snapshot files**: `core/snapshots/{chain}_latest.json`

Generated by `scripts/snapshot_generator.py`. Contains reserve data and user positions. Review if detector output seems stale.

**How to search logs**:

```bash
# Find all breaker events
grep "BREAKER" logs/chimera.log.*.log

# Find all denied opportunities for a specific reason
grep -A2 "PACING: DENIED" logs/chimera.log.*.log | grep "reason"

# Find simulation failures
grep "SimulationFailed" logs/chimera.log.*.log

# Pretty-print a single log line
head -n1 logs/chimera.log.2026-07-05 | python -m json.tool
```

---

## 5. Expected Metrics Ranges

These are **normal operating ranges** for a healthy Chimera instance in shadow or live mode. Deviation from these ranges is not automatically an emergency, but warrants investigation.

### 5.1 Throughput

| Metric | Normal Range | Notes |
|--------|-------------|-------|
| Candidates seen / day | 5–50 | Depends on market volatility; spikes during large price drops |
| Simulations run / day | 5–50 | One per candidate; may be lower if pacing gates deny early |
| Executions / day | 1–4 | Capped by daily limit and 6h minimum interval |
| Executions / week | 7–20 | Weekly cap ($5,000) is the hard ceiling |

### 5.2 Financial

| Metric | Normal Range | Notes |
|--------|-------------|-------|
| Daily net USD | $0–$1,666 | Hard cap; operator should never see >$1,666 |
| Weekly net USD | $0–$5,000 | Hard cap |
| Single execution profit | $10–$800 | Most profitable liquidations are $20–$200 on L2 |
| Daily loss (ETH) | $0–$0.005 | Hard cap; includes gas + negative outcomes |
| Profit multiplier | 2.5x–10x | Minimum 2.5x; higher is normal in calm markets |

### 5.3 Gas & Fees (Base)

| Metric | Normal Range | Notes |
|--------|-------------|-------|
| L2 gas used | 150,000–400,000 | Depends on collateral/debt asset count |
| L2 gas price (gwei) | 0.001–0.1 | Base is cheap; spikes to 0.5 during congestion |
| L1 data fee (wei) | 500–5,000 | Scales with blob market; 1.15x buffer applied |
| Total cost per tx | $0.50–$5.00 | L2 execution is cheap; L1 data fee dominates |

### 5.4 Gas & Fees (Arbitrum)

| Metric | Normal Range | Notes |
|--------|-------------|-------|
| L2 gas used | 150,000–400,000 | Similar to Base |
| L2 gas price (gwei) | 0.01–0.1 | Slightly higher than Base |
| L1 data fee (wei) | 1,000–10,000 | Arbitrum L1 fee is higher than OP Stack |
| Total cost per tx | $1.00–$8.00 | L1 data fee is the dominant cost |

### 5.5 Latency

| Metric | Normal Range | Notes |
|--------|-------------|-------|
| Simulation latency (p50) | 10–50ms | Cold DB: 100–500ms; warm DB: 10–50ms |
| Simulation latency (p99) | 50–200ms | Spikes to 500ms+ during snapshot reload |
| RPC round-trip | 50–200ms | Depends on provider and geography |
| End-to-end (detect → submit) | 500ms–3s | Must be < block time (2s Base, ~0.25s Arb) |

### 5.6 Breaker State

| State | Frequency | Action |
|-------|----------|--------|
| `0` (OK) | 99%+ of uptime | None |
| `1` (Tripped) | Rare | See Emergency Procedures |

**Target uptime**: 99.5% (breaker-tripped time is considered downtime).

---

## 6. Weekly Tasks

In addition to the daily checklist, perform these tasks once per week:

1. **Rotate snapshots**: Delete `core/snapshots/*.json` older than 7 days to save disk space.
2. **Review Grafana**: Check the weekly financial panel. Confirm weekly net is within $5,000.
3. **Audit logs**: Search for any `SimulationFailed` or `FeeQueryError` events. File a bug if reproducible.
4. **Update venues**: Run `python scripts/update_venues.py` or manually verify liquidity thresholds.
5. **Keystore backup**: Copy `encrypted_keystore/` to an offline encrypted USB drive. Test decryption.
6. **Dependency check**: Run `cargo outdated` (if installed) or review `Cargo.lock` for critical security updates.
7. **Profit reconciliation**: Compare Grafana daily net against on-chain balance changes (manual in v1).

---

## 7. Monthly Tasks

1. **Full system restart**: Stop the engine, archive `core/state/outcomes.jsonl`, and restart to verify cold-boot behavior.
2. **EOA rotation**: Generate a new pool of 10 clean wallets and update `encrypted_keystore/`.
3. **Config review**: Re-evaluate caps against actual performance. Do not increase without 30 days of shadow data.
4. **Disaster recovery drill**: Restore `outcomes.jsonl` from backup and confirm engine resumes correctly.

---

## 8. Related Documentation

- [`README.md`](../README.md) — Quick start, financial guardrails, "Never" rules
- [`docs/architecture.md`](architecture.md) — Component diagrams, data flow, security model
- [`docs/emergency-procedures.md`](emergency-procedures.md) — Breaker tripping, recovery, state restoration
- [`docs/testing-strategy-liquidations.md`](testing-strategy-liquidations.md) — Simulation accuracy plan
