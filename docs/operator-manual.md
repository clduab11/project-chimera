# Project Chimera — Operator Manual

**Version**: 1.2
**Last Updated**: 2026-07-10
**Status**: Current implemented standalone-Executor workflow; shadow-first and operator-gated.
**Audience**: Solo operator running the sovereign L2 MEV engine.
**Time Commitment**: 10–20 minutes daily.

> The current binary uses host-run deployment with metrics on port
> `9100 + (chain_id % 1000)` (e.g. `:9553` for Base, chain_id=8453).
> See `docs/runbook-7day-soak.md` and `docs/runbook-keystore-multisig-go-live.md`
> for the detailed soak and custody procedures. Live transactions are broadcast
> through the configured standard RPC; private/protected submission is not wired.

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

# 2. Confirm the checked-in default remains shadow
grep execute_mode config/pacing.yaml
# Expected: execute_mode: shadow

# 3. Inspect the deployment-local override used by this shell/service
printf 'CHIMERA_EXECUTE_MODE=%s\n' "${CHIMERA_EXECUTE_MODE:-<unset: checked-in shadow default applies>}"
```

**Critical**: Keep checked-in `config/pacing.yaml` in shadow mode. If the effective deployment-local `CHIMERA_EXECUTE_MODE` is `live` and you did not intentionally complete the 7-day transition, stop immediately. See [Emergency Procedures](emergency-procedures.md).

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
# Check every active, non-excluded EOA-pool worker through a real RPC.
# Omitting --rpc makes this script report offline/mock balances.
python scripts/check_balances.py --rpc "$BASE_RPC_URL" \
  --eoa-pool "$CHIMERA_EOA_POOL_PATH" --chain base --min-balance 0.01
# Expected: Every active, non-excluded worker shows ≥ 0.01 ETH.
```

**Minimums**:
- **Base**: Each active, non-excluded EOA ≥ 0.01 ETH and never below the deployment's configured `min_worker_balance_eth`.
- **Arbitrum**: Each active, non-excluded EOA ≥ 0.005 ETH and never below the deployment's configured `min_worker_balance_eth`.

**Action if low**: Fund from your cold wallet. Never fund from a CEX directly — use an intermediate wallet.

### 1.4 Keystore Integrity

```bash
# Confirm all deployment-local custody inputs exist without printing secrets.
test -n "${CHIMERA_TREASURY_KEYSTORE:-}"
test -n "${CHIMERA_WORKER_KEYSTORE_DIR:-}"
test -n "${CHIMERA_EOA_POOL_PATH:-}"
test -n "${CHIMERA_KEYSTORE_PASSWORD:-}"
test -r "$CHIMERA_TREASURY_KEYSTORE"
test -d "$CHIMERA_WORKER_KEYSTORE_DIR"
test -r "$CHIMERA_EOA_POOL_PATH"
python -m json.tool "$CHIMERA_EOA_POOL_PATH" >/dev/null

# Decrypt in memory and verify every active, non-excluded pool worker has a
# matching deployment-local worker keystore. No private key is printed.
python scripts/provision_wallets.py --verify --eoa-pool "$CHIMERA_EOA_POOL_PATH"
# Expected: Verify OK for every non-excluded, non-zero pool address.
```

Never echo `CHIMERA_KEYSTORE_PASSWORD`, pass it as a command-line argument, or place it in the repository. Live startup separately decrypts the treasury keystore and fails closed on signer/address or role mismatches.

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
- [ ] Every active, non-excluded EOA-pool worker is funded and balance-checked
- [ ] Operator has manually tripped and cleared a test breaker at least once
- [ ] Grafana dashboard shows clean, readable metrics for 7 consecutive days
- [ ] `chimera_revert_total` remains at 0 (no on-chain reverts possible in shadow)

### 3.2 Transitioning to Live Mode

**Step 1**: Use `toggle_shadow.py` to set live mode:

```bash
python scripts/toggle_shadow.py --set-live
```

This enforces the 7-day shadow rule and writes the mode transition to `core/state/mode.json`.

**Step 2**: Set live mode only in the protected deployment-local environment (shell, service `EnvironmentFile`, or secret manager):

```bash
export CHIMERA_EXECUTE_MODE=live
```

Do not change or overwrite checked-in `config/pacing.yaml`; it must remain `execute_mode: shadow`.

**Step 3**: Complete the standalone Executor and signer preflight before starting live execution:

```bash
# Require deployed standalone Executor bytecode and the canonical Aave Pool.
cast code "$CHIMERA_EXECUTOR_ADDRESS" --rpc-url "$BASE_RPC_URL"
cast call "$CHIMERA_EXECUTOR_ADDRESS" "pool()(address)" --rpc-url "$BASE_RPC_URL"

# Repeat for every active, non-excluded address in the deployment-local EOA pool.
cast call "$CHIMERA_EXECUTOR_ADDRESS" "isWorker(address)(bool)" \
  <ACTIVE_WORKER_ADDRESS> --rpc-url "$BASE_RPC_URL"

# Verify EOA-pool/signer parity, then native gas funding.
python scripts/provision_wallets.py --verify --eoa-pool "$CHIMERA_EOA_POOL_PATH"
python scripts/check_balances.py --rpc "$BASE_RPC_URL" \
  --eoa-pool "$CHIMERA_EOA_POOL_PATH" --chain base --min-balance 0.01
```

Require non-empty Executor bytecode, exact `pool()` equality, `isWorker(address) == true` for every active non-excluded worker, treasury signer/address parity, one decrypted signer per active non-excluded EOA-pool worker, a treasury native balance of at least the configured `refund_topup_eth`, and every active non-excluded worker at or above `min_worker_balance_eth`. Live startup checks these conditions and must fail closed. Explicitly accept the standard-RPC/public-orderflow risk or add and validate protected submission before real-money operation.

**Step 4**: Reload config (requires restart)

```bash
# Stop current instance
pkill chimera

# Restart with live mode
cargo run --release --bin chimera
```

**Step 5**: Verify live mode is active

```bash
tail -f logs/chimera.log.$(date +%Y-%m-%d) | grep execute_mode
# Expected: "execute_mode: live"
```

**Step 6**: First live execution watch

- Stay at the terminal for the first 1–2 hours.
- Monitor Grafana and logs in real time.
- Confirm the first execution produces the expected `LiquidationCall` event on BaseScan/Arbiscan.
- Verify the first debt-token profit remains in Executor, then withdraw it through the owner multisig with `withdraw(token, amount)`; profit is not expected in the worker EOA.

**Rollback to shadow** (restart required):

```bash
# Set shadow via toggle script
python scripts/toggle_shadow.py --set-shadow
# Set the deployment-local override back to shadow (also update the service env file/secret manager).
export CHIMERA_EXECUTE_MODE=shadow
# Restart engine
pkill chimera && cargo run --release --bin chimera
```

Never overwrite `config/pacing.yaml` during rollback; preserve its complete checked-in shadow configuration.

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
| Executions / week | 7–20 | Weekly cap ($7,500) is the hard ceiling |

### 5.2 Financial

| Metric | Normal Range | Notes |
|--------|-------------|-------|
| Daily net USD | $0–$2,000 | Hard cap; operator should never see >$2,000 |
| Weekly net USD | $0–$7,500 | Hard cap |
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
2. **Review Grafana**: Check the weekly financial panel. Confirm weekly net is within $7,500.
3. **Audit logs**: Search for any `SimulationFailed` or `FeeQueryError` events. File a bug if reproducible.
4. **Update venues**: Run `python scripts/update_venues.py` or manually verify liquidity thresholds.
5. **Keystore backup**: Back up the deployment-local `$CHIMERA_TREASURY_KEYSTORE` and `$CHIMERA_WORKER_KEYSTORE_DIR` to offline encrypted media, never inside the repository. Test restoration and rerun `python scripts/provision_wallets.py --verify --eoa-pool "$CHIMERA_EOA_POOL_PATH"` without exposing keys.
6. **Dependency check**: Run `cargo outdated` (if installed) or review `Cargo.lock` for critical security updates.
7. **Profit reconciliation**: Compare Grafana daily net against Executor token balances and owner-multisig withdrawals. Rust `SweepScheduler` manages native worker gas only; Executor token profit uses owner-multisig `withdraw(token, amount)`. `scripts/sweep_profits.py` is a legacy/manual raw-key helper, not the primary profit path.

---

## 7. Monthly Tasks

1. **Full system restart**: Stop the engine, archive `core/state/outcomes.jsonl`, and restart to verify cold-boot behavior.
2. **EOA rotation**: Rotate the configured active worker set using deployment-local `$CHIMERA_WORKER_KEYSTORE_DIR` and `$CHIMERA_EOA_POOL_PATH`; verify every active, non-excluded worker, then have the Executor-owner multisig revoke retired workers and authorize replacements.
3. **Config review**: Re-evaluate caps against actual performance. Do not increase without 30 days of shadow data.
4. **Disaster recovery drill**: Restore `outcomes.jsonl` from backup and confirm engine resumes correctly.

---

## 8. Live Detection Refresh (Snapshot + Prices)

Detection state is refreshed on two axes; both retain last-known-good on any failure and
never regress the in-memory snapshot.

### 8.1 Live repricing (engine-internal, seconds cadence)

The engine refreshes every reserve's USD price from the Aave oracle (the same source the
protocol's `liquidationCall` uses) in ONE batched `eth_call` per interval, then re-evaluates
every tracked position against the 1.05 HF threshold. Knobs in `config/risk.yaml`:

```yaml
price_refresh_secs: 8        # min seconds between refreshes; failures back off to <= 60s
price_max_stale_secs: 300    # past this age: shadow warns; LIVE skips candidate emission
```

No operator action needed; repricing pauses automatically while the emergency flag is active.

### 8.2 Discovery refresh (operator-scheduled, ~15 min cadence)

Discovery of NEW at-risk borrowers requires regenerating `config/snapshot.json`. The engine
only watches the file (the generator's temp+`os.replace` write is atomic); a changed file is
validated (parse, chain match, non-empty reserves, block number must not go backwards or be
zero) and swapped in within one scan, and the simulator's fork DB is rebuilt from it.

Schedule the generator externally — WSL cron (or a Windows Scheduled Task invoking
`wsl.exe -e`), every ~15 minutes:

```bash
# WSL crontab -e  (BASE_RPC_URL from .env.live; --logs-rpc = a public/log-capable endpoint,
# Alchemy free tier caps eth_getLogs at 10 blocks)
*/15 * * * * cd /mnt/c/Users/cld-main/Desktop/github-projects/project-chimera && \
  python3 scripts/snapshot_generator.py --chain base --output config/snapshot.json \
  --scan-blocks 50000 --hf-max 1.10 --logs-rpc "$LOGS_RPC_URL" >> logs/snapshot_gen.log 2>&1
```

### 8.3 Refresh metrics & alarms (port 9554 in the current deployment)

| Metric | Healthy | Alarm |
|---|---|---|
| `chimera_price_refresh_age_seconds` | < 3× `price_refresh_secs` | > `price_max_stale_secs`: repricing degraded (RPC 429s?); in live mode scans are being skipped |
| `chimera_price_refresh_total{result="error"}` | rate ≈ 0 | sustained growth = rate-limited RPC; backoff is automatic, consider a paid tier |
| `chimera_snapshot_age_seconds` | < 2× generator cadence | growing unbounded = generator dead / cron broken |
| `chimera_snapshot_block_number` | advances every generator run | flat = generator writing stale data or reloads being rejected |
| `chimera_snapshot_reload_total{result="rejected"/"read_error"}` | ≈ 0 | rejected = generator regression (backwards block / wrong chain / mock output) |
| `chimera_scans_skipped_stale_price_total` | 0 in shadow | any growth in live mode = detection halted on stale prices |

Note: metric names above are the engine's; the derived default port for Base
(`9100 + 8453 % 1000 = 9553`) collides with the dashboard's own bind — the current
deployment runs the engine's metrics on **9554** via explicit override. Do not assume the
derived default.

---

## 9. Related Documentation

- [`README.md`](../README.md) — Quick start, financial guardrails, "Never" rules
- [`docs/architecture.md`](architecture.md) — Component diagrams, data flow, security model
- [`docs/emergency-procedures.md`](emergency-procedures.md) — Breaker tripping, recovery, state restoration
- [`docs/testing-strategy-liquidations.md`](testing-strategy-liquidations.md) — Simulation accuracy plan
