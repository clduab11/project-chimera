# Project Chimera — Deployment Checklist

> **Read first.** This checklist gates the path from a clean checkout to live
> execution. It is ordered: do not skip ahead. The engine is **shadow-mode-first
> and never auto-flips to live** — the only way to go live is to satisfy every
> gate below and then explicitly run `toggle_shadow.py --set-live`, which itself
> enforces the 7-day soak rule.
>
> Honesty note: some referenced helpers are fully wired and some are still
> partial (e.g. `sweep_profits.py` currently prints a ready message rather than
> sweeping; `fund_eoa.py` has a known `wallets` vs `workers` JSON-shape
> mismatch). Treat a partial helper as "verify/extend before relying on it,"
> not "done." See `docs/first-run-onboarding.md` §8–10.
>
> Nothing in this repo has been validated end-to-end with the full toolchain in
> this environment. Run the Pre-flight gate on a workstation with Rust, Foundry,
> and Slither before trusting any output.

---

## 1. Pre-flight (clean checkout)

Run from a clean clone on a full-toolchain workstation. Every box must be green
or explicitly documented before moving on.

- [ ] `cargo test -p chimera-core` — green
- [ ] `forge test --root contracts/ -vvv` — green, including:
  - [ ] owner/auth tests (`exec`, `setPool`, `withdraw` reject non-owner)
  - [ ] withdraw path tests (ETH + ERC20, USDT-style no-return)
  - [ ] profit-gate overflow tests (`minProfit`/`tip` add-overflow guards)
  - [ ] reentrancy tests (Executor atomic flow; FundDistributor known-gap noted)
  - [ ] constructor-owner test (owner set from appended arg)
  - [ ] `transferOwnership` tests (happy path + zero-address rejection)
  - [ ] pool-validation test (`executeOperation` rejects non-pool caller)
- [ ] `python -m compileall scripts ai-audit/scripts` — clean
- [ ] `cargo audit` — clean or each advisory documented (incl. RUSTSEC-2024-0437 risk-accept rationale)
- [ ] `slither contracts --config-file slither.config.json` — clean or findings triaged
- [ ] `pip-audit` — clean or documented
- [ ] `osv-scanner -r .` — clean or documented
- [ ] CI green on the target commit, including the shadow-guard check
- [ ] AGENTS.md invariant #1 verified: `config/pacing.yaml` ↔ `config.rs` defaults ↔ `valid_yaml()` fixture all match

---

## 2. Contract Deployment (testnet first)

Deploy and exercise on testnet before any mainnet deployment.

- [ ] Deploy via `contracts/script/Deploy.s.sol` to **Base Sepolia** and/or **Arbitrum Sepolia**
- [ ] Confirm Executor `owner()` == the intended **multisig** address
- [ ] Confirm `multisig.code.length > 0` (owner is a deployed contract, not an EOA typo)
- [ ] Call `setPool(<aaveV3Pool>)` **from the multisig**; confirm slot 1 reads back the correct pool
- [ ] `FundDistributor`: constructor `_owner` set; then `transferOwnership(multisig)`
- [ ] `FundDistributor`: multisig calls `acceptOwnership`; confirm `owner` == multisig and `pendingOwner` == 0
- [ ] Verify both contracts on the block explorer (source + constructor args)
- [ ] Smoke test on testnet: one end-to-end liquidation via flash loan; confirm `Profit` event and profit gate behavior
- [ ] Confirm an unauthorized caller is rejected (non-owner `exec`/`withdraw`, non-pool `executeOperation`)
- [ ] Repeat the owner/pool verification on **mainnet** addresses before §5

---

## 3. Operational Setup

- [ ] Encrypted keystore created **outside the repo**; never committed (confirm `.gitignore`)
- [ ] `CHIMERA_KEYSTORE_PATH` and `CHIMERA_KEYSTORE_PASSWORD` set in the host env
- [ ] `CHIMERA_OPERATOR_TOKEN` set (gates `clear_breaker`); strong, not reused
- [ ] RPC env(s) set (`RPC_URL`; multi-chain `BASE_RPC_URL`/`ARB_RPC_URL` is partial — see onboarding §9)
- [ ] `config/eoa_pool.json` populated with **public addresses only**
- [ ] Worker EOAs funded via `fund_eoa.py` (verify the `wallets` vs `workers` JSON-shape mismatch is resolved first; otherwise fund manually, ~0.02 ETH/worker)
- [ ] `check_balances.py` confirms worker balances are gas-sized and treasury is cold
- [ ] `config/pacing.yaml` reviewed — caps sane and conservative:
  - [ ] `max_daily_net_usd` (default $2,000)
  - [ ] `max_weekly_net_usd` (default $7,500)
  - [ ] `max_single_transfer_usd` (default $1,000)
  - [ ] `max_daily_loss_eth` (default 0.005 ETH)
  - [ ] `min_profit_multiplier` (default 2.5x)
  - [ ] `auto_halt_on_reverts` (default 3) and `max_gas_gwei` (default 300)
  - [ ] `execute_mode: shadow`
- [ ] Snapshot generated via `snapshot_generator.py` (`--mock` first, then real RPC)
- [ ] Monitoring up: Prometheus + Grafana (`localhost:3002`) + Alertmanager; dashboard provisioned; metrics endpoint responds on `:9100`
- [ ] `chimera_breaker_state` reads `0` at startup
- [ ] `health_check.py --full` — PASS

---

## 4. Shadow Soak (mandatory)

The soak is non-negotiable. It is the primary evidence that the system behaves
as designed before any capital is at risk.

- [ ] Run shadow mode for **≥ 7 continuous days**
- [ ] **Zero unexpected breaker trips** over the window (any trip → investigate, do not paper over)
- [ ] Simulated profit within **10%** of historical, cross-checked against `fetch_historical_liquidations.py`
- [ ] Crash-restart recovery verified: kill the process, restart, confirm state restored via `recover_state.py`; verify recovered state against on-chain data
- [ ] Emergency drill: `emergency_pause.py` trips the breaker in **≤ 5s**, visible as `chimera_breaker_state 1`, with an incident entry written
- [ ] Confirm breaker auto-trip conditions fire in shadow (3+ reverts, gas > 300 gwei, daily loss ≥ 0.005 ETH) via controlled simulation
- [ ] `status.py` shows healthy, incrementing candidate/sim counters and no `ERROR` logs

---

## 5. Go-Live Gate

Only enter this section after §1–4 are fully satisfied.

- [ ] Soak evidence archived (metrics export + log summary for the 7-day window)
- [ ] Multisig ownership re-confirmed for both contracts on mainnet (§2)
- [ ] `docs/emergency-procedures.md` reviewed by the operator on duty
- [ ] Rollback path documented and the operator can execute it from memory (§6)
- [ ] Run `toggle_shadow.py --set-live` — **this enforces the 7-day rule**; if it refuses, the soak is not satisfied, stop
- [ ] Start with **small capital** (minimum viable worker balances) for the first live window
- [ ] Watch the first ~10 live ops at reduced sizing; confirm inclusion > 85% and no breaker trips
- [ ] Sweep first profits with a manual/verified path (do not rely on the `sweep_profits.py` skeleton unsupervised)

---

## 6. Rollback / Abort

Any operator must be able to stop the system and protect funds quickly. Practice
this in the §4 drill.

- [ ] **Halt now:** `emergency_pause.py --reason "<what you saw>"` (cancels pending, sets pause flag, writes incident)
- [ ] **Return to safe mode:** `toggle_shadow.py --set-shadow` (logs only, no execution)
- [ ] **Recover funds:**
  - [ ] Executor: `withdraw(token, amount)` from the multisig owner (amount `0` = full balance; `token=0` = native ETH)
  - [ ] FundDistributor: `emergencyWithdraw()` from the multisig owner
- [ ] **Rotate exposure:** `rotate_eoa.py` (pool hygiene) and, if keys may be compromised, `rotate_wallet.py`
- [ ] Do not clear the breaker until root cause is resolved; `clear_breaker` requires `CHIMERA_OPERATOR_TOKEN` by design
- [ ] File an incident using the template in `docs/emergency-procedures.md`; leave the system explicitly paused or in shadow — never ambiguous
