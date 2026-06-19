# Emergency Procedures

## Auto-Trip Conditions

The circuit breaker auto-trips when any of the following thresholds are breached:

| Condition | Threshold | Action |
|-----------|-----------|--------|
| Consecutive reverts | 3+ | Halt all on-chain transactions |
| Gas price | > 300 gwei | Pause non-urgent operations |
| Daily loss | >= 0.005 ETH | Immediate shutdown |

When tripped, the bot enters **shadow mode** (logs only, no execution).

---

## Manual Emergency Pause

If you detect an anomaly before the breaker trips:

```bash
python scripts/emergency_pause.py --reason "<description>"
```

This immediately:
1. Cancels pending transactions
2. Sets the global `paused` flag
3. Writes a timestamped entry to `logs/incidents/`

To confirm status:

```bash
python scripts/emergency_pause.py --status
```

---

## Recovery Checklist

After a breaker trip, do **not** resume blindly. Complete the following:

- [ ] **RPC Health**: Verify primary and fallback RPC endpoints respond within 2s
- [ ] **Wallet Balances**: Check hot wallet ETH and token balances for unexpected drains
- [ ] **Log Review**: Inspect `logs/breaker/` and `logs/execution/` for root cause
- [ ] **Counter Reset**: Only reset revert/loss counters if the underlying issue is resolved

Run the health script:

```bash
python scripts/health_check.py --full
```

All checks must pass before proceeding.

---

## Resuming from Shadow Mode

Once recovery checks pass:

1. Review shadow-mode logs for missed opportunities or failed simulations
2. Confirm strategy parameters are still valid (pool fees, token addresses)
3. Run a dry-run transaction:
   ```bash
   python scripts/dry_run.py --strategy <name>
   ```
4. If dry-run succeeds, disable shadow mode:
   ```bash
   python scripts/toggle_shadow.py --off
   ```

Resume with reduced position sizing for the first 10 transactions.

---

## State Recovery from JSONL

Persistent state is written to `data/state/*.jsonl`. To restore after restart or crash:

```bash
python scripts/recover_state.py --file data/state/latest.jsonl
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
