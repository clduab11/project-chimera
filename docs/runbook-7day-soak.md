# Project Chimera — 7-Day Shadow Soak Runbook

**Document version:** 1.1
**Last updated:** 2026-07-05
**Target audience:** Solo operator
**Status:** Operator-executed runbook — do NOT execute during authoring.
**Deployment model:** Host-run binary + monitoring stack via Docker Compose (Grafana, Prometheus, Qdrant only; the chimera-core binary runs directly on the host).

This runbook walks the operator through the mandatory 7-day (604,800-second) shadow soak period before capital is ever put at risk. Every step is checklisted. Every command is copy-paste safe (assuming the working directory is the repo root).

The 7-day minimum is non-negotiable and is enforced by two independent gates:

- `scripts/toggle_shadow.py --set-live` refuses exit 1 if `shadow_since` age < 7 days.
- `PacingConfig::validate_mode_transition()` in `core/src/config.rs:219` refuses boot if previous_mode=shadow and shadow_since is missing or < 604,800 s.

---

## 1. Purpose & Prerequisites

The soak period proves the engine runs the full detect→simulate→pace pipeline stably under real market conditions without ever broadcasting a transaction. It is the primary evidence gate before capital-at-risk go-live.

### 1.1 Prerequisites

- [ ] Rust toolchain (stable 1.74+): `cargo --version`
- [ ] Python 3.10+: `python3 --version`
- [ ] Docker + Docker Compose: `docker --version && docker compose version`
- [ ] `pip install -r requirements.txt` completed in repo root
- [ ] A Base RPC endpoint ready (env var `RPC_URL` or `BASE_RPC_URL`)
- [ ] `config/pacing.yaml` has `execute_mode: shadow`
- [ ] `config/eoa_pool.json` populated with your worker public addresses
- [ ] At least one worker EOA funded with ≥ 0.02 ETH (for eventual live gas; optional for shadow)
- [ ] `config/pacing.yaml` values match defaults in `core/src/config.rs` tests (Invariant #1)

### 1.2 Soak Success Criteria (what you must prove)

- [ ] ≥ 50 simulation runs completed with plausible candidates
- [ ] Zero unexpected breaker trips (manual drill trips are expected and logged)
- [ ] Prometheus metrics exported continuously for 7 full days
- [ ] `chimera_breaker_state` reads 0 at all times except during scheduled drills
- [ ] Grafana dashboard shows readable, clean metrics for 7 consecutive days
- [ ] Crash-restart drill passes (state recovered, continuity confirmed)
- [ ] Emergency pause/resume drill passes (breaker tripped, cleared, resumed)
- [ ] Auto-trip validation passes for revert threshold, gas spike, and loss threshold
- [ ] Log scans show no `ERROR` or `panic` strings outside of known drill events

---

## 2. Day 0: Soak Setup

### 2.1 Build the Binary

```bash
cargo build -p chimera-core --release
```

Confirm binary exists:

```bash
ls -la target/release/chimera || ls -la target/release/chimera.exe
```

### 2.2 Verify Configuration

Run the preflight validator. This NEVER broadcasts — it only checks config, snapshot, and optional RPC reachability:

```bash
python scripts/dry_run.py --rpc https://mainnet.base.org
```

Expected output must end with `Preflight PASS (execute_mode=shadow).`

If `dry_run.py` fails, resolve the issue before proceeding. Common failures:
- `pacing.yaml` missing or `execute_mode` not `shadow`
- RPC unreachable
- YAML unparseable

Optionally, confirm the binary boots for 5 seconds without panic:

```bash
python scripts/dry_run.py --rpc https://mainnet.base.org --run-secs 5 --binary target/release/chimera
```

### 2.3 Generate the Initial Snapshot

Mock first (no RPC required):

```bash
python scripts/snapshot_generator.py --chain base --mock
```

Then with live data (requires `web3.py` and `RPC_URL` / `BASE_RPC_URL`):

```bash
python scripts/snapshot_generator.py --chain base --rpc-url https://mainnet.base.org
```

Confirm snapshot parses:

```bash
python -c "import json; s = json.load(open('config/snapshot.json')); print(f'reserves={len(s.get(\"reserves\",[]))} users={len(s.get(\"users\",[]))}')"
```

### 2.4 Start the Shadow Clock

The binary auto-stamps `shadow_since = now()` in `core/state/mode.json` on first boot when `execute_mode=shadow` and `mode.json` is absent. To explicitly stamp before boot, run:

```bash
python scripts/toggle_shadow.py --set-shadow
```

Verify the stamp:

```bash
python scripts/toggle_shadow.py --show
```

Expected output includes:

```json
{
  "previous_mode": "shadow",
  "shadow_since": <unix_timestamp>,
  "_exists": true,
  "_shadow_age": "0d 0h 0m",
  "_soak_satisfied": false
}
```

Note the `shadow_since` timestamp. Write it down. The 7-day clock started when this was stamped.

### 2.5 Set RPC Environment Variable

```bash
export RPC_URL=https://mainnet.base.org
# Or export BASE_RPC_URL=... (main.rs reads RPC_URL first, then BASE_RPC_URL for Base chain_id)
```

### 2.6 Start the Engine in Shadow Mode

**Important — metrics port offset:** The binary binds its Prometheus metrics server on port `base_port + (chain_id % 1000)` at runtime (see `core/src/main.rs:150-151`). The `metrics_port: 9100` value in `config/pacing.yaml` is the **base** port. For Base (chain_id=8453): `9100 + 453 = 9553`. All `curl`, `grep`, and `health_check.py` commands in this runbook use port **9553**, not 9100.

```bash
RUST_LOG=chimera=info ./target/release/chimera
```

Confirm healthy boot:
1. stdout shows `Orchestrator starting chain_id=8453`
2. `curl http://localhost:9553` returns Prometheus text with `chimera_` prefixes
3. `chimera_breaker_state 0` in metrics output

Keep this terminal open. In a second terminal, proceed to 2.7.

### 2.7 Start the Monitoring Stack

The monitoring stack (Grafana, Prometheus, Qdrant) runs via Docker Compose. The chimera-core binary runs on the host — do NOT expect a `chimera-core` container in compose output.

```bash
docker compose -f docker/docker-compose.yml up -d
```

Wait for containers to be healthy:

```bash
docker compose -f docker/docker-compose.yml ps
```

All services (`grafana`, `prometheus`, `qdrant`) should show `Up` or `running`. The `chimera-core` service is NOT started by compose under the host-run model.

Verify monitoring endpoints:

```bash
curl -s http://localhost:9090/-/healthy      # Prometheus
curl -s http://localhost:3002/api/health     # Grafana (NOT port 3003)
curl -s http://localhost:9553 | head -5      # Engine metrics (host binary on port 9553)
```

Grafana is at **`http://localhost:3002`**. The operator manual references port 3003 — that is incorrect. The Docker Compose file maps `3002:3002`.

**Prometheus scrape target:** The Prometheus config at `monitoring/prometheus.yml:29` targets `chimera-core:9553` (Docker internal hostname). For host-run binary, Prometheus cannot reach the host binary via that hostname. To fix, either:
- Update `monitoring/prometheus.yml` target to `host.docker.internal:9553` (Docker Desktop) or `172.17.0.1:9553` (Linux), OR
- Accept that Prometheus target will be unreachable in the compose dashboard and use direct `curl` queries to `localhost:9553` for all metrics verification.

### 2.8 Day-0 Health Check

Run the operator health probe:

```bash
python scripts/health_check.py --rpc https://mainnet.base.org --metrics-port 9553
```

Expected: all checks PASS or WARN (no FAIL). Common WARNs on Day 0:
- `snapshot_freshness` — stale until re-generated
- `mode_state` — WARN if binary hasn't stamped yet (resolve with `--set-shadow`)

For machine-readable output suitable for cron:

```bash
python scripts/health_check.py --rpc https://mainnet.base.org --metrics-port 9553 --json
```

### 2.9 Verify the Binary is Producing Outcomes

After 5–10 minutes of runtime:

```bash
python scripts/status.py --chain base
```

Expected dashboard sections:
1. **Header** — Mode: `shadow`, Chain: `base`
2. **EOA Pool** — Your worker addresses with balance info
3. **Pacing State** — Daily/Weekly spend vs configured caps
4. **Circuit Breaker** — `CLEAR` with revert count
5. **Recent Outcomes** — Should start populating with `Denied` entries (shadow mode denies all submissions)

If the dashboard shows an empty State panel, the binary hasn't written outcomes yet. Wait a few more minutes and retry.

If `status.py` shows an empty outcomes panel, wait a few more scan cycles — the JSONL is written at end-of-scan and may need 1-2 cycles to populate.

### 2.10 Baseline Screenshot

Open Grafana at `http://localhost:3002`, log in (default admin / chimera per `docker-compose.yml`), and take a screenshot of the main dashboard. Save:

```
logs/screenshots/day-0-baseline.png
```

---

## 3. Daily Operator Tasks (Days 1–7)

Perform these once per day, ideally at the same time (± 2 hours) to establish a consistent operational rhythm. Each day's checklist takes 10–20 minutes.

### 3.1 Daily Checklist

#### Day ___ (fill in date: ________)

- [ ] **Shadow clock check** — Run `python scripts/toggle_shadow.py --show` and confirm `_soak_satisfied` remains `false` (expected until Day 7+). Note the `_shadow_age` field.

- [ ] **Health probe** — Run `python scripts/health_check.py --rpc <URL> --metrics-port 9553`. All critical checks must PASS. Investigate any FAIL immediately.

- [ ] **Status dashboard** — Run `python scripts/status.py --chain base`. Review:
  - Circuit Breaker panel: must read `CLEAR`
  - Pacing State panel: daily/weekly values should be within configured caps
  - Recent Outcomes panel: confirm entries are flowing

- [ ] **Grafana review** — Open `http://localhost:3002`. Inspect these panels:
  | Panel | Look for |
  |---|---|
  | `chimera_breaker_state` | Must be `0` |
  | `chimera_candidates_seen_total` | Should increment daily |
  | `chimera_sims_run_total{result="ok"}` | Should increment over time |
  | `chimera_sim_latency_seconds` (p99) | Should be under 500 ms |
  | `chimera_gas_used` | Should show sane ranges (150k–500k) |

- [ ] **Grafana screenshot** — Save to `logs/screenshots/day-N-grafana.png` (where N = day number).

- [ ] **Log scan** — Run the daily log grep:

  The binary logs to `logs/chimera.log` with daily rotation via `tracing_appender::rolling::daily`. Rotated files are named `logs/chimera.log.YYYY-MM-DD` (the date is appended as a suffix after `.log`).

  ```bash
  grep -iE "error|panic|BREAKER|warn" logs/chimera.log
  # Or scan the most recent rotated file:
  tail -n 200 "logs/chimera.log.$(date +%Y-%m-%d)" 2>/dev/null | grep -iE "error|panic|BREAKER|warn"
  ```

  Expected findings:
  - `BREAKER` lines: only from scheduled drills (Days 5–7)
  - `ERROR` lines: review each one; known shadow-mode false-positives (e.g., "keystore not configured") are acceptable
  - `panic` lines: must be zero. Any panic requires a restart and root-cause investigation.

- [ ] **Breaker state verification** — Run:
  ```bash
  curl -s http://localhost:9553 | grep chimera_breaker_state
  ```
  Must return `chimera_breaker_state 0` outside of scheduled drill windows.

- [ ] **Balance check** — Run:
  ```bash
  python scripts/check_balances.py --chain base --min-balance 0.005
  ```
  No wallet should show `LOW`. Exit code must be 0. If any wallet is LOW, fund it from treasury before proceeding.

- [ ] **Disk check** — Run `df -h .`. At least 5 GB free on the logs partition.

- [ ] **Log archival** — Copy today's rotated log to the soak archive. Because `tracing_appender::rolling::daily` produces `logs/chimera.log.YYYY-MM-DD`, use:
  ```bash
  mkdir -p soak-evidence/logs
  cp "logs/chimera.log.$(date +%Y-%m-%d)" soak-evidence/logs/
  ```

#### Day-Specific Notes

- **Day 1:** Confirm the JSONL audit trail is growing. Check both possible filenames:
  ```bash
  ls -la core/state/*.jsonl
  ```
  Verify the active file by checking which one has recent modification time. Run `wc -l core/state/outcomes.jsonl 2>/dev/null || wc -l core/state/audit.jsonl`.

- **Day 2:** Verify `recover_state.py` can rebuild state from JSONL (warm-up for Day 3–4 drill):
  ```bash
  python scripts/recover_state.py --jsonl core/state/outcomes.jsonl
  ```
- **Day 3–4:** Scheduled crash-restart drill (see Section 4).
- **Day 5–6:** Scheduled emergency drill (see Section 5).
- **Day 6–7:** Auto-trip validation (see Section 6).
- **Day 7:** Soak completion review (see Section 7).

---

## 4. Crash-Restart Drill (Scheduled, Day 3–4)

Purpose: Prove the engine can recover state from the JSONL audit trail after an ungraceful shutdown, and resume without data loss.

### 4.1 Before the Drill

- [ ] Confirm the JSONL audit file exists and has entries:
  ```bash
  wc -l core/state/outcomes.jsonl 2>/dev/null || wc -l core/state/audit.jsonl
  ```
- [ ] Take a baseline state snapshot:
  ```bash
  python scripts/recover_state.py --jsonl core/state/outcomes.jsonl
  ```
  Record the values of `daily_usage_usd`, `weekly_usage_usd`, `consecutive_reverts`.

- [ ] Note the engine PID:
  ```bash
  pgrep -f "target/release/chimera" || pgrep -f chimera
  ```

### 4.2 Kill the Binary

```bash
kill -9 <PID>
```

Confirm it's dead:

```bash
pgrep -f chimera
# Expected: no output
```

### 4.3 Verify State Recovery

```bash
python scripts/recover_state.py --jsonl core/state/outcomes.jsonl
```

The recovered values must match the baseline captured in 4.1. If they do not:
- Check that the JSONL file was not corrupted by the kill
- Re-run recovery and compare
- If `consecutive_reverts` differs, check the ordering of JSONL lines

### 4.4 Restart the Engine

```bash
RUST_LOG=chimera=info ./target/release/chimera
```

### 4.5 Confirm Continuity

- [ ] `curl http://localhost:9553 | grep chimera_breaker_state` returns `0`
- [ ] `python scripts/status.py --chain base` shows dashboard populating again
- [ ] Compare `daily_usage_usd` from new outcomes against the pre-kill baseline — values should be continuous, not reset

### 4.6 Log the Drill

```bash
echo "$(date -Iseconds) | CRASH-RESTART DRILL | killed PID $PID | state recovered | engine restarted | breaker clear" >> soak-evidence/drill-log.txt
```

Take a post-restart Grafana screenshot: `logs/screenshots/day-N-post-restart.png`.

---

## 5. Emergency-Drill (Scheduled, Day 5–6)

Purpose: Prove the emergency pause system works end-to-end: flag creation, breaker metrics reflection, incident logging, flag clearing, and resumption.

### 5.1 Trip the Breaker

```bash
python scripts/emergency_pause.py --state-file core/state/emergency.flag --reason "scheduled-soak-drill-day5"
```

Expected output:
```
EMERGENCY PAUSE triggered by <user>: scheduled-soak-drill-day5
```

### 5.2 Verify Breaker State

```bash
curl -s http://localhost:9553 | grep chimera_breaker_state
```

Must return `chimera_breaker_state 1`.

### 5.3 Verify the Emergency Flag File

```bash
cat core/state/emergency.flag
```

Must show:
```json
{
  "paused": true,
  "reason": "scheduled-soak-drill-day5",
  "triggered_at": <unix_seconds>,
  "triggered_by": "<user>"
}
```

### 5.4 Verify the Incident is Recorded

```bash
grep -i "pause\|emergency\|breaker" logs/chimera.log
```

Confirm the log shows the pause event was detected or the engine halted.

### 5.5 Health Check During Pause

```bash
python scripts/health_check.py --rpc <RPC_URL> --metrics-port 9553
```

Expected: `emergency_flag` shows `FAIL` with `PAUSED: scheduled-soak-drill-day5`.

### 5.6 Clear the Breaker and Resume

```bash
python scripts/emergency_pause.py --state-file core/state/emergency.flag --resume
```

Expected output:
```
EMERGENCY PAUSE cleared by <user>
Pause state cleared (core/state/emergency.flag removed)
```

### 5.7 Verify Resumption

- [ ] `curl -s http://localhost:9553 | grep chimera_breaker_state` returns `0`
- [ ] `core/state/emergency.flag` no longer exists:
  ```bash
  ls core/state/emergency.flag
  # Expected: No such file or directory
  ```
- [ ] `python scripts/health_check.py --rpc <RPC_URL> --metrics-port 9553` shows `emergency_flag: PASS`
- [ ] `python scripts/status.py --chain base` shows breaker `CLEAR` and outcomes flowing again

### 5.8 Log the Drill

```bash
echo "$(date -Iseconds) | EMERGENCY DRILL | paused: scheduled-soak-drill-day5 | resumed: $(date -Iseconds) | breaker back to 0" >> soak-evidence/drill-log.txt
```

Take a post-resume Grafana screenshot: `logs/screenshots/day-N-post-resume.png`.

### 5.9 Webhook Validation (Optional)

If you have a webhook URL configured, repeat the drill with `--alert-url`:

```bash
python scripts/emergency_pause.py \
  --state-file core/state/emergency.flag \
  --reason "soak-drill-webhook-test" \
  --alert-url https://hooks.slack.com/services/YOUR/WEBHOOK/URL
```

Confirm the webhook was received, then resume:

```bash
python scripts/emergency_pause.py --state-file core/state/emergency.flag --resume --alert-url https://hooks.slack.com/services/YOUR/WEBHOOK/URL
```

---

## 6. Auto-Trip Validation (Scheduled, Day 6–7)

Purpose: Prove each auto-trip condition in the circuit breaker functions correctly. These tests create controlled failure scenarios. **Perform them sequentially and clear the breaker between each test.**

### 6.1 Revert Threshold Trip (3+ Consecutive Reverts)

The breaker trips when `consecutive_reverts >= auto_halt_on_reverts` (default: 3).

**Verification method:** In shadow mode, the engine does not submit transactions, so on-chain reverts cannot occur. Inspect the JSONL to confirm the logic:

```bash
python scripts/recover_state.py --jsonl core/state/outcomes.jsonl
```

Verify that `consecutive_reverts` is computed correctly against the file order.

**Manual validation:** Since shadow mode produces zero on-chain reverts, this condition is verified by code inspection of the pacing engine logic and by confirming that the `consecutive_reverts` field in `recover_state.py` output tracks the JSONL correctly.

- [ ] `recover_state.py` output shows `consecutive_reverts` = 0 when no reverted entries exist
- [ ] Reviewed `auto_halt_on_reverts: 3` in `config/pacing.yaml` — confirmed threshold is configured

### 6.2 Gas Spike Trip (Gas > 300 gwei)

The breaker trips when `max_gas_gwei` (default 300) is exceeded.

**Verification method:** The gas check runs per-candidate during the pacing evaluation. To validate:

1. Run the status dashboard and confirm the breaker is `CLEAR`:
   ```bash
   python scripts/status.py --chain base
   ```

2. Check the current L2 gas price via RPC:
   ```bash
   curl -X POST https://mainnet.base.org \
     -H "Content-Type: application/json" \
     -d '{"jsonrpc":"2.0","method":"eth_gasPrice","params":[],"id":1}'
   ```
   Convert the hex result to gwei. The engine reads this at candidate evaluation time.

3. If current gas is below 300 gwei (expected on Base under normal conditions), the breaker should remain clear.

- [ ] Confirmed `max_gas_gwei: 300` in `config/pacing.yaml`
- [ ] Breaker did not trip spuriously during normal gas conditions
- [ ] Understanding: a gas spike above 300 gwei would trip the breaker automatically

### 6.3 Daily Loss Threshold Trip (Loss ≥ 0.005 ETH)

The breaker trips when `daily_loss_eth >= max_daily_loss_eth` (default 0.005).

**Verification method:** In shadow mode, no actual gas is spent on-chain, so daily loss should remain near zero. Validate the tracking logic:

```bash
python scripts/recover_state.py --jsonl core/state/outcomes.jsonl
```

- [ ] `daily_usage_usd` is within configured caps
- [ ] No entries show negative `realized_net_usd` large enough to trigger loss threshold
- [ ] Confirmed `max_daily_loss_eth: 0.005` in `config/pacing.yaml`

**Note:** Full end-to-end auto-trip validation for the loss threshold requires live mode (actual gas expenditure). The soak period validates that the tracking infrastructure (JSONL, Prometheus metrics, breaker gauge) is wired correctly and would respond if the threshold were crossed.

### 6.4 Document Auto-Trip Validation Results

```bash
echo "$(date -Iseconds) | AUTO-TRIP VALIDATION | revert threshold: CONFIG VERIFIED | gas spike: CONFIG VERIFIED ($(curl -s -X POST https://mainnet.base.org -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","method":"eth_gasPrice","params":[],"id":1}' | python -c "import sys,json; print(int(json.load(sys.stdin)['result'],16)/1e9)") gwei current) | loss threshold: CONFIG VERIFIED" >> soak-evidence/drill-log.txt
```

---

## 7. Soak Completion Criteria

After 7 full days (604,800 seconds since `shadow_since`), verify every item below before proceeding to go-live:

### 7.1 Soak Clock

- [ ] `python scripts/toggle_shadow.py --show` reports `"_soak_satisfied": true`
- [ ] `_shadow_age` shows ≥ `7d 0h 0m`

### 7.2 Metrics Completeness

- [ ] Prometheus has been scraping for ≥ 7 days: check `http://localhost:9090/targets` — up time ≥ 168h
- [ ] Grafana dashboard shows 7 full days of data at `http://localhost:3002`
- [ ] `chimera_breaker_state` was 0 for ≥ 99% of uptime (excluding scheduled drill windows)

### 7.3 Simulation Volume

- [ ] ≥ 50 simulation runs recorded in the JSONL audit trail:
  ```bash
  wc -l core/state/outcomes.jsonl 2>/dev/null || wc -l core/state/audit.jsonl
  ```
- [ ] ≥ 50 candidates seen: `curl -s http://localhost:9553 | grep chimera_candidates_seen_total`

### 7.4 Drill Completion

- [ ] Crash-restart drill completed and logged (Section 4)
- [ ] Emergency pause/resume drill completed and logged (Section 5)
- [ ] Auto-trip validation completed and documented (Section 6)
- [ ] `soak-evidence/drill-log.txt` contains entries for all three drills

### 7.5 Log Hygiene

- [ ] No `panic` strings in any log file from the soak period:
  ```bash
  grep -rn "panic" logs/ --include="chimera.log*" | wc -l
  # Expected: 0
  ```
- [ ] No unresolved `ERROR` lines (acceptable: known shadow-mode messages like keystore absence)

### 7.6 Balance Check

- [ ] `python scripts/check_balances.py --chain base --min-balance 0.005` exits 0 with no LOW wallets
- [ ] All worker EOAs funded to ≥ 0.02 ETH

### 7.7 Operator Confidence

- [ ] Operator understands the shadow→live transition procedure
- [ ] Operator has read `docs/emergency-procedures.md`
- [ ] Operator knows how to trip the breaker manually and clear it
- [ ] Operator has `scripts/emergency_pause.py` command memorized or saved

If any item above is unchecked, the soak is NOT complete. Do not proceed to go-live. Extend the soak period until all criteria are met.

---

## 8. Soak Evidence Archival

Before transitioning to live mode, archive all soak evidence permanently.

### 8.1 Export Prometheus Snapshot

Create a timestamped snapshot of the full metrics state:

```bash
mkdir -p soak-evidence/metrics
curl -s http://localhost:9090/api/v1/query?query=chimera_breaker_state > soak-evidence/metrics/breaker_state.json
curl -s http://localhost:9090/api/v1/query?query=chimera_candidates_seen_total > soak-evidence/metrics/candidates_seen.json
curl -s http://localhost:9090/api/v1/query?query=chimera_sims_run_total > soak-evidence/metrics/sims_run.json
```

For a full Prometheus snapshot dump, use the Prometheus API:

```bash
curl -s http://localhost:9090/api/v1/status/tsdb > soak-evidence/metrics/tsdb_status.json
```

### 8.2 Collect All Logs

```bash
mkdir -p soak-evidence/logs
cp logs/chimera.log.* soak-evidence/logs/
# Also copy the current active log:
cp logs/chimera.log soak-evidence/logs/chimera.log.active 2>/dev/null
```

### 8.3 Collect Grafana Screenshots

```bash
mkdir -p soak-evidence/screenshots
cp logs/screenshots/*.png soak-evidence/screenshots/ 2>/dev/null || echo "No screenshots to copy"
```

If screenshots were not taken daily, take a final set now:

- `soak-evidence/screenshots/final-dashboard.png` — full Grafana dashboard
- `soak-evidence/screenshots/final-breaker-panel.png` — breaker panel detail
- `soak-evidence/screenshots/final-throughput-panel.png` — throughput panel detail

### 8.4 Export Mode State Snapshot

```bash
python scripts/toggle_shadow.py --show > soak-evidence/mode-state-final.json
```

### 8.5 Export Pacing State

```bash
python scripts/recover_state.py --jsonl core/state/outcomes.jsonl --json > soak-evidence/recovered-state-final.json
```

### 8.6 Export Dashboard Snapshot

```bash
python scripts/status.py --chain base > soak-evidence/status-dashboard-final.txt 2>&1
```

### 8.7 Create Archive Manifest

```bash
cat > soak-evidence/MANIFEST.txt << 'EOF'
Project Chimera — 7-Day Shadow Soak Evidence Archive
=====================================================
Operator: (fill in)
Shadow start (unix): (fill in from toggle_shadow.py --show)
Shadow end (unix):   (fill in)
Soak duration:       (fill in from _shadow_age field)

Contents:
  metrics/     — Prometheus query snapshots
  logs/        — Full daily log files from soak period
  screenshots/ — Grafana dashboard captures (daily + final)
  mode-state-final.json       — mode.json at soak completion
  recovered-state-final.json   — recover_state.py output at completion
  status-dashboard-final.txt   — status.py output at completion
  drill-log.txt               — Drill execution log

Validation:
  - [ ] toggle_shadow.py --show reports _soak_satisfied: true
  - [ ] All 3 drills completed and logged
  - [ ] chimera_breaker_state was 0 for ≥ 99% of uptime
  - [ ] ≥ 50 simulation runs completed
  - [ ] No panics in logs
  - [ ] All worker EOAs funded

This archive constitutes the primary pre-go-live evidence that
Project Chimera operated stably in shadow mode for the mandatory
7-day soak period.
EOF
```

### 8.8 Hash the Archive

```bash
cd soak-evidence && find . -type f -exec sha256sum {} \; > checksums.sha256 && cd ..
```

### 8.9 Final Confirmation

Before proceeding to the go-live procedure:

- [ ] `soak-evidence/MANIFEST.txt` is complete
- [ ] `soak-evidence/checksums.sha256` validates
- [ ] All evidence files are present (count them: `find soak-evidence -type f | wc -l`)
- [ ] `python scripts/toggle_shadow.py --show` reports `_soak_satisfied: true`

The soak is complete. Proceed to the go-live transition.

---

## Appendix A: Quick-Reference Commands

```bash
# Show shadow clock status
python scripts/toggle_shadow.py --show

# Preflight validation (NEVER broadcasts)
python scripts/dry_run.py --rpc <URL>

# Read-only operational dashboard
python scripts/status.py --chain base

# Health probe (all checks) — uses port 9553 for Base (chain_id=8453)
python scripts/health_check.py --rpc <URL> --metrics-port 9553

# EOA balance inspector
python scripts/check_balances.py --chain base --min-balance 0.005

# Emergency pause
python scripts/emergency_pause.py --state-file core/state/emergency.flag --reason "..."

# Emergency resume
python scripts/emergency_pause.py --state-file core/state/emergency.flag --resume

# Crash recovery
python scripts/recover_state.py --jsonl core/state/outcomes.jsonl

# Snapshot generation (mock)
python scripts/snapshot_generator.py --chain base --mock

# Snapshot generation (live)
python scripts/snapshot_generator.py --chain base --rpc-url <URL>

# Start monitoring (Grafana + Prometheus + Qdrant only; binary runs on host)
docker compose -f docker/docker-compose.yml up -d

# Check metrics on host binary (port 9553 for Base chain_id=8453)
curl -s http://localhost:9553 | grep chimera_breaker_state

# Check Grafana health
curl -s http://localhost:3002/api/health

# Check Prometheus targets
curl -s http://localhost:9090/targets

# Count simulation runs (check both possible filenames)
wc -l core/state/outcomes.jsonl 2>/dev/null || wc -l core/state/audit.jsonl
```

## Appendix B: Key File Locations

| Path | Purpose |
|---|---|
| `config/pacing.yaml` | Execute mode, pacing caps, chain config |
| `config/risk.yaml` | Risk thresholds, circuit breaker params |
| `config/routing.yaml` | RPC endpoints, venue definitions |
| `config/eoa_pool.json` | Worker EOA addresses (public keys only) |
| `config/snapshot.json` | Market snapshot for detector |
| `core/state/mode.json` | Shadow→live transition state gate |
| `core/state/outcomes.jsonl` | JSONL audit trail (binary output; `recover_state.py` default) |
| `core/state/audit.jsonl` | JSONL audit trail (`status.py` default) |
| `core/state/emergency.flag` | Emergency pause flag |
| `logs/chimera.log` | Current engine log (stdout + rolling file) |
| `logs/chimera.log.YYYY-MM-DD` | Daily rotated log (tracing_appender::rolling::daily) |
| `logs/screenshots/` | Grafana screenshot directory |
| `soak-evidence/` | Soak evidence archive |
| `docker/docker-compose.yml` | Monitoring stack (Grafana, Prometheus, Qdrant; NOT chimera-core for host-run) |
| `monitoring/prometheus.yml` | Prometheus scrape config |

## Appendix C: Metrics Port Offset

The binary binds its Prometheus metrics server at runtime using the formula (see `core/src/main.rs:150-151`):

```
actual_port = config.metrics_port + (chain_id % 1000)
```

| Chain | chain_id | Calculation | Actual Port |
|---|---|---|---|
| Base | 8453 | 9100 + 453 | **9553** |
| Arbitrum One | 42161 | 9100 + 161 | **9261** |
| OP Mainnet | 10 | 9100 + 10 | **9110** |

All `curl`, `grep`, and `health_check.py` commands in this runbook use port **9553** (Base). If operating on a different chain, adjust the port accordingly.

## Appendix D: Transition Gate Enforcement

The 7-day shadow soak is enforced at two independent points in the codebase:

1. **`scripts/toggle_shadow.py --set-live`** (line 137–176): Refuses exit 1 if `shadow_since` age < 604,800 seconds. Prints remaining time and exits without writing.

2. **`core/src/config.rs:validate_mode_transition()`** (line 219–246): Called at startup (main.rs:176). Refuses boot with `ConfigError` if `previous_mode == "shadow"` and `execute_mode == "live"` but `shadow_since` is missing or < 604,800 seconds.

Both gates must be satisfied. `toggle_shadow.py` manages `mode.json` but does NOT change `execute_mode` in `config/pacing.yaml` — that is a separate deliberate operator action.

## Appendix E: Known Partial Implementations

Per `docs/first-run-onboarding.md` §9, the following are documented as not fully wired as of the current binary. They do not block shadow-mode operation but are relevant for go-live planning:

- Multi-chain RPC wiring (`BASE_RPC_URL` / `ARB_RPC_URL`) — `main.rs` reads `RPC_URL` primarily
- JSONL audit trail / state file on disk — library ready; verify wiring before live
- EOA pool loaded at startup — library ready; verify wiring before live
- Live execution path — not implemented in current binary; must be completed before go-live
- `fund_eoa.py` wallets vs workers JSON shape mismatch — verify before funding

Before transitioning to live, confirm that all wired components have been completed and tested.
