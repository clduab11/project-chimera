# Incident Log

Chronological incident record for post-mortem analysis. Every incident that triggers the pacing engine circuit breaker (`BreakerReason` auto-halt) or a manual emergency pause must be recorded here before the breaker is cleared.

> **This log is append-only. Never edit or reorder existing entries.** If new information surfaces after an entry is written (e.g., post-mortem findings, follow-up actions), append a new paragraph prefixed with `**UPDATE YYYY-MM-DD:**` to that entry. Immutability of the historical record is required for accurate post-mortem analysis.

---

## Template

Copy the block below for each new incident. Replace placeholder values and delete unused fields.

```markdown
## INCIDENT-YYYYMMDD-NNN

- **Time (UTC)**: YYYY-MM-DD HH:MM
- **Severity**: P0 / P1 / P2 / P3 / P4
- **Title**: One-line summary
- **Status**: detected | responding | mitigated | resolved | postmortem-complete
- **Trigger**: auto-trip | manual pause | anomaly detected
- **Condition**: 3+ reverts | gas spike | loss threshold | emergency flag | other
- **Description**: What happened, how it was detected, and timeline of events.
- **Immediate Action**: paused | shadow mode | wallet rotated | other
- **Root Cause**: Technical analysis of the underlying cause.
- **Impact**:
  - Financial: ETH / token losses, gas costs, opportunity cost
  - Operational: Downtime duration, missed opportunities, affected components
- **Mitigation**: Steps taken to stop the incident and prevent immediate recurrence.
- **Resolution**: How the incident was permanently resolved.
- **Countermeasures**: Changes that prevent recurrence (code, config, process, monitoring).
- **Follow-up Actions**: Any remaining work items (link to issues/tickets).
- **Related PRs/Commits**: Links to PRs, commits, or the state branch `incident/YYYYMMDD-HHMM`.
```

### Severity Definitions

| Severity | Criteria |
|----------|----------|
| **P0** | Funds at risk, active exploit, or chain-wide outage affecting all operations |
| **P1** | Breaker tripped with loss >= 0.005 ETH, or sustained operation halt > 10 min |
| **P2** | Breaker tripped on gas spike or consecutive reverts, no financial loss |
| **P3** | Anomaly detected, investigated, no breaker trip required |
| **P4** | Scheduled maintenance, planned downtime, or false-positive investigation |

### Related Mechanisms

- **Pacing engine auto-halt**: When `chimera_breaker_state` transitions to `1`, the pacing engine skips all detect/simulate/submit work. See `core/src/simulator/pacing.rs` for threshold definitions (`consecutive_reverts >= 3`, `gas_price > 300 gwei`, `daily_loss >= 0.005 ETH`).
- **Emergency pause script**: `scripts/emergency_pause.py` writes `core/state/emergency.flag` which the orchestrator detects within 3s, triggering `BreakerReason::EmergencyHalt`. See `docs/emergency-procedures.md` for full procedure.
- **Recovery**: The breaker is cleared on restart (in-memory state). Resumption requires the recovery checklist in `docs/emergency-procedures.md`.

---

## Incident Records

_No incidents recorded. Entries are appended below this line in reverse chronological order (newest first)._
