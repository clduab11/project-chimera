# Project Chimera - Keystore, Multisig & Go-Live Runbook

**Version**: 2.0
**Last Updated**: 2026-07-09
**Audience**: Operator controlling the gas treasury signer and Executor-owner multisig.
**Prerequisites**: `docs/deployment-checklist.md` sections 1-4 complete.

> Operator-executed runbook. Commands marked `[LIVE]` change on-chain state or
> start live execution. Do not put passwords or private keys in shell arguments,
> process lists, logs, repository files, or command transcripts.

## 1. Implemented Model

- Worker EOAs sign ordinary EIP-1559 transactions targeting the configured
  standalone Executor's `execute(bytes)` entrypoint. There is no EIP-7702 or
  delegation model.
- Executor calls its configured Aave Pool's `flashLoanSimple` with itself as
  receiver. The callback requires `caller() == pool()` and
  `initiator == Executor`.
- Executor is owned at construction by the multisig. The multisig must call
  `setPool(canonicalPool)` and `setWorker(worker, true)` for every active worker.
- Aave supplies liquidation capital. Treasury and worker deposits are native
  gas ETH only.
- Successful debt-token profit remains in Executor. The multisig consolidates
  it through `withdraw(token, amount)`.
- The Rust `SweepScheduler` is the primary worker-gas path: excess native ETH
  moves from workers to the gas treasury, and underfunded workers receive
  refunds. It does not sweep ERC20s; `sweep_tokens` is currently unused.
- Live liquidation and scheduler transactions are broadcast as raw EIP-1559
  transactions through the configured standard RPC. Private/protected
  submission is not wired and should be added before real-money operation.
- Foundry remains configured for Cancun.

## 2. Required Runtime Inputs

Keep the checked-in config in shadow mode with empty operator fields. Supply
deployment-local values through a protected environment file or equivalent
secret manager:

| Variable | Purpose |
|---|---|
| `BASE_RPC_URL` or `RPC_URL` | Standard RPC used for reads and raw transaction submission |
| `CHIMERA_EXECUTOR_ADDRESS` | Deployed standalone Executor |
| `CHIMERA_TREASURY_ADDRESS` | Address derived from the treasury keystore |
| `CHIMERA_TREASURY_KEYSTORE` | Encrypted gas-treasury keystore outside the repo |
| `CHIMERA_WORKER_KEYSTORE_DIR` | Directory of encrypted active-worker keystores outside the repo |
| `CHIMERA_EOA_POOL_PATH` | Deployment-local EOA pool containing public worker addresses |
| `CHIMERA_KEYSTORE_PASSWORD` | Shared keystore password, loaded from the environment only |

There is no `CHIMERA_KEYSTORE_PATH` requirement. `CHIMERA_OPERATOR_TOKEN` is
also required for breaker-clear operations. Do not print either secret.

Use the no-key-export workflow in `docs/runbook-wallet-provisioning.md` to create
and verify keystores. Do not use examples that pass treasury or worker private
keys on a CLI. The treasury keystore must decrypt to
`CHIMERA_TREASURY_ADDRESS`; the treasury must not also be a worker.

Checklist:

- [ ] Keystores are outside the repository and readable only by the runtime user
- [ ] `CHIMERA_KEYSTORE_PASSWORD` is supplied by a protected env file/secret manager
- [ ] `python scripts/provision_wallets.py --verify --eoa-pool <EOA_POOL_PATH>` passes
- [ ] EOA-pool placeholder addresses were replaced or excluded; placeholders were never funded
- [ ] Every active pool address has exactly one matching encrypted worker keystore
- [ ] No private key material exists under the repository root

## 3. Configure the Standalone Executor

Executor ownership is assigned to the multisig during construction. Perform
owner actions from the multisig UI, hardware-wallet workflow, or reviewed
calldata. The read-only commands below use addresses only.

```bash
cast call <EXECUTOR_ADDRESS> "owner()(address)" --rpc-url "$BASE_RPC_URL"
cast call <EXECUTOR_ADDRESS> "pool()(address)" --rpc-url "$BASE_RPC_URL"
cast code <EXECUTOR_ADDRESS> --rpc-url "$BASE_RPC_URL"
```

The multisig must submit:

```text
[LIVE] Executor.setPool(<CANONICAL_AAVE_V3_POOL>)
[LIVE] Executor.setWorker(<ACTIVE_WORKER_ADDRESS>, true)  # repeat for every active worker
```

For a Safe transaction, select the Executor as the target and use its verified
ABI. If ABI entry is unavailable, generate calldata without signing locally:

```bash
cast calldata "setPool(address)" <CANONICAL_AAVE_V3_POOL>
cast calldata "setWorker(address,bool)" <ACTIVE_WORKER_ADDRESS> true
```

Paste the reviewed calldata into the multisig transaction builder; do not use a
raw owner key. Verify each authorization:

```bash
cast call <EXECUTOR_ADDRESS> "isWorker(address)(bool)" <ACTIVE_WORKER_ADDRESS> \
  --rpc-url "$BASE_RPC_URL"
```

Checklist:

- [ ] Executor has non-empty deployed bytecode on the intended chain
- [ ] `owner()` equals the intended deployed multisig
- [ ] `pool()` equals the canonical Aave V3 Pool for this chain
- [ ] `isWorker(worker)` is `true` for every non-excluded EOA-pool worker
- [ ] Retired/compromised workers return `false` after multisig `setWorker(worker, false)`
- [ ] Contract source and construction owner argument are verified on the explorer

## 4. Gas Funding Only

Initial funding is not strategy capital. Aave flash loans provide the debt token
used for liquidation. Fund only:

- the treasury signer with enough native ETH for worker refunds and refund gas;
- each active worker with enough native ETH to meet
  `min_worker_balance_eth` and submit transactions.

Use the multisig/wallet UI or the encrypted-keystore testnet/funding workflow;
never pass a private key on the command line. Verify public addresses directly:

```bash
cast balance <TREASURY_ADDRESS> --rpc-url "$BASE_RPC_URL"
cast balance <ACTIVE_WORKER_ADDRESS> --rpc-url "$BASE_RPC_URL"
python scripts/check_balances.py --chain base --min-balance 0.01
```

Live startup fails if the treasury balance is zero or any active worker is below
`min_worker_balance_eth`. Keep worker balances gas-sized; the Rust scheduler
returns excess native ETH to the treasury and tops up underfunded workers.

## 5. Mandatory Seven-Day Gate

Do not proceed without at least seven continuous days of shadow operation and
archived evidence. The engine never auto-flips to live.

```bash
python scripts/toggle_shadow.py --show
```

Required evidence:

- [ ] `_soak_satisfied` is `true`
- [ ] Metrics and logs cover at least seven continuous days
- [ ] No unexplained breaker trips remain open
- [ ] Crash recovery and emergency-pause drills passed
- [ ] Standard-RPC/public-orderflow risk was explicitly accepted, or protected submission was added and validated

Then run the explicit mode-state gate:

```bash
python scripts/toggle_shadow.py --set-live
```

Set `CHIMERA_EXECUTE_MODE=live` only in the deployment-local environment. Do
not commit `execute_mode: live` to `config/pacing.yaml`; `shadow-guard` protects
the checked-in shadow default.

## 6. Live Startup Preflight

Before starting:

```bash
test -n "$CHIMERA_KEYSTORE_PASSWORD"
test -n "$CHIMERA_OPERATOR_TOKEN"
python -m json.tool "$CHIMERA_EOA_POOL_PATH" >/dev/null
```

Start from the supervised operator terminal or service manager:

```bash
RUST_LOG=chimera=info ./target/release/chimera
```

Live startup is expected to fail closed unless all of these checks pass:

- configured Executor address is non-zero and has deployed bytecode;
- Executor `pool()` equals the runtime's canonical Aave Pool;
- every active EOA-pool worker has a matching decrypted signer;
- every active worker returns `true` from Executor `isWorker(address)`;
- the decrypted treasury signer equals `CHIMERA_TREASURY_ADDRESS`;
- treasury and worker roles are distinct;
- treasury native balance is non-zero;
- every active worker meets `min_worker_balance_eth`.

There is no fallback to `CHIMERA_KEYSTORE_PATH`. Treat any startup error as a
blocked go-live, not as a warning to bypass.

Checklist:

- [ ] `toggle_shadow.py --set-live` succeeded
- [ ] `CHIMERA_EXECUTE_MODE=live` exists only in deployment-local state
- [ ] Live Executor validation completed successfully
- [ ] Live funding validation completed successfully
- [ ] `chimera_breaker_state 0`
- [ ] Metrics endpoint responds on the configured/computed port
- [ ] No startup `ERROR` messages

## 7. First-Operations Watch

Supervise the first live window continuously. Keep the default conservative
caps and stop on unexpected behavior.

```bash
watch -n 10 'curl -s http://localhost:9553/metrics | grep -E "chimera_candidates|chimera_sims|chimera_breaker|chimera_profit|chimera_revert|chimera_sweep"'
```

Confirm:

- [ ] Submitted transactions are ordinary EIP-1559 calls to Executor `execute(bytes)`
- [ ] No worker transaction targets Aave Pool directly
- [ ] Revert count remains below the breaker threshold
- [ ] Gas and net-flow caps remain within configured limits
- [ ] Inclusion behavior is acceptable for the configured standard RPC
- [ ] Worker native-ETH sweep/refund metrics and logs match expected gas maintenance
- [ ] Debt-token profit accrues to Executor, not worker EOAs

If anything is unexpected:

```bash
python scripts/emergency_pause.py \
  --state-file core/state/emergency.flag \
  --reason "<operator incident summary>"
python scripts/toggle_shadow.py --set-shadow
```

Return the deployment-local execute mode to `shadow`, preserve evidence, and do
not clear the breaker until root cause is established.

## 8. Profit Consolidation

The normal profit path is multisig withdrawal from Executor:

```text
[LIVE] Executor.withdraw(<DEBT_TOKEN>, <AMOUNT>)
```

`amount = 0` withdraws Executor's full balance for that token to the multisig
caller. Generate reviewable calldata without a signing key:

```bash
cast calldata "withdraw(address,uint256)" <DEBT_TOKEN> <AMOUNT>
```

Checklist:

- [ ] Token and amount were independently reviewed against Executor balance
- [ ] Multisig transaction target is the configured Executor
- [ ] Multisig threshold approvals completed through the normal custody workflow
- [ ] Receipt succeeded and multisig token balance increased

Do not use `scripts/sweep_profits.py` for Executor profit. That script is an
implemented legacy/manual helper for worker-held balances, accepts raw key files
or CLI material, and does not use encrypted `SignerRegistry` keystores. It is
not the scheduled primary workflow. The Rust scheduler handles worker **native
gas ETH only** and does not sweep ERC20 tokens.

## 9. Rollback and Recovery

1. Write the emergency pause flag and set shadow mode as shown in section 7.
2. Stop the live process after preserving logs and transaction hashes.
3. From the multisig, revoke affected workers with
   `setWorker(worker, false)` and, if needed, disable execution with
   `setPool(address(0))`.
4. Use multisig `withdraw(token, amount)` to recover Executor-held assets.
5. Rotate compromised keystores and update the EOA pool; never reuse a
   compromised worker.
6. Re-run the full deployment checklist and shadow validation before resuming.

## 10. Cross-References

| Document | Purpose |
|---|---|
| `docs/deployment-checklist.md` | Full preflight, contract, soak, and live gates |
| `docs/runbook-wallet-provisioning.md` | Safe encrypted-keystore provisioning and parity verification |
| `docs/runbook-7day-soak.md` | Mandatory soak evidence |
| `docs/emergency-procedures.md` | Incident response and breaker recovery |
| `SECURITY.md` | Custody, submission, and residual-risk model |
