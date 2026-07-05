# Emergency Procedures

## Auto-Trip Conditions

The circuit breaker auto-trips when any of the following thresholds are breached:

| Condition | Threshold | Action |
|-----------|-----------|--------|
| Consecutive reverts | 3+ | Halt all on-chain transactions |
| Gas price | > 300 gwei | Pause non-urgent operations |
| Daily loss | >= 0.005 ETH | Immediate shutdown |

When tripped, the pacing engine sets `chimera_breaker_state` to `1`. The breaker remains tripped until the operator manually clears it via the gated `clear_breaker` path (requires `CHIMERA_OPERATOR_TOKEN`). The engine continues running but skips all detect/simulate/submit work.

---

## Manual Emergency Pause

If you detect an anomaly before the breaker trips:

```bash
python scripts/emergency_pause.py --state-file core/state/emergency.flag --reason "<description>"
```

The script writes `core/state/emergency.flag` with JSON `{paused: true, reason: "...", triggered_at: ..., triggered_by: "..."}`. The orchestrator detects this at the top of each scan loop (within 3s) and calls `trip_emergency(reason)` which trips the breaker with `BreakerReason::EmergencyHalt`.

To confirm the breaker is tripped:

```bash
curl -s http://localhost:9553/metrics | grep chimera_breaker_state
```

Returns `1` when tripped. Alternatively, check the flag file directly:

```bash
cat core/state/emergency.flag
```

---

## Recovery Checklist

After a breaker trip, do **not** resume blindly. Complete the following:

- [ ] **RPC Health**: Verify primary and fallback RPC endpoints respond within 2s
- [ ] **Wallet Balances**: Check hot wallet ETH and token balances for unexpected drains
- [ ] **Log Review**: Inspect the active log `logs/chimera.log` and rotated logs `logs/chimera.log.YYYY-MM-DD` for root cause. Search for `ERROR`, `BREAKER`, or `panic`:
      ```bash
      grep -E "ERROR|BREAKER|panic" logs/chimera.log logs/chimera.log.*
      ```
- [ ] **Counter Reset**: Only reset revert/loss counters if the underlying issue is resolved

Run the health script:

```bash
python scripts/health_check.py --rpc <URL> --metrics-port 9553
```

All checks must pass before proceeding.

---

## Resuming After a Breaker Trip

Once recovery checks pass and root cause is resolved:

1. If the emergency flag is active, remove it:
   ```bash
   python scripts/emergency_pause.py --state-file core/state/emergency.flag --resume
   ```
   This deletes the flag file, but the breaker REMAINS tripped.

2. Clear the breaker via a controlled restart:
   The `clear_breaker()` method on `PacingEngine` is not exposed via a separate
   operator CLI. The supported procedure is:
   a. Confirm the emergency flag is removed (step 1 above).
   b. Stop the binary: `pkill -f "target/release/chimera"` or Ctrl+C in the running terminal.
   c. Restart the binary: `RUST_LOG=chimera=info ./target/release/chimera`
   A fresh boot starts with the breaker clear (breaker state is in-memory, not
   persisted across restarts).

3. Verify: `curl -s http://localhost:9553/metrics | grep chimera_breaker_state` returns `0`

4. Resume with reduced position sizing for the first 10 transactions.

---

## State Recovery from JSONL

Persistent state is written to `core/state/outcomes.jsonl`. To restore after restart or crash:

```bash
python scripts/recover_state.py --jsonl core/state/outcomes.jsonl
```

This replays:
- Open positions
- Daily P&L accumulator
- Breaker counters

Always verify recovered state against on-chain data before resuming.

---

## Solo-Operator Escalation Checklist

If the issue exceeds your current context or time constraints:

- [ ] Capture current logs and state snapshots
- [ ] Document the triggering condition and last successful action
- [ ] Push a branch with current state: `git checkout -b incident/YYYYMMDD-HHMM`
- [ ] If funds are at risk, rotate the hot wallet via `scripts/rotate_wallet.py`
- [ ] Open a tracking issue with the incident template below

Do not leave the system in an ambiguous state. Explicitly pause or resume.

---

## Incident Log Template

Use this format for all incidents:

```markdown
## INCIDENT-YYYYMMDD-NNN

- **Time (UTC)**: YYYY-MM-DD HH:MM
- **Trigger**: [auto-trip | manual pause | anomaly detected]
- **Condition**: [3+ reverts | gas spike | loss threshold | other]
- **Immediate Action**: [paused / shadow mode / wallet rotated]
- **Root Cause**: [to be filled during review]
- **Resolution**: [how it was resolved]
- **Countermeasures**: [what prevents recurrence]
```

Append to `docs/incident-log.md` and include the commit hash of the state branch.
