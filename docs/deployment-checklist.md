# Project Chimera — Deployment Checklist

> **Read first.** This checklist gates the path from a clean checkout to live
> execution. It is ordered: do not skip ahead. The engine is **shadow-mode-first
> and never auto-flips to live** — the only way to go live is to satisfy every
> gate below and then explicitly run `toggle_shadow.py --set-live`, which itself
> enforces the 7-day soak rule.
>
> The live path uses a standalone multisig-owned Executor. Worker EOAs sign
> ordinary EIP-1559 calls to `execute(bytes)`; there is no EIP-7702/delegation.
> Private/protected submission is not wired, so the current path broadcasts raw
> transactions to the configured standard RPC.
>
> Nothing in this repo has been validated end-to-end with the full toolchain in
> this environment. Run the Pre-flight gate on a workstation with Rust, Foundry,
> and Slither before trusting any output.

---

## 1. Pre-flight (clean checkout)

Run from a clean clone on a full-toolchain workstation. Every box must be green
or explicitly documented before moving on.

- [ ] `cargo test -p chimera-core` — green
- [ ] `cargo test -p chimera-core test_load_valid_config` — config-load test passes (unit test in `core/src/config.rs`, not a separate integration test)
- [ ] `forge test --root contracts/ -vvv` — green, including:
  - [ ] owner/worker auth tests (`execute`, `setPool`, `setWorker`, `withdraw`)
  - [ ] withdraw path tests (ETH + ERC20, USDT-style no-return)
  - [ ] profit-gate overflow tests (`minProfit`/`tip` add-overflow guards)
  - [ ] Executor atomic-flow and callback validation tests
  - [ ] constructor-owner test (owner set from appended arg)
  - [ ] `transferOwnership` tests (happy path + zero-address rejection)
  - [ ] pool-validation test (`executeOperation` rejects non-pool caller)
  - [ ] initiator-validation test (`executeOperation` rejects initiator other than Executor)
  - [ ] worker authorization tests (`setWorker` / `isWorker`)
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
- [ ] Call `setPool(<canonicalAaveV3Pool>)` **from the multisig**; verify with the public getter:
      ```bash
      cast call <EXECUTOR_ADDRESS> "pool()(address)" --rpc-url <RPC_URL>
      ```
- [ ] From the multisig, call `setWorker(<worker>, true)` for **every active worker**; verify each with `isWorker(address)`
- [ ] Verify Executor on the block explorer (source + construction owner argument)
- [ ] Smoke test on testnet: one end-to-end liquidation via flash loan; confirm `Profit` event and profit gate behavior
- [ ] Confirm the worker sends an ordinary EIP-1559 transaction to Executor `execute(bytes)`, not to Aave Pool and not via delegation
- [ ] Confirm Executor calls `flashLoanSimple` with itself as receiver and rejects a non-pool callback or non-Executor initiator
- [ ] Confirm an unauthorized caller is rejected (`execute`, `setPool`, `setWorker`, `withdraw`)
- [ ] Repeat owner/pool/worker authorization verification on **mainnet** addresses before §5

---

## 3. Operational Setup

- [ ] Encrypted keystore created **outside the repo**; never committed (confirm `.gitignore`)
- [ ] `CHIMERA_TREASURY_KEYSTORE`, `CHIMERA_WORKER_KEYSTORE_DIR`, and `CHIMERA_KEYSTORE_PASSWORD` set in the protected host environment; there is no `CHIMERA_KEYSTORE_PATH` requirement
- [ ] `CHIMERA_EXECUTOR_ADDRESS`, `CHIMERA_TREASURY_ADDRESS`, and `CHIMERA_EOA_POOL_PATH` set to deployment-local values
- [ ] `CHIMERA_OPERATOR_TOKEN` set (gates `clear_breaker`); strong, not reused
- [ ] RPC env set (`BASE_RPC_URL` or fallback `RPC_URL`); credentials are not committed or logged
- [ ] EOA pool populated with **public addresses only** using `wallets`; checked-in placeholders replaced or excluded and never funded
- [ ] Treasury keystore address equals `CHIMERA_TREASURY_ADDRESS`; treasury is not an active worker
- [ ] Every active EOA-pool address has a matching encrypted worker keystore
- [ ] Treasury and workers funded with **gas ETH only**; Aave flash loans supply liquidation capital
- [ ] `check_balances.py` confirms each active worker is at or above `min_worker_balance_eth` and the treasury has ETH for refunds
- [ ] `config/pacing.yaml` reviewed — caps sane and conservative:
  - [ ] `max_daily_net_usd` (default $2,000)
  - [ ] `max_weekly_net_usd` (default $7,500)
  - [ ] `max_single_transfer_usd` (default $1,000)
  - [ ] `max_daily_loss_eth` (default 0.005 ETH)
  - [ ] `min_profit_multiplier` (default 2.5x)
  - [ ] `auto_halt_on_reverts` (default 3) and `max_gas_gwei` (default 300)
  - [ ] `execute_mode: shadow`
- [ ] Snapshot generated via `snapshot_generator.py` (`--mock` first, then real RPC)
- [ ] Monitoring up: Prometheus + Grafana (`localhost:3002`) + Alertmanager; dashboard provisioned; metrics endpoint responds on the computed port
  - [ ] **Important:** The actual metrics port is `9100 + (chain_id % 1000)`, e.g. `:9553` for Base (chain_id=8453). It is NOT a flat `:9100` in the default binary configuration. Verify the port in `config/pacing.yaml` or with `curl` against the running process.
- [ ] `chimera_breaker_state` reads `0` at startup
- [ ] Run `health_check.py --rpc <RPC_URL> --metrics-port <ACTUAL_PORT>` — there is no `--full` flag; use the flags above
- [ ] Run `check_balances.py --chain base --min-balance 0.01` to verify worker balances (default `--min-balance` is `0.0`; explicitly set the threshold you expect)
- [ ] Confirm Rust `SweepScheduler` behavior in shadow/testnet: excess **native ETH** sweep and worker refund; `sweep_tokens` is unused and ERC20 sweeping is not implemented there

---

## 4. Shadow Soak (mandatory)

The soak is non-negotiable. It is the primary evidence that the system behaves
as designed before any capital is at risk.

- [ ] Run shadow mode for **≥ 7 continuous days**
- [ ] **Zero unexpected breaker trips** over the window (any trip → investigate, do not paper over)
- [ ] Simulated profit within **10%** of historical, cross-checked against `fetch_historical_liquidations.py`
- [ ] Crash-restart recovery verified: kill the process, restart, confirm state restored via `recover_state.py`; verify recovered state against on-chain data
- [ ] Emergency drill: `emergency_pause.py --state-file core/state/emergency.flag --reason "drill"` trips the breaker in **≤ 5s**, visible as `chimera_breaker_state 1`, with an incident entry written
- [ ] Confirm breaker auto-trip conditions fire in shadow (3+ reverts, gas > 300 gwei, daily loss ≥ 0.005 ETH) via controlled simulation
- [ ] `status.py` shows healthy, incrementing candidate/sim counters and no `ERROR` logs

---

## 5. Go-Live Gate

Only enter this section after §1–4 are fully satisfied.

- [ ] Soak evidence archived (metrics export + log summary for the 7-day window)
- [ ] Standalone Executor bytecode and multisig ownership re-confirmed on mainnet (§2)
- [ ] Executor `pool()` equals the canonical Aave Pool and `isWorker` is true for every active worker
- [ ] Treasury signer/address parity, EOA-pool/signer parity, and live funding checks pass
- [ ] `docs/emergency-procedures.md` reviewed by the operator on duty
- [ ] Rollback path documented and the operator can execute it from memory (§6)
- [ ] Run `toggle_shadow.py --set-live` — **this enforces the 7-day rule**; if it refuses, the soak is not satisfied, stop
- [ ] Start with minimum viable **gas ETH** for treasury and workers; do not deposit trading/liquidation capital
- [ ] Watch the first ~10 live ops at reduced sizing; confirm inclusion > 85% and no breaker trips
- [ ] Explicitly assess the unwired private/protected-submission risk before real money; current raw transactions use the standard RPC
- [ ] Verify debt-token profit remains in Executor, then consolidate it with multisig `withdraw(token, amount)`
- [ ] Verify the Rust scheduler manages worker native gas ETH; do not treat `scripts/sweep_profits.py` as the Executor-profit or primary scheduled path

---

## 6. Rollback / Abort

Any operator must be able to stop the system and protect funds quickly. Practice
this in the §4 drill.

- [ ] **Halt now:** `emergency_pause.py --state-file core/state/emergency.flag --reason "<what you saw>"` (cancels pending, sets pause flag, writes incident)
- [ ] **Return to safe mode:** `toggle_shadow.py --set-shadow` (logs only, no execution)
- [ ] **Recover funds:**
  - [ ] Executor: `withdraw(token, amount)` from the multisig owner (amount `0` = full balance; `token=0` = native ETH)
- [ ] **Disable/revoke:** multisig calls `setWorker(worker, false)` for affected workers and may call `setPool(address(0))` to disable Executor execution
- [ ] **Rotate exposure:** `rotate_eoa.py` (pool hygiene) and, if keys may be compromised, rotate keys manually
- [ ] Do not clear the breaker until root cause is resolved; `clear_breaker` requires `CHIMERA_OPERATOR_TOKEN` by design
- [ ] File an incident using the template in `docs/emergency-procedures.md`; leave the system explicitly paused or in shadow — never ambiguous
