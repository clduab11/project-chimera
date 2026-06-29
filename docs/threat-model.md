# Project Chimera — Threat Model

> **Status & honesty note.** This threat model describes the system as it exists
> in the current repository: a **shadow-mode-first** liquidation MEV engine that
> has undergone a security + wiring remediation pass (Waves 1–4) but has **not**
> been independently audited and has **not** been validated end-to-end with the
> full toolchain in this environment. Mitigation claims below reflect code that
> has been written and self-reviewed. Where a control is partial, deferred, or
> merely planned, it is marked as such. Do not read "Mitigated" as "audited."
>
> This is a working threat model, not a certification. It should be revisited
> after the validation gate (`cargo test`, `forge test`, `slither`, dependency
> CVE triage) passes on a full-toolchain workstation and after the mandatory
> 7-day shadow soak.

---

## 1. System Overview & Trust Boundaries

Chimera combines a local Rust engine, two on-chain contracts, and a set of local
Python operator scripts. The core insight for threat modeling is that **the
funds path is gated at two layers**: the Rust pacing engine (off-chain, decides
*whether* to act) and the on-chain contracts (owner/pool-gated, decide *who* may
act). Neither layer trusts the network or the counterparties.

### 1.1 Components

| Component | Location | Trust level | Notes |
|---|---|---|---|
| Rust engine (`chimera-core`) | Local host | Trusted (operator-owned) | Detector, simulator, oracle, pacing engine, executor/submitter |
| Yul `Executor` | On-chain (Base/Arbitrum) | Semi-trusted, owner + pool gated | Flash-loan atomic liquidation; storage slot 0 = owner, slot 1 = pool |
| `FundDistributor.sol` | On-chain | Semi-trusted, `Ownable` two-step | Batch ETH top-ups to worker EOAs |
| Operator scripts (`scripts/`) | Local host | Trusted (operator-owned) | Funding, sweep, rotation, snapshot, emergency, health |
| Worker EOAs | Hot keys | Low trust (disposable) | Small gas-only balances; rotated for hygiene |
| Treasury | Cold wallet / multisig | Highest trust | Never touches the engine directly |
| Encrypted keystore | Local host (outside repo) | Highest trust | Operator's responsibility; never committed |
| RPC provider | External | **Untrusted** | Can lie, stall, censor, or front-run |
| DEX routers | On-chain, external | **Untrusted** | Arbitrary code; output validated by `amountOutMin` + profit gate |
| Aave V3 pool | On-chain, external | Semi-trusted protocol | Callback caller validated against slot 1 |

### 1.2 Trust boundary diagram (ASCII)

```
                            UNTRUSTED NETWORK / EXTERNAL
   +------------------------------------------------------------------+
   |   RPC provider     DEX routers     Aave V3 pool    MEV searchers  |
   |   (can lie/stall)  (arbitrary)     (protocol)      (competitors)  |
   +----------+----------------+----------------+---------------+------+
              |                |                |               |
   ===========|================|================|===============|=========
   TRUST       \               |   on-chain calls               |
   BOUNDARY A   \              v                v                |
   (local <->    \   +------------------------------------+      |
    on-chain)     \  |  ON-CHAIN (semi-trusted, gated)    |      |
                   \ |  Executor.yul   FundDistributor.sol|      |
                    \|  owner=slot0    Ownable two-step   |      |
                     |  pool =slot1    onlyOwner          |      |
                     +-----------------+------------------+      |
                                       ^                         |
   ====================================|=========================|====
   TRUST BOUNDARY B (host perimeter)   | signed txns             |
   +-----------------------------------|-------------------------|----+
   |  LOCAL HOST (trusted, operator-owned)                            |
   |                                                                  |
   |   +-------------------+      +----------------------------+      |
   |   |  Rust engine      |      |  Operator scripts (Python) |      |
   |   |  Detector/Sim/    |      |  fund_eoa, sweep_profits,  |      |
   |   |  Oracle/PACING <--+------+  emergency_pause, rotate,  |      |
   |   |  Executor/Submit  | flag |  snapshot, health_check    |      |
   |   +---------+---------+      +-------------+--------------+      |
   |             |                              |                     |
   |   ==========|=== TRUST BOUNDARY C =========|=========           |
   |             v   (key custody)              v                     |
   |   +-------------------+        +------------------------+        |
   |   | Encrypted keystore|        | Worker EOAs (hot,      |        |
   |   | (operator secret) |        | disposable, gas-only)  |        |
   |   +-------------------+        +------------------------+        |
   |                                                                  |
   +------------------------------------------------------------------+
                                       ^
                                       | cold transfers only
                            +----------+-----------+
                            |  Treasury / multisig |  (highest trust)
                            +----------------------+
```

**Boundary A — local ↔ on-chain.** Everything crossing this boundary is a signed
transaction or a read. On-chain contracts re-validate authority (owner/pool)
and economics (profit gate) because the local side cannot be assumed correct by
the chain, and the chain/counterparties cannot be assumed correct by the local
side.

**Boundary B — host perimeter.** If the host is compromised, the keystore
password and operator token are reachable; this is treated as a catastrophic,
out-of-scope-to-fully-mitigate event (see Residual Risks). The design narrows
blast radius (small worker balances, multisig-owned contracts) rather than
claiming host compromise is prevented.

**Boundary C — key custody.** Private keys live in an encrypted keystore outside
the repo. `config/eoa_pool.json` holds **public addresses only**.

---

## 2. Assets

| Asset | Why it matters | Where it lives |
|---|---|---|
| Worker EOA funds | Directly spendable hot ETH (gas) | Worker keys (hot) |
| Treasury funds | The bulk of capital | Cold wallet / multisig |
| Encrypted keystore / private keys | Compromise = total loss of controllable funds | Local host, outside repo |
| Executor transient token balances | Mid-transaction debt/collateral tokens held by `Executor` | On-chain, during the atomic op |
| Operator token (`CHIMERA_OPERATOR_TOKEN`) | Gates `clear_breaker`; protects the breaker from unauthorized clears | Env var on host |
| Config integrity | `pacing.yaml` caps/breakers are the financial guardrails; drift removes protection | `config/`, mirrored in `config.rs` |

---

## 3. Threat Actors

| Actor | Capability | Primary goal |
|---|---|---|
| External on-chain attacker | Submit arbitrary txns, deploy contracts | Drain Executor / FundDistributor, grief liquidations |
| Malicious DEX router / token | Arbitrary code at a called address; nonstandard ERC20 | Steal mid-tx balances, break profit accounting |
| Compromised RPC provider | Lie about state, withhold/delay txns, leak mempool | Induce bad decisions, censor, enable front-run |
| MEV searcher / competitor | Observe mempool, front-run, back-run | Steal the liquidation, sandwich the swap |
| Local host compromise | Read host memory/files, run as operator | Exfiltrate keys, password, operator token |
| Insider / operator error | Legitimate access, mistakes | Misconfigure caps, fund wrong address, clear breaker prematurely |
| Sequencer (Base/Arbitrum) | Order/stall/censor L2 inclusion | Stall execution, strand in-flight strategy |

---

## 4. STRIDE Attack Trees / Scenarios

Each row maps a STRIDE category to a concrete Chimera scenario and the control
that addresses it. "Slot 0/1" refer to `Executor.yul` storage.

| STRIDE | Scenario | Control / mitigation | Status |
|---|---|---|---|
| **Spoofing** | Attacker contract calls `executeOperation` pretending to be the Aave pool | Pool validation: `caller()` must equal `sload(1)`; else `InvalidPool` revert (`Executor.yul` CASE A) | Mitigated |
| **Spoofing** | Attacker calls `exec`, `setPool`, `withdraw`, or `transferOwnership` as if owner | Owner gate: `caller()` must equal `sload(0)`; else `Unauthorized` revert | Mitigated |
| **Spoofing** | Lazy-init owner hijack: first caller becomes owner if slot 0 == 0 | Constructor sets owner from appended arg; deploy script must set owner to multisig at construction; `transferOwnership` rejects zero address to prevent re-opening lazy-init | Partial (depends on correct deploy + verification step) |
| **Tampering** | `pacing.yaml` edited to weaken caps; drifts from `config.rs` defaults | 3-way sync invariant (AGENTS.md #1): `pacing.yaml` ↔ `config.rs` defaults ↔ `valid_yaml()` fixture; CI/test must catch drift | Partial (enforced by test that must be run) |
| **Tampering** | Snapshot poisoning: malicious Aave state fed to simulator | Snapshot schema contract (`docs/snapshot-schema.md` ↔ `prewarm.rs`); simulation re-derives economics; profit gate is on-chain regardless | Partial |
| **Repudiation** | No record of why a decision/execution happened | JSONL audit trail (`StatePersistence`) logs every outcome with id, timestamp, venue, eoa, chain_id | Mitigated (library; wiring into `main.rs` still partial — see README §9) |
| **Information disclosure** | Keystore password or key leaked via logs | Password/keys never logged; logs go to stdout; keystore lives outside repo; `.gitignore` keeps secrets untracked | Partial (operator-dependent; no automated secret-scan gate yet) |
| **DoS** | RPC timeout / transient failure stalls the loop | Retry/backoff on the submitter path; conservative behavior on read failure | Partial |
| **DoS** | L2 sequencer stall / censorship strands a strategy | Atomic all-or-nothing op (no partial state); deadline param on swaps; engine halts rather than retries blindly | Partial (cannot prevent sequencer behavior; limits damage) |
| **DoS** | Dependency advisory (e.g. RUSTSEC-2024-0437) in the tree | Risk-accepted with rationale (see §6); tracked via `cargo audit` in the validation gate | Accepted (documented) |
| **Elevation** | Unauthorized `exec(bytes)` to run an arbitrary strategy | Owner gate on `exec` (slot 0) | Mitigated |
| **Elevation** | Unauthorized `withdraw` of Executor balances | Owner gate on `withdraw`; funds go only to `sload(0)` owner | Mitigated |
| **Elevation** | Unauthorized `clear_breaker` to resume after a halt | `CHIMERA_OPERATOR_TOKEN` env gate; refuses if unset or mismatched | Mitigated |
| **Elevation** | `transferOwnership(0)` re-enables lazy-init hijack | Zero-address rejection in `transferOwnership` (Executor) and `ZeroAddress` revert in FundDistributor | Mitigated |

---

## 5. DeFi-Specific Attack Vectors

| Vector | Description | Mitigation in code | Status |
|---|---|---|---|
| Reentrancy (Executor) | Re-enter during the atomic op | Single atomic flash-loan flow; profit gate and accounting computed from `balanceOf` after steps; no external calls after state is finalized | Mitigated |
| Reentrancy (FundDistributor) | Recipient `.call{value}` re-enters `distribute`/`emergencyWithdraw` | **Known gap:** no `nonReentrant` guard. `onlyOwner` limits the caller to a trusted multisig and there is no post-transfer state to corrupt, but a guard is **deferred** and should be added before live | Open (deferred, documented) |
| Profit-gate overflow | `balanceBefore + minProfit + tip` overflows to bypass the gate | Explicit add-overflow guards: each sum checked `lt(sum, prev)` → `ProfitGateFailed` (both CASE A and CASE B) | Mitigated |
| Donation / price manipulation | Inflate balances or manipulate swap price to fake profit | Profit gate requires `balanceAfter > balanceBefore + minProfit + tip`; `amountOutMin` bounds the swap; oracle staleness window (`oracle_staleness_seconds`) | Partial |
| USDT no-return-value | Tokens that return nothing on `approve`/`transfer` | `callApprove` / `withdraw` treat empty returndata as success and only fail on explicit `false` | Mitigated |
| Nonce collision in funding | Concurrent funding txns reuse a nonce | Addressed in funding path; sequential nonce handling | Mitigated (verify under load) |
| Bad-debt liquidation | Liquidating a position that leaves uncovered bad debt | **Must-not-attempt:** simulator screens bad-debt coverage before submission; engine declines | Partial (relies on simulator fidelity) |
| MEV front-running | Searcher steals the liquidation or sandwiches the swap | `tip` parameter and profit gate make unprofitable theft self-defeating for us; **private submission is not yet wired** — current state submits via standard RPC | Open (private submission planned) |
| Flash-loan callback abuse | Attacker triggers `executeOperation` with crafted params | Pool validation (slot 1) + params length checks (≥288 bytes) + atomic revert on any failed step | Mitigated |

---

## 6. Residual Risks & Assumptions

These are **not** mitigated by code and must be tracked operationally:

1. **Toolchain validation not yet run here.** `cargo test`, `forge test`,
   `slither`, and dependency CVE triage have not been executed in the
   remediation environment. Nothing below "Mitigated" should be trusted until
   the validation gate passes on a full-toolchain workstation.
2. **7-day shadow soak required.** No shadow-to-live transition before a clean
   7-day soak (enforced in `validate_mode_transition()`).
3. **Multisig ownership required for live.** Both `Executor` (slot 0) and
   `FundDistributor` (`owner`) must be owned by a multisig with deployed code
   before any live capital.
4. **Keystore security is the operator's responsibility.** The system cannot
   protect keys against a compromised host or a weak keystore password.
5. **Single-host trust.** Compromise of the local host is treated as
   catastrophic. Blast radius is narrowed (small worker balances, cold
   treasury, multisig contracts) but not eliminated.
6. **RUSTSEC-2024-0437 risk-accept.** Carried as an accepted dependency
   advisory with rationale documented in the dependency CVE triage; re-evaluate
   on each `cargo audit` run before a live milestone.
7. **FundDistributor reentrancy guard deferred.** See §5; add before live.

---

## 7. Risk Matrix (Top 10)

Likelihood × Impact, with current mitigation and status. Scale: Low / Med /
High. Status: Mitigated / Partial / Accepted / Open.

| # | Risk | Likelihood | Impact | Current mitigation | Status |
|---|---|---|---|---|---|
| 1 | Host compromise → key/keystore exfiltration | Low | High | Out-of-repo keystore, small hot balances, multisig contracts; no in-code defense | Open |
| 2 | Lazy-init owner hijack on Executor (mis-deploy) | Low | High | Constructor owner arg + zero-addr reject + mandatory owner/pool verification step | Partial |
| 3 | Config drift weakens caps/breakers | Med | High | 3-way sync invariant + test fixture; CI must enforce | Partial |
| 4 | MEV front-running steals liquidation | High | Med | Profit gate + tip; private submission not yet wired | Open |
| 5 | Malicious DEX router/token drains mid-tx balance | Low | High | `amountOutMin`, profit gate, atomic revert, USDT-safe calls | Partial |
| 6 | Compromised/lying RPC induces bad action or censors | Med | Med | Retry/backoff, oracle staleness, on-chain re-validation | Partial |
| 7 | FundDistributor reentrancy (no guard) | Low | Med | `onlyOwner` (multisig) + no post-transfer state; guard deferred | Open |
| 8 | Sequencer stall strands in-flight strategy | Med | Med | Atomic all-or-nothing + swap deadline; engine halts | Partial |
| 9 | Operator error (premature breaker clear, bad fund) | Med | Med | Operator-token gate on `clear_breaker`, dry-run flags, onboarding rules | Partial |
| 10 | Bad-debt liquidation attempted | Low | Med | Simulator bad-debt screen; must-not-attempt rule | Partial |

**Top risks to watch before live:** (1) host/key custody — the single largest
unmitigated impact; (4) MEV front-running — high likelihood and unaddressed in
the current submission path; and (3) config drift — the cheapest way to silently
disable every financial guardrail. None of these are closed by code today; all
three are gated by process (validation gate, soak, multisig, private submission)
rather than claimed as solved.
