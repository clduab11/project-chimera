# Project Chimera — Testnet Deployment Runbook (Base Sepolia)

> **Operator-executed runbook — do not execute during authoring.**
>
> This document is the single source of truth for deploying Project Chimera to
> Base Sepolia testnet and verifying correctness. Every command listed here is
> written for the operator who will execute these steps on a provisioned
> workstation. Read the full runbook before starting. Do not skip steps.
>
> Cross-references:
> - Architecture overview: `docs/architecture.md`
> - Production deployment checklist: `docs/deployment-checklist.md`
> - Emergency procedures: `docs/emergency-procedures.md`
> - Onboarding guide: `docs/first-run-onboarding.md`
> - Validation gate: `AGENTS.md` §Validation Gate
> - Snapshot schema: `docs/snapshot-schema.md`

---

## 1. Purpose & Prerequisites

### 1.1 What This Runbook Achieves

| Step | Outcome |
| --- | --- |
| Pre-flight validation | All tests pass, binaries compile, audit scanners are clean |
| Testnet configuration | Base Sepolia values recorded for contract rehearsal without changing mainnet runtime defaults |
| Contract deployment | Executor + FundDistributor deployed, multisig-owned |
| Post-deploy verification | `owner()` == multisig, `pool()` correct, worker enabled through `setWorker`, FundDistributor ownership accepted |
| Auth-test execution | All auth gates reject unauthorized callers with correct custom errors |
| Smoke test | Authorized worker calls standalone `Executor.execute(bytes)` for one Base Sepolia rehearsal |
| Block explorer verification | Source code and constructor args verified on Basescan Sepolia |

### 1.2 Machine & Toolchain Requirements

- [ ] Rust toolchain (stable, `rustc >= 1.80`), `cargo`, `cargo audit`
- [ ] Foundry toolchain (`forge`, `cast`, `slither`) — latest stable
- [ ] Python 3.11+ with `venv` and requirements from `requirements.txt`
- [ ] Node.js 20+ (for block explorer verification tooling)
- [ ] `osv-scanner` (optional; used in full validation gate)
- [ ] Base Sepolia RPC endpoint (Alchemy, Infura, or public)
- [ ] ETH on Base Sepolia for deployer (0.05–0.2 testnet ETH)
- [ ] Multisig address deployed on Base Sepolia (Safe or compatible, must be a contract)

### 1.3 Accounts & Env Vars

The operator must prepare these values before starting. Values in `<>` brackets
must be replaced.

| Variable | Description | Example |
| --- | --- | --- |
| `CHIMERA_MULTISIG` | Multisig contract on Base Sepolia | `0x...` |
| `DEPLOYER_PRIVATE_KEY` | Funded deployer EOA key required by `Deploy.s.sol` (testnet only) | Not shown |
| `DEPLOYER_ADDRESS` | Public address corresponding to the testnet deployer | `0x...` |
| `WORKER_ADDRESS` | Testnet worker address the multisig will explicitly authorize | `0x...` |
| `WORKER_KEYSTORE` | Path to a local encrypted testnet keystore corresponding to `WORKER_ADDRESS` | `/protected/path/worker.json` |
| `CHIMERA_WORKER` | Optional single worker address for a post-deploy action notice only | `0x...` |
| `CHIMERA_AAVE_POOL` | Base Sepolia Aave V3 Pool address | See §3.1 |
| `BASE_SEPOLIA_RPC` | RPC endpoint for Base Sepolia | `https://sepolia.base.org` |

`DEPLOYER_PRIVATE_KEY` is a testnet-only exception required because
`Deploy.s.sol` reads it directly with `vm.envUint`. Supply it only through a
protected, ephemeral process environment immediately before deployment. Never
commit it, save it in a project `.env`, pass it as a command-line argument,
echo it, enable shell tracing around it, or allow it into logs. Unset it as soon
as the deployment command finishes. This is not a mainnet deployment or custody
pattern.

---

## 2. Pre-Flight Validation

All checks in this section must pass before any contract deployment. Run from
the repository root.

### 2.1 Checkout & Environment

- [ ] Clone or ensure working directory is clean: `git status` shows no
  unexpected modifications
- [ ] Checkout the target commit/branch: `git checkout <target-branch>`
- [ ] `python -m venv .venv && source .venv/bin/activate` (or `.venv\Scripts\activate` on Windows)
- [ ] `pip install -r requirements.txt`
- [ ] Verify Foundry: `forge --version` (expect `forge 0.3.0` or newer)
- [ ] Verify Rust: `cargo --version` (expect `cargo 1.80.0` or newer)

### 2.2 Validation Gate (AGENTS.md)

Run each command. Every box must be checked.

- [ ] `cargo test -p chimera-core`
  - All tests green, including `test_load_valid_config`,
    `test_rejects_unsafe_daily_cap`, `test_env_override`,
    `test_validate_mode_transition_requires_shadow_period`,
    `test_strategy_params_288_byte_abi_layout`.
- [ ] `forge test --root contracts/ -vvv`
   - All current `Executor.t.sol` tests green. Confirm these exact names:
     - `testAuthorizedWorkerExecutesFullFlashLoanFlow`
     - `testOwnerCanExecute`
     - `testUnauthorizedWorkerCannotExecute`
     - `testLegacyDirectExecSelectorIsRemoved`
     - `testExecuteRejectsWrongPayloadLengths`
     - `testExecuteRejectsZeroAssetAndAmount`
     - `testExecuteRejectsUnconfiguredPool`
     - `testFlashLoanFailureRevertsAtomically`
     - `testProfitGateStillRevertsFullFlow`
     - `testExecuteOperationRejectsWrongCaller`
     - `testExecuteOperationRejectsWrongInitiator`
     - `testExecuteOperationRejectsWrongPayloadLengths`
     - `testSetWorkerOnlyOwnerAndViewReflectsChanges`
     - `testSetPoolOnlyOwner`
     - `testConstructionOwnerIsConfigured`
     - `testWithdrawOnlyOwner`
     - `testWithdrawNativeByOwner`
     - `testTransferOwnershipOnlyOwner`
     - `testConstructorRejectsMissingOwnerArgument`
     - `testRequestAndCallbackShapesAreExact`
     - `testSelectorsMatchCanonicalSignatures`
   - `testLegacyDirectExecSelectorIsRemoved` is a removal assertion. It does not
     document or enable a legacy execution path.
   - All `FundDistributor.t.sol` tests green.
- [ ] Opt-in Base mainnet fork tests are understood before enabling them:
  - `testBaseForkStandaloneExecutorConfiguration` skips when `BASE_FORK_URL` is
    unset. Set it only in the local environment; never put an RPC URL in docs,
    source, or committed configuration.
  - `testBaseForkRealLiquidationWhenConfigured` also requires an explicit,
    current opportunity through `BASE_LIQUIDATION_USER`,
    `BASE_COLLATERAL_TOKEN`, `BASE_DEBT_TOKEN`, `BASE_DEBT_AMOUNT`,
    `BASE_DEX_ROUTER`, and `BASE_AMOUNT_OUT_MIN`; `BASE_MIN_PROFIT` is optional.
    These values are environment-only, can become stale, and the test never
    broadcasts.
- [ ] `python -m compileall scripts ai-audit/scripts`
  - No errors. All `.py` files compile without importing web3.py.
- [ ] `slither contracts --config-file slither.config.json`
  - No HIGH findings. MEDIUM/LOW findings must be triaged and documented.
  - Confirm `filter_paths` excludes `Executor.yul` (Yul source, not analyzable by Slither).
- [ ] (Optional, full gate) `cargo audit` — advisory `RUSTSEC-2024-0437`
  risk-accepted per `docs/deployment-checklist.md` §1.
- [ ] (Optional, full gate) `osv-scanner -r .` — clean or findings documented.

### 2.3 Invariant Verification (AGENTS.md §Invariants)

- [ ] Invariant #1: `config/pacing.yaml` values match `core/src/config.rs`
  `Default` impl and `valid_yaml()` test fixture.
  - Confirm: `max_daily_net_usd=2000`, `max_weekly_net_usd=7500`,
    `max_single_transfer_usd=1000`, `chain_id=8453` (mainnet default, will
    change in §3), `execute_mode=shadow`, `min_profit_multiplier=2.5`,
    `max_daily_loss_eth=0.005`, `metrics_port=9100`.
- [ ] Invariant #2: `docs/snapshot-schema.md` reserve fields match
  `core/src/simulator/prewarm.rs` `ReserveData` struct. Diff the two; any
  divergence must be resolved before proceeding.
- [ ] Invariant #3: All monetary values in Rust use `Decimal` (confirmed by
  `config.rs` field types).
- [ ] Invariant #4: `foundry.toml` `evm_version = "cancun"`. Both `[profile.default]`
  and `[profile.ci]` confirm this.
- [ ] Invariant #5: `python scripts/dry_run.py --help` runs without web3.py
  installed. Test: `python -c "import web3" 2>/dev/null || echo 'web3 absent (expected)'`.
- [ ] Invariant #6: `forge test --root contracts/ --match-path test/Executor.t.sol`
  passes (contract tests exist for all executor logic).

### 2.4 Binary Build

- [ ] `cargo build --release -p chimera-core`
  - Confirm `target/release/chimera` (or `chimera.exe`) exists.

> **Base Sepolia limitation:** `core/src/main.rs` explicitly rejects chain ID
> `84532`. Building the binary is part of repository validation, but this
> runbook does not boot `chimera` against Base Sepolia. The deployed-contract
> rehearsal and Foundry harness are distinct from the mainnet-only runtime.

---

## 3. Testnet Rehearsal Configuration

All values in this section are testnet-only inputs for deployment, `cast`, and
Foundry rehearsal. Do not replace the runtime's Base mainnet defaults with chain
ID `84532`, and do not set `execute_mode: live`. The normal runtime remains in
its conservative `shadow` default; it is not used to boot Base Sepolia.

### 3.1 Record Base Sepolia Aave Addresses

Base Sepolia Aave V3 addresses differ from mainnet. Record the following as
local environment or harness inputs; do not replace the `[base]` mainnet
section in `config/pools.toml`. Verify addresses against
[Aave Addresses docs](https://docs.aave.com/developers/deployed-contracts/v3-testnet-addresses)
before executing):

```toml
# Local Base Sepolia rehearsal values; do not commit over mainnet config.
# Operator: verify each address against Aave docs before proceeding.
# Source: https://docs.aave.com/developers/deployed-contracts/v3-testnet-addresses

[base]
# Aave V3 Pool Proxy — Base Sepolia
pool = "0x07eA79F68B2B3df56440f754B6aA0E498b9B75Fb"

# Aave V3 PoolDataProvider — Base Sepolia
pool_data_provider = "0x86Fc51D0f8A8cA1e27Ee2b9b47368d0A48EB7b9C"

# Aave V3 Oracle — Base Sepolia
oracle = "0x4B679FE601b16583Fc6FF04f2F008dC1eFfa3B1E"

# Wrapped ETH (WETH) — Base Sepolia canonical WETH
weth = "0x4200000000000000000000000000000000000006"

# USD Coin (USDC) — Base Sepolia USDC
usdc = "0x036CbD53842c5426634e7929541eC2318f3dCF7e"
```

- [ ] Confirm each address has code on Base Sepolia:
  ```bash
  ADDRS=("0x07eA79F68B2B3df56440f754B6aA0E498b9B75Fb" \
         "0x86Fc51D0f8A8cA1e27Ee2b9b47368d0A48EB7b9C" \
         "0x4B679FE601b16583Fc6FF04f2F008dC1eFfa3B1E" \
         "0x4200000000000000000000000000000000000006" \
         "0x036CbD53842c5426634e7929541eC2318f3dCF7e")
  for ADDR in "${ADDRS[@]}"; do
    echo -n "$ADDR: "
    cast code "$ADDR" --rpc-url "$BASE_SEPOLIA_RPC" | wc -c
  done
  ```
  Every address must report code length > 2 (non-empty bytecode).

### 3.2 Record a Testnet V2 Route

The existing `routing.yaml` uses mainnet DEX addresses. Do not overwrite it for
this rehearsal. Record one verified Base Sepolia V2-compatible router and pair
as local harness inputs. If no compatible testnet venue exists, skip the
end-to-end liquidation and use the fallback validation in §7.5.

For smoke-test purposes, Aerodrome V2 on Base Sepolia uses a different router.
Verify the correct testnet address before editing.

**Illustrative local harness record** (one V2 venue for the smoke test):

```yaml
venues:
  - name: "aerodrome-base-sepolia"
    chain: "base"
    liquidity_usd_min: 50000
    type: "dex"
    kyc: false
    router_compatibility: "v2"
    router_address: "<AERODROME_TESTNET_ROUTER>"
    pairs:
      - token_in: "0x4200000000000000000000000000000000000006"
        token_out: "0x036CbD53842c5426634e7929541eC2318f3dCF7e"
```

- [ ] Confirm the selected router has deployed code and supports the exact V2
  `swapExactTokensForTokens` shape used by the Executor.
- [ ] Do not use a V3-only router; the Executor's active swap path is V2-shaped.

### 3.3 Preserve Runtime Pacing Defaults

- [ ] Do not change `config/pacing.yaml` to chain ID `84532`; `main.rs` rejects it.
- [ ] Verify `execute_mode: shadow` remains set:
  ```bash
  grep execute_mode config/pacing.yaml
  # Expected: execute_mode: shadow
  ```
- [ ] Verify the repository's Base runtime default remains mainnet chain ID `8453`:
  ```bash
  grep chain_id config/pacing.yaml
  # Expected: chain_id: 8453
  ```
- [ ] Run `cargo test -p chimera-core test_load_valid_config` to confirm pacing
  invariants remain intact.

### 3.4 Preserve `config/risk.yaml`

No changes needed for testnet. The risk thresholds (`max_loss_eth: 0.005`,
`min_profit: 2.5`, `slippage_max_bps: 50`, etc.) are conservative defaults
suitable for testnet smoke testing. Confirm:

```bash
grep -E "max_loss_eth|min_profit:|slippage_max_bps" config/risk.yaml
```

- [ ] `config/risk.yaml` parses: `cargo test -p chimera-core
  test_disk_risk_yaml_parses` passes.

### 3.5 Re-run Pre-flight Before Deployment

- [ ] `cargo test -p chimera-core` — all green with repository defaults intact
- [ ] `forge test --root contracts/ -vvv` — standalone Executor tests green
- [ ] `execute_mode` is still `shadow`
- [ ] No attempt has been made to boot `main.rs` with chain ID `84532`

---

## 4. Contract Deployment

### 4.1 Prerequisites

- [ ] `CHIMERA_MULTISIG` env var set to a deployed contract on Base Sepolia:
  ```bash
  echo "CHIMERA_MULTISIG=$CHIMERA_MULTISIG"
  cast code "$CHIMERA_MULTISIG" --rpc-url "$BASE_SEPOLIA_RPC" | wc -c
  # Must report code length > 2
  ```
- [ ] `DEPLOYER_PRIVATE_KEY` env var set to a funded EOA on Base Sepolia:
  ```bash
  # Presence checks produce no output and do not disclose the key.
  test -n "${DEPLOYER_PRIVATE_KEY:-}"
  test -n "${DEPLOYER_ADDRESS:-}"
  cast balance "$DEPLOYER_ADDRESS" --rpc-url "$BASE_SEPOLIA_RPC"
  # Should show at least 0.02 ETH
  ```
- [ ] (Optional) `CHIMERA_AAVE_POOL` env var set to the Base Sepolia Aave V3
  Pool. If unset, `setPool` must be called separately by the multisig. Setting
  it at deployment time does NOT auto-call `setPool` — the deploy script only
  prints an ACTION REQUIRED notice. See Deploy.s.sol:112-124.
  ```bash
  export CHIMERA_AAVE_POOL="0x07eA79F68B2B3df56440f754B6aA0E498b9B75Fb"
  ```
- [ ] (Optional) `CHIMERA_WORKER` set to one test worker only if its post-deploy
  action notice is useful. It does not authorize that worker, and it does not
  represent the complete worker set.
- [ ] `forge build` has been run; `out/Executor.yul/Executor.json` exists:
  ```bash
  ls -la out/Executor.yul/Executor.json
  ```

### 4.2 Execute Deployment

All verified testnet addresses are set, and the testnet-only deployer key is in
the protected ephemeral environment described in §1.3. Disable shell tracing,
run the script, and then unset the key:

```bash
set +x
trap 'unset DEPLOYER_PRIVATE_KEY' EXIT
forge script contracts/script/Deploy.s.sol \
    --rpc-url "$BASE_SEPOLIA_RPC" \
    --broadcast \
    --gas-estimate-multiplier 120 \
    -vvvv
unset DEPLOYER_PRIVATE_KEY
trap - EXIT
```

`Deploy.s.sol` reads `DEPLOYER_PRIVATE_KEY` from the environment; do not add a
`--private-key` argument. On a successful run, the following lines always print:

```
Executor deployed: 0x<executor-address>
FundDistributor deployed: 0x<fund-distributor-address>
Owner (multisig): 0x<multisig-address>
MODEL: worker EOAs send normal execute(bytes) transactions to Executor.
ACTION REQUIRED: multisig must call FundDistributor.acceptOwnership()
```

The remaining notice depends on each optional variable:

- If `CHIMERA_AAVE_POOL` is set, one `Executor.setPool` action notice and its selector print; otherwise a note says the pool can be set later.
- If `CHIMERA_WORKER` is set, one `Executor.setWorker(worker, true)` action notice and its selector print for that single address; otherwise a note says workers can be authorized later.

`CHIMERA_WORKER` is optional and does not authorize the worker. The deploy script
can print at most one worker action notice. Every worker intended for the
rehearsal must still be authorized separately by the multisig with
`setWorker(worker, true)` and verified through `isWorker(address)`.

### 4.3 Record Deployment Addresses

- [ ] Executor address: `___________`
- [ ] FundDistributor address: `___________`
- [ ] Tx hash (Executor deploy): `___________`
- [ ] Tx hash (FundDistributor deploy): `___________`

Save these to a local scratch file (never committed):
```bash
echo "EXECUTOR_ADDRESS=0x..." >> .env
echo "FUND_DISTRIBUTOR_ADDRESS=0x..." >> .env
```

### 4.4 Why the Deploy Script Does NOT Call `setPool`

Per `contracts/script/Deploy.s.sol:112-124`: `setPool` is owner-gated on
Executor.yul (slot 0 check). The owner is the multisig, which was set at
construction via appended 32-byte arg. The deployer EOA is NOT the owner, so
calling `setPool` or `setWorker` from the deploy script would revert. The
multisig must configure both separately after deployment (see §5.3 and §5.4).

---

## 5. Post-Deploy Verification

### 5.1 Confirm Executor `owner() == multisig`

```bash
# Read owner() from Executor (selector 0x8da5cb5b)
cast call "$EXECUTOR_ADDRESS" "owner()" --rpc-url "$BASE_SEPOLIA_RPC"
```

- [ ] Output matches `$CHIMERA_MULTISIG` exactly (case-insensitive comparison
  against the checksummed address).

```bash
# Confirm multisig is a contract with code (belt-and-suspenders)
cast code "$CHIMERA_MULTISIG" --rpc-url "$BASE_SEPOLIA_RPC" | wc -c
# Expected: > 2
```

### 5.2 Confirm Executor `pool()` Is Unset

At this point, `setPool` has NOT been called, so the public `pool()` getter
(selector `0x16f0115b`) should return the zero address:

```bash
cast call "$EXECUTOR_ADDRESS" "pool()" --rpc-url "$BASE_SEPOLIA_RPC"
# Expected: 0x0000000000000000000000000000000000000000
```

### 5.3 Set Pool (Multisig Action)

The operator (or multisig signer) must execute the following transaction **from
the multisig**:

```bash
# Encode setPool(address) call — selector 0x4437152a
POOL_CALLDATA=$(cast calldata "setPool(address)" "$CHIMERA_AAVE_POOL")

# Initiate multisig transaction (Safe example — adapt to your multisig):
# Go to Safe web UI → New Transaction → Contract Interaction
#   Target: $EXECUTOR_ADDRESS
#   Value: 0
#   Contract method selector: setPool(address)
#   Parameter: $CHIMERA_AAVE_POOL
# OR use cast + Safe CLI/SDK for the multisig tx.
```

- [ ] Multisig executes `setPool($CHIMERA_AAVE_POOL)`.
- [ ] Verify `pool()` now returns the exact pool address:
  ```bash
  cast call "$EXECUTOR_ADDRESS" "pool()" --rpc-url "$BASE_SEPOLIA_RPC"
  # Expected: $CHIMERA_AAVE_POOL
  ```

### 5.4 Authorize the Test Worker (Multisig Action)

The worker is only a transaction signer. It must be explicitly enabled by the
Executor owner and cannot configure or withdraw from the Executor.

```bash
# setWorker(address,bool) selector: 0xc373d7f3
WORKER_CALLDATA=$(cast calldata "setWorker(address,bool)" "$WORKER_ADDRESS" true)

# In the multisig UI, submit:
#   Target: $EXECUTOR_ADDRESS
#   Value: 0
#   Method: setWorker(address,bool)
#   Parameters: $WORKER_ADDRESS, true
```

- [ ] Multisig executes `setWorker($WORKER_ADDRESS,true)`.
- [ ] Verify through `isWorker(address)` (selector `0xaa156645`):
  ```bash
  cast call "$EXECUTOR_ADDRESS" "isWorker(address)" "$WORKER_ADDRESS" \
      --rpc-url "$BASE_SEPOLIA_RPC"
  # Expected: true
  ```

### 5.5 FundDistributor Ownership Acceptance (Multisig Action)

The deploy script called `transferOwnership(multisig)` which initiated the
two-step transfer. The multisig must now accept:

```bash
# Verify pending owner is the multisig
cast call "$FUND_DISTRIBUTOR_ADDRESS" "pendingOwner()" --rpc-url "$BASE_SEPOLIA_RPC"
# Expected: $CHIMERA_MULTISIG

# Verify current owner is still the deployer
cast call "$FUND_DISTRIBUTOR_ADDRESS" "owner()" --rpc-url "$BASE_SEPOLIA_RPC"
# Expected: deployer address (not multisig yet)
```

- [ ] Multisig executes `acceptOwnership()` on FundDistributor:
  - Target: `$FUND_DISTRIBUTOR_ADDRESS`
  - Method: `acceptOwnership()`
  - Value: 0
- [ ] Verify FundDistributor owner is now multisig:
  ```bash
  cast call "$FUND_DISTRIBUTOR_ADDRESS" "owner()" --rpc-url "$BASE_SEPOLIA_RPC"
  # Expected: $CHIMERA_MULTISIG
  ```
- [ ] Verify `pendingOwner()` returns `0x0000...0000`:
  ```bash
  cast call "$FUND_DISTRIBUTOR_ADDRESS" "pendingOwner()" --rpc-url "$BASE_SEPOLIA_RPC"
  # Expected: 0x0000000000000000000000000000000000000000
  ```

---

## 6. Auth-Test Execution

Verify every authorization gate in Executor.yul. All tests use `cast call`
(read-only sim) to avoid wasting gas; reverted calls confirm the auth gate
is active.

### 6.1 Unauthorized `execute(bytes)` Reverts

```bash
# execute(bytes) selector: 0x09c5eabe
# Call from an address that is neither owner nor an enabled worker.
ALICE_ADDR="0x0000000000000000000000000000000000000001"
EMPTY_REQUEST=$(cast abi-encode "f(bytes)" "0x")
cast call "$EXECUTOR_ADDRESS" --data "0x09c5eabe${EMPTY_REQUEST#0x}" \
    --from "$ALICE_ADDR" --rpc-url "$BASE_SEPOLIA_RPC"
```

- [ ] Reverts with `Unauthorized()` — selector `0x82b42900`.
  Confirm the revert data first 4 bytes match:
  ```bash
  cast call "$EXECUTOR_ADDRESS" --data "0x09c5eabe${EMPTY_REQUEST#0x}" \
      --from "$ALICE_ADDR" --rpc-url "$BASE_SEPOLIA_RPC" 2>&1 | grep -i "82b42900"
  ```

### 6.2 Non-Owner `withdraw(address,uint256)` Reverts

```bash
# withdraw(address,uint256) selector: 0xf3fef3a3
# Try to withdraw 1 wei of ETH (token=0) from non-owner
cast call "$EXECUTOR_ADDRESS" "withdraw(address,uint256)" \
    "0x0000000000000000000000000000000000000000" "1" \
    --from "$ALICE_ADDR" --rpc-url "$BASE_SEPOLIA_RPC"
```

- [ ] Reverts with `Unauthorized()` (`0x82b42900`).

### 6.3 Non-Pool `executeOperation` Reverts

```bash
# executeOperation(asset,amount,premium,initiator,params) selector: 0x1b11d0ff
# Call from a non-pool address with valid-sized params (288 bytes)
# Even with correctly formatted params, it must revert because caller != pool
ENCODED_PARAMS="00000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000120000000000000000000000000eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee000000000000000000000000000000000000000000000000000000000000006400000000000000000000000000000000000000000000000000000000000000320000000000000000000000000000000000000000000000000000000000000000000000000000000000000000eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee000000000000000000000000dead00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000ffffffff"
cast call "$EXECUTOR_ADDRESS" "executeOperation(address,uint256,uint256,address,bytes)" \
    "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee" "100" "50" \
    "$EXECUTOR_ADDRESS" "$ENCODED_PARAMS" \
    --from "$ALICE_ADDR" --rpc-url "$BASE_SEPOLIA_RPC"
```

- [ ] Reverts with `InvalidPool()` — selector `0x2083cd40`. Even if the Aave
  pool calls it, the initiator must be `address(this)` (the Executor itself).
  A direct external call will fail either `InvalidPool()` or `Unauthorized()`.

### 6.4 Non-Owner `setPool(address)` Reverts

```bash
cast call "$EXECUTOR_ADDRESS" "setPool(address)" "$CHIMERA_AAVE_POOL" \
    --from "$ALICE_ADDR" --rpc-url "$BASE_SEPOLIA_RPC"
```

- [ ] Reverts with `Unauthorized()` (`0x82b42900`).

### 6.5 Owner `withdraw` Works (Multisig Only — Test via Staticcall)

```bash
# Simulate owner withdrawing 0 ETH (full balance sweep)
cast call "$EXECUTOR_ADDRESS" "withdraw(address,uint256)" \
    "0x0000000000000000000000000000000000000000" "0" \
    --from "$CHIMERA_MULTISIG" --rpc-url "$BASE_SEPOLIA_RPC"
```

- [ ] Does NOT revert. (Staticcall returns nothing on success for `stop()` —
  a successful empty response confirms the auth gate passes.)

### 6.6 Non-Owner `setWorker(address,bool)` Reverts

```bash
cast call "$EXECUTOR_ADDRESS" "setWorker(address,bool)" "$ALICE_ADDR" "true" \
    --from "$ALICE_ADDR" --rpc-url "$BASE_SEPOLIA_RPC"
```

- [ ] Reverts with `Unauthorized()` (`0x82b42900`).
- [ ] `isWorker($ALICE_ADDR)` remains false.

### 6.7 All Custom Error Selectors Confirmed

- [ ] `ProfitGateFailed()` — `0x9b89663c`
- [ ] `AtomicFail()` — `0xc4cae92f`
- [ ] `Unauthorized()` — `0x82b42900`
- [ ] `InvalidDexRouter()` — `0xd7c4b506`
- [ ] `InvalidPool()` — `0x2083cd40`
- [ ] `WithdrawFailed()` — `0x750b219c`

These selectors are the active constants in `Executor.yul`; the contract tests
exercise the corresponding paths. Confirm source and tests still agree before
deployment.

---

## 7. Smoke Test — End-to-End Liquidation

This test uses Base Sepolia Aave V3 to execute one flash-loan liquidation
through the deployed Executor contract.

### 7.1 Provision Aave Positions on Base Sepolia

Before the smoke test, a test position must be created on Base Sepolia Aave V3
that is liquidatable. Use the Aave testnet faucet or manual execution:

1. **Supply collateral (WETH)** to an EOA-controlled wallet on Base Sepolia Aave.
2. **Borrow debt (USDC)** against the collateral.
3. **Trigger a price movement** or adjust parameters to make the position
   liquidatable (health factor < 1.0).

This is a manual setup step. The operator should use the Aave V3 UI
(`https://app.aave.com`) connected to Base Sepolia, or use `cast send` to
interact with the Pool directly.

Record:
- [ ] Liquidatable user address: `___________`
- [ ] Collateral asset address (WETH): `0x4200000000000000000000000000000000000006`
- [ ] Debt asset address (USDC): `0x036CbD53842c5426634e7929541eC2318f3dCF7e`

### 7.2 Build StrategyParams for the Liquidation

The worker sends `execute(bytes)` an exactly 352-byte (11×32) request:
`asset`, `amount`, `collateralAsset`, `userToLiquidate`, `debtToCover`,
`receiveAToken`, `dexRouter`, `amountOutMin`, `minProfit`, `tip`, `deadline`.
The Executor passes the final nine words (288 bytes) to the callback.

The Executor's `executeOperation` can only be called by the Aave Pool as a
flash-loan callback. To trigger this atomically:

1. **Authorized worker calls `Executor.execute(bytes)`** with the exact
   352-byte request.
2. **Executor calls Aave `flashLoanSimple`** with `receiverAddress=Executor`.
3. **Aave Pool calls `Executor.executeOperation`** with `caller=Pool`,
   `initiator=Executor`, and the final nine request words as callback params.
4. **Executor** liquidates, swaps, approves repayment, emits `Profit`, and
   retains the debt-token profit.

### 7.3 Execute the Smoke Test Transaction

Using `cast`, construct and send `Executor.execute(bytes)` from the explicitly
authorized worker through a local encrypted testnet keystore. The Executor, not
the worker, is Aave's flash-loan initiator and receiver:

```bash
# Build the 352-byte request with cast abi-encode or a local harness.
# Do not place keys, RPC URLs, or opportunity values in committed files.
EXECUTOR_REQUEST_352_BYTES="0x<exact-11-word-request>"
test -r "$WORKER_KEYSTORE"

cast send "$EXECUTOR_ADDRESS" \
    "execute(bytes)" \
    "$EXECUTOR_REQUEST_352_BYTES" \
    --keystore "$WORKER_KEYSTORE" \
    --rpc-url "$BASE_SEPOLIA_RPC"
```

Enter the keystore password only at `cast`'s hidden interactive prompt; do not
put it in the command or logs. If the test worker is held on a supported
hardware wallet, replace `--keystore "$WORKER_KEYSTORE"` with
`--ledger --from "$WORKER_ADDRESS"` (or the corresponding reviewed
hardware-wallet option).
This manual `cast send` flow is testnet-only and is not the mainnet worker
custody pattern; mainnet operation uses the protected deployment-local encrypted
keystores loaded by Chimera's signer registry.

> **Do not call `Pool.flashLoanSimple` from the worker.** Such a call makes the
> worker the Aave initiator, but `executeOperation` requires
> `initiator=Executor`; it is not the supported standalone flow. The main Rust
> binary also cannot be used for this Base Sepolia rehearsal because it rejects
> chain ID `84532`; use `cast` or the Foundry harness.

### 7.4 Verify Smoke Test Success

- [ ] Transaction succeeds (no revert).
- [ ] `Profit(uint256)` event emitted from the Executor address.
  ```bash
  cast logs --address "$EXECUTOR_ADDRESS" --rpc-url "$BASE_SEPOLIA_RPC" | jq '.[] | select(.topics[0] == "0x357d905f1831209797df4d55d79c5c5bf1d9f7311c976afd05e13d881eab9bc8")'
  ```
  Topic0 `0x357d905f...` = `keccak256("Profit(uint256)")` per Executor.yul:48.
- [ ] Executor balance of the debt token increased post-transaction (profit
  retained).
- [ ] Flash loan fully repaid — no leftover debt on Aave.

### 7.5 Smoke Test Fallback

If the full flash-loan smoke test is not feasible (e.g., no liquidatable
position exists on Base Sepolia), execute this minimal validation instead:

- [ ] `setPool` set correctly (confirmed in §5.3).
- [ ] `setWorker` set and `isWorker()` true (confirmed in §5.4).
- [ ] `owner()` == multisig (confirmed in §5.1).
- [ ] All auth gates reject unauthorized callers (confirmed in §6).
- [ ] Deploy script assertions passed (confirmed in §4.2 output).
- [ ] Run `forge test --root contracts/ --match-path test/Executor.t.sol -vvv`.
- [ ] Optionally run the Base mainnet fork configuration test with a locally set
  `BASE_FORK_URL`; it skips when the variable is absent and never broadcasts.

This fallback validates the standalone contract and harness. It does not claim
that `main.rs` boots on Base Sepolia.

---

## 8. Block Explorer Verification (Basescan Sepolia)

### 8.1 Verify Executor Contract

1. Open `https://sepolia.basescan.org/address/<executor-address>#code`.
2. Click **Verify & Publish**.
3. **Contract type:** Yul is not natively supported by Basescan's standard
   verification. EITHER:
   - A) Use **Foundry's `forge verify-contract`** with a Solidity wrapper
     that forwards to the deployed Yul bytecode (if a wrapper exists), OR
   - B) Manually submit the deployed bytecode via the "Bytecode" verification
     tab, providing the Yul source as the contract source.
4. **Constructor arguments:** Append the 32-byte multisig address
   (left-padded to 32 bytes). The Executor constructor at `Executor.yul:6-17`
   reads the trailing 32 bytes of init code as the owner arg.
5. [ ] Verifier confirms match. Basescan shows "Contract Source Code Verified".

### 8.2 Verify FundDistributor Contract

1. Open `https://sepolia.basescan.org/address/<fund-distributor-address>#code`.
2. **Verify via Foundry:**
   ```bash
   forge verify-contract \
       "$FUND_DISTRIBUTOR_ADDRESS" \
       contracts/src/FundDistributor.sol:FundDistributor \
       --constructor-args $(cast abi-encode "constructor(address)" "$DEPLOYER_ADDRESS") \
       --verifier-url "https://api-sepolia.basescan.org/api" \
       --etherscan-api-key "$BASESCAN_API_KEY" \
       --chain 84532
   ```
3. [ ] Verifier confirms match.

### 8.3 Record Block Explorer Links

- [ ] Executor: `https://sepolia.basescan.org/address/<executor-address>`
- [ ] FundDistributor: `https://sepolia.basescan.org/address/<fund-distributor-address>`

---

## 9. Final Checklist State

Before considering the testnet deployment complete, all boxes below must be
checked:

### Pre-Flight
- [ ] `cargo test -p chimera-core` — green
- [ ] `forge test --root contracts/ -vvv` — green
- [ ] `python -m compileall scripts ai-audit/scripts` — clean
- [ ] `slither contracts --config-file slither.config.json` — clean/triaged

### Configuration
- [ ] Base Sepolia Aave V3 addresses verified as local rehearsal inputs
- [ ] V2-compatible testnet route verified as a local rehearsal input
- [ ] `pacing.yaml` mainnet chain_id remains 8453 and execute_mode remains shadow
- [ ] `risk.yaml` unchanged, parses
- [ ] `cargo test -p chimera-core` with repository defaults — green

### Deployment
- [ ] Executor deployed, deployed address recorded
- [ ] FundDistributor deployed, deployed address recorded
- [ ] `owner()` == multisig (Executor)
- [ ] Multisig code.length > 0
- [ ] `setPool(aaveV3Pool)` executed by multisig, `pool()` confirmed
- [ ] `setWorker(worker,true)` executed by multisig, `isWorker(worker)` confirmed

### Auth Tests
- [ ] Unauthorized `execute(bytes)` → `Unauthorized()` (0x82b42900)
- [ ] Non-owner `withdraw()` → `Unauthorized()` (0x82b42900)
- [ ] Non-pool `executeOperation` → `InvalidPool()` (0x2083cd40)
- [ ] Non-owner `setPool()` → `Unauthorized()` (0x82b42900)
- [ ] Non-owner `setWorker()` → `Unauthorized()` (0x82b42900)
- [ ] Owner `withdraw()` staticcall passes

### Smoke Test
- [ ] End-to-end `Executor.execute(bytes)` liquidation OR contract/harness fallback
- [ ] `Profit` event and retained Executor profit confirmed when liquidation runs

### Block Explorer
- [ ] Executor source verified on Basescan Sepolia
- [ ] FundDistributor source verified on Basescan Sepolia
- [ ] Constructor args verified (multisig for Executor, deployer for FundDistributor)

---

## A. Reference: Key Addresses (Base Sepolia)

| Contract | Address |
| --- | --- |
| Aave V3 Pool | `0x07eA79F68B2B3df56440f754B6aA0E498b9B75Fb` |
| Aave V3 PoolDataProvider | `0x86Fc51D0f8A8cA1e27Ee2b9b47368d0A48EB7b9C` |
| Aave V3 Oracle | `0x4B679FE601b16583Fc6FF04f2F008dC1eFfa3B1E` |
| WETH | `0x4200000000000000000000000000000000000006` |
| USDC | `0x036CbD53842c5426634e7929541eC2318f3dCF7e` |
| Aerodrome V2 Router | _Verify against testnet deployment_ |
| Chainlink ETH/USD Feed | _Verify against testnet deployment_ |

> **Operator note:** Aave testnet contracts are occasionally redeployed. Always
> verify the addresses above against the official Aave documentation before
> executing this runbook:
> https://docs.aave.com/developers/deployed-contracts/v3-testnet-addresses

## B. Reference: Custom Error Selectors (Executor.yul)

| Error | Selector | Defined at |
| --- | --- | --- |
| `ProfitGateFailed()` | `0x9b89663c` | Active Yul constant |
| `AtomicFail()` | `0xc4cae92f` | Active Yul constant |
| `Unauthorized()` | `0x82b42900` | Active Yul constant |
| `InvalidDexRouter()` | `0xd7c4b506` | Active Yul constant |
| `InvalidPool()` | `0x2083cd40` | Active Yul constant |
| `WithdrawFailed()` | `0x750b219c` | Active Yul constant |

## C. Troubleshooting Quick Reference

| Symptom | Likely Cause | Check |
| --- | --- | --- |
| `forge script` fails "multisig must be a contract" | `CHIMERA_MULTISIG` is an EOA or unset | `cast code $CHIMERA_MULTISIG --rpc-url $BASE_SEPOLIA_RPC` |
| `forge script` fails "executor deploy failed" | CREATE reverted (gas, nonce, or bytecode) | Increase gas multiplier; check deployer balance |
| `cast call owner()` returns wrong address | Constructor arg not appended correctly | Re-run deploy script with correct `CHIMERA_MULTISIG` |
| `setPool` call reverts | Caller is not the multisig | Confirm `cast call` uses `--from $CHIMERA_MULTISIG` |
| `execute(bytes)` returns `Unauthorized()` | Worker was not enabled or was revoked | Confirm `isWorker($WORKER_ADDRESS)` and have multisig call `setWorker(...,true)` |
| Base Sepolia binary boot fails as unsupported | `main.rs` rejects chain ID 84532 | Use contract rehearsal/Foundry harness; do not claim runtime support |
| Slither fails on `Executor.yul` | Slither cannot analyze Yul | Already excluded in `slither.config.json` `filter_paths` |
| `cargo test` fails after config edits | Config validation rejected a value | Check pacing.yaml against invariants in `config.rs::validate()` |
| Cannot verify Yul contract on Basescan | Yul not natively supported | Use bytecode verification tab; provide source inline |
