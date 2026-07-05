# Security Policy

## Supported Versions

Project Chimera is pre-release software under active development. Only the current
version receives security updates.

| Version | Status              | Notes                                               |
| ------- | ------------------- | --------------------------------------------------- |
| 0.1.x   | :white_check_mark: Supported | Shadow-mode only; live mode gated by operator preconditions |
| 0.0.x   | :x: Unsupported     | Pre-remediation; do not use                         |

Security patches are released as commits to `main`. There are no backport or LTS
branches at this stage.

---

## Reporting a Vulnerability

Project Chimera is a private repository. There is **no public bug bounty** and
no public disclosure process.

### Contact Channel

Report suspected vulnerabilities directly to the repository maintainers via the
private repository's issue tracker or direct communication channel. Mark the
issue as **confidential** if the platform supports it.

### What to Include

- **Component** — which module is affected (core engine, Executor contract,
  FundDistributor, scripts, ai-audit, configuration)
- **Description** — what the vulnerability is, how it can be triggered
- **Impact** — funds at risk, data exposure, operational disruption
- **Reproduction steps** — command-line invocations, transaction parameters,
  configuration values
- **Suggested fix** — if you have one, include it

### Response Timeline

| Phase          | Target               |
| -------------- | -------------------- |
| Acknowledgment | Within 48 hours      |
| Triage         | Within 5 business days |
| Fix            | Case-dependent; critical issues (funds at risk) prioritized for immediate patch |
| Disclosure     | Coordinated with maintainers; no public disclosure without agreement |

### Safe Harbor

Security research conducted in good faith on your own deployed instances or
local test environments is welcome. Do not:

- Test against contracts or EOAs you do not own
- Attempt to access or exfiltrate data from systems operated by others
- Disrupt production infrastructure

Vulnerability reports are treated confidentially. Researchers acting in good
faith will not face legal action from the maintainers.

---

## Security Model

Project Chimera operates at the intersection of off-chain automation and
on-chain execution. The design is **defense-in-depth**: no single component
failure should result in loss of funds.

### Private Key Management

- **Encrypted keystore files** — private keys for worker EOAs are stored in
  encrypted keystore files outside the repository. These files are listed in
  `.gitignore` and must **never** be committed.
- **`config/eoa_pool.json`** — contains only **public addresses** and rotation
  metadata. No private keys, no mnemonics, no encrypted key material.
- **No hot wallet private keys in configuration files** — `pacing.yaml`,
  `risk.yaml`, `routing.yaml`, `pools.toml`, and all other config files contain
  only operational parameters.

### Shadow-Mode-First Execution

- The `execute_mode` defaults to `shadow` at startup. The engine surveils,
  detects, and simulates — but does **not** submit real transactions.
- A CI job (`shadow-guard`) blocks any PR that sets `execute_mode: live` in
  committed configuration or test fixtures.
- Transition to live mode is gated by `toggle_shadow.py`, which enforces a
  **mandatory 7-day shadow soak** before permitting `mode.json` to be flipped.

### Pacing Engine Auto-Halt Triggers

The pacing engine enforces code-level guardrails with no override mechanism:

| Trigger                         | Threshold            | Action               |
| ------------------------------- | -------------------- | -------------------- |
| Consecutive transaction reverts | 3+                   | Full halt            |
| Gas price                       | > 300 gwei           | Pause non-urgent ops |
| Daily realized loss             | ≥ 0.005 ETH          | Immediate shutdown   |

When tripped, `chimera_breaker_state` is set to `1`. The engine continues
running in a no-op loop but skips all detect/simulate/submit work.

### Emergency Pause Circuit Breaker

- `scripts/emergency_pause.py` writes a `core/state/emergency.flag` file
  detected within 3 seconds by the orchestrator's scan loop.
- The breaker can only be cleared with a valid `CHIMERA_OPERATOR_TOKEN`
  environment variable — unauthorized clear attempts are rejected.
- Recovery requires completing the full checklist in
  `docs/emergency-procedures.md` before resuming.

### Multi-Signature Deployment Requirement

- Before any live capital is committed, both `Executor.yul` (owner = slot 0)
  and `FundDistributor.sol` (owner) **must** be owned by a multisig wallet with
  deployed code.
- `contracts/script/Deploy.s.sol` enforces multisig-owned deployment at the
  Forge script level.
- Single-EOA ownership is permitted only in shadow/testing mode.

### EOA Pool Rotation

- Worker EOAs carry small gas-only balances and are treated as disposable.
- `scripts/rotate_eoa.py` performs round-robin rotation with a cooldown period.
- If funds are at risk, `scripts/rotate_wallet.py` can rotate the active hot
  wallet immediately.

---

## Dependency Security

### Continuous Monitoring

The validation gate (AGENTS.md) and CI pipeline enforce:

```bash
cargo audit         # Rust dependency CVE scan (RustSec advisory DB)
cargo deny check    # License + duplicate + advisory checks
pip-audit           # Python dependency CVE scan
osv-scanner -r .    # Multi-language OSV database scan
```

Outputs are archived in `audits/osv-scanner.json` and `audits/pip-audit.json`.

### Static Analysis

- **Slither** — `slither contracts --config-file slither.config.json` runs on
  the Solidity/Yul contracts. A CI job is configured; SARIF output can be
  written to `ai-audit/queue/slither.sarif` for structured review.
- **Foundry tests** — `forge test --root contracts/` is required before every
  commit. All executor contract changes require a corresponding test in
  `contracts/test/` (AGENTS.md invariant #6).
- **`cargo test -p chimera-core`** — Rust unit and integration tests must pass.

### Accepted Risks

Known dependency advisories (e.g., RUSTSEC-2024-0437) are documented with
rationale in `docs/research/dependency-cve-triage.md`. Each advisory is
re-evaluated before every live milestone as part of the `cargo audit` run.

---

## Known Risks

The following risks are documented in the threat model
(`docs/threat-model.md`) and remain partially or fully open:

### Flash Loan Atomicity Assumptions

The Executor contract relies on Aave V3's single-transaction flash-loan
callback. If the callback is invoked outside the expected atomic context,
or if the Aave pool address changes without an on-chain update, the
execution could fail or be exploited. Mitigations: pool validation against
slot 1, initiator check, exact 288-byte params. Status: **Mitigated**
(see threat model §4).

### Oracle Staleness Window

The engine uses Chainlink oracles with an `oracle_staleness_seconds`
threshold. A stale price could cause the simulator to compute incorrect
profitability. Mitigation: staleness guard with configurable window;
profit gate is enforced on-chain regardless of oracle data. Status:
**Partial** (see threat model §5).

### MEV Competition / Frontrunning

Submissions go through a standard RPC provider. Searchers can observe the
mempool and front-run liquidations. Mitigation: profit gate with `tip`
parameter makes theft self-defeating for the attacker, but does not
prevent it. **Private submission (Flashbots/MEV-Share) is planned but not
yet wired.** Status: **Open** (see threat model §6, risk #4).

### L2 Sequencer Risks

Base and Arbitrum sequencers can reorder, delay, or censor transactions.
Mitigation: atomic all-or-nothing execution, swap deadline parameters, and
engine halts on stall detection rather than retrying blindly. Status:
**Partial** (see threat model §4).

### Host Compromise

Full compromise of the operator host can expose the keystore password and
`CHIMERA_OPERATOR_TOKEN`. This is treated as catastrophic. Blast radius is
narrowed (small worker balances, cold treasury, multisig-owned contracts)
but not eliminated. Status: **Open** (see threat model §7, risk #1).

---

## Audit Status

| Assessment Type       | Status               | Details                              |
| --------------------- | -------------------- | ------------------------------------ |
| Self-review (Waves 1–5) | :white_check_mark: Complete | Security + wiring remediation pass (see README §Development Status) |
| Threat model          | :white_check_mark: Complete | `docs/threat-model.md` — STRIDE analysis, risk matrix, residual risks |
| ai-audit pipeline     | :white_check_mark: Available | `ai-audit/scripts/run_audit.py` — Slither + local Ollama contract scanner (auxiliary, independent of the funds path) |
| Foundry tests         | :white_check_mark: Implemented | `forge test --root contracts/` covers owner gate, pool validation, overflow guards, `transferOwnership`, lazy-init hardening |
| Third-party audit     | :x: Not conducted     | No external firm has audited this codebase                   |

### Pre-Live Requirements

Before any live execution with real capital, the following must be completed:

1. Full toolchain validation (`cargo test`, `forge test`) on a developer
   workstation — **not yet run in the remediation environment**.
2. `slither contracts --config-file slither.config.json` — static analysis pass.
3. Dependency CVE triage (`cargo audit`, `pip-audit`, `osv-scanner`) — resolve
   or document-accept all findings.
4. Mandatory 7-day shadow soak with clean metrics.
5. Multisig ownership deployed for both `Executor` and `FundDistributor`.
6. Encrypted keystore provisioned outside the repository.
7. Private transaction submission (Flashbots/MEV-Share) evaluated and enabled.
8. FundDistributor reentrancy guard added (deferred — see threat model §5).

Consult `docs/threat-model.md` for the full risk matrix and
`docs/emergency-procedures.md` for incident response workflows.
