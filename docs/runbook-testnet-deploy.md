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
| Testnet configuration | Config files updated for Base Sepolia (chain_id=84532) |
| Contract deployment | Executor + FundDistributor deployed, multisig-owned |
| Post-deploy verification | `owner()` == multisig, pool set, FundDistributor ownership accepted |
| Auth-test execution | All auth gates reject unauthorized callers with correct custom errors |
| Smoke test | One end-to-end flash-loan liquidation on Base Sepolia Aave V3 |
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
| `DEPLOYER_PRIVATE_KEY` | Funded deployer EOA (testnet only) | `0x...` |
| `CHIMERA_AAVE_POOL` | Base Sepolia Aave V3 Pool address | See §3.1 |
| `BASE_SEPOLIA_RPC` | RPC endpoint for Base Sepolia | `https://sepolia.base.org` |

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
  - All `Executor.t.sol` tests green. Confirm:
    - `test_constructor_owner` — owner set from appended arg
    - `test_exec_owner_only` — non-owner `exec()` reverts with `Unauthorized()`
    - `test_withdraw_owner_only` — non-owner `withdraw()` reverts with `Unauthorized()`
    - `test_setPool_owner_only` — non-owner `setPool()` reverts with `Unauthorized()`
    - `test_executeOperation_pool_gate` — non-pool `executeOperation` reverts with `InvalidPool()`
    - `test_executeOperation_initiator_gate` — wrong initiator reverts with `Unauthorized()`
    - `test_profit_gate_no_profit` — profit gate reverts with `ProfitGateFailed()`
    - `test_executeOperation_emits_profit_event` — `Profit(uint256)` emitted
    - `test_direct_exec_via_exec` — EIP-7702 `exec(bytes)` path works
    - `test_withdraw_erc20` + `test_withdraw_eth` — both withdraw paths work
    - `test_transferOwnership` — ownership transfers, zero-address rejected
    - `test_profit_gate_overflow_guard` — overflow guards active
  - All `FundDistributor.t.sol` tests green.
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
- [ ] `scripts/dry_run.py --binary target/release/chimera --run-secs 5`
  - Binary boots in shadow mode, no panic, terminates cleanly.

---

## 3. Testnet Configuration

### 3.1 Update `config/pools.toml` for Base Sepolia

Base Sepolia Aave V3 addresses differ from mainnet. Replace the `[base]`
section in `config/pools.toml` with the following (verify addresses against
[Aave Addresses docs](https://docs.aave.com/developers/deployed-contracts/v3-testnet-addresses)
before executing):

```toml
# config/pools.toml — Base Sepolia Aave V3 testnet addresses
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

### 3.2 Update `config/routing.yaml` for Testnet

The existing `routing.yaml` uses mainnet DEX addresses. For testnet, update
the router addresses in the `base`-chain venues. Base Sepolia DEX testnet
counterparts are needed. If no testnet DEX exists for a venue, mark it as
`excluded` in the comment or remove it temporarily.

For smoke-test purposes, Aerodrome V2 on Base Sepolia uses a different router.
Verify the correct testnet address before editing.

**Minimum testnet routing configuration** (leave only one V2 venue for the
smoke test):

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

- [ ] Verified: all `base`-chain venues in routing.yaml have
  `router_compatibility: "v2"` or are removed. V3 venues (`router_compatibility:
  "v3"`) MUST NOT be present for `base` chain — the V2-only routing engine will
  filter them, but their presence is misleading. Remove `uniswap-v3-base` from
  the venues list for testnet.
- [ ] Run `cargo test -p chimera-core` after editing routing.yaml to confirm
  `test_routing_yaml_new_fields_present` still passes (it checks for at least
  one venue with `router_address` + `pairs` populated).

### 3.3 Update `config/pacing.yaml` for Base Sepolia

- [ ] Change `chain_id: 8453` → `chain_id: 84532`:
  ```bash
  sed -i 's/chain_id: 8453/chain_id: 84532/' config/pacing.yaml
  ```
- [ ] Update `eth_usd_feed_address` to Base Sepolia Chainlink ETH/USD feed.
  Base Sepolia does not have a Chainlink feed deployed at the mainnet address
  `0x71041dddad3595F9CEd3DcCbe3D9337177BcC57b`. Use the correct Sepolia feed
  address or set a fallback. If no feed exists, the `eth_price_usd_fallback`
  value (`1800`) will be used.
- [ ] Verify `execute_mode: shadow` is still set (must NOT be `live` for
  testnet deployment):
  ```bash
  grep execute_mode config/pacing.yaml
  # Expected: execute_mode: shadow
  ```
- [ ] Verify `chain_id: 84532` took effect:
  ```bash
  grep chain_id config/pacing.yaml
  # Expected: chain_id: 84532
  ```
- [ ] Run `cargo test -p chimera-core`
  `test_load_valid_config` to confirm the edited pacing.yaml still parses.

### 3.4 Update `config/risk.yaml`

No changes needed for testnet. The risk thresholds (`max_loss_eth: 0.005`,
`min_profit: 2.5`, `slippage_max_bps: 50`, etc.) are conservative defaults
suitable for testnet smoke testing. Confirm:

```bash
grep -E "max_loss_eth|min_profit:|slippage_max_bps" config/risk.yaml
```

- [ ] `config/risk.yaml` parses: `cargo test -p chimera-core
  test_disk_risk_yaml_parses` passes.

### 3.5 Re-run Pre-flight After Config Changes

- [ ] `cargo test -p chimera-core` — all green with updated configs
- [ ] `scripts/dry_run.py` — PASS, execute_mode is shadow, pacing parses
- [ ] `scripts/health_check.py --state-dir core/state` — mode_state reports
  shadow (or absent, which is acceptable before first boot)

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
  DEPLOYER_ADDR=$(cast wallet address "$DEPLOYER_PRIVATE_KEY")
  cast balance "$DEPLOYER_ADDR" --rpc-url "$BASE_SEPOLIA_RPC"
  # Should show at least 0.02 ETH
  ```
- [ ] (Optional) `CHIMERA_AAVE_POOL` env var set to the Base Sepolia Aave V3
  Pool. If unset, `setPool` must be called separately by the multisig. Setting
  it at deployment time does NOT auto-call `setPool` — the deploy script only
  prints an ACTION REQUIRED notice. See Deploy.s.sol:112-124.
  ```bash
  export CHIMERA_AAVE_POOL="0x07eA79F68B2B3df56440f754B6aA0E498b9B75Fb"
  ```
- [ ] `forge build` has been run; `out/Executor.yul/Executor.json` exists:
  ```bash
  ls -la out/Executor.yul/Executor.json
  ```

### 4.2 Execute Deployment

All verified testnet addresses and keys are in env vars. Run:

```bash
forge script contracts/script/Deploy.s.sol \
    --rpc-url "$BASE_SEPOLIA_RPC" \
    --broadcast \
    --private-key "$DEPLOYER_PRIVATE_KEY" \
    --gas-estimate-multiplier 120 \
    -vvvv
```

**Expected output (console.log from Deploy.s.sol):**

```
Executor deployed: 0x<executor-address>
FundDistributor deployed: 0x<fund-distributor-address>
Owner (multisig): 0x<multisig-address>
ACTION REQUIRED: multisig must call Executor.setPool: 0x07eA79F68B2B3df56440f754B6aA0E498b9B75Fb
ACTION REQUIRED: multisig must call FundDistributor.acceptOwnership()
```

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
calling `setPool` from the deploy script would revert. The multisig must call
it separately after deployment (see §5.3).

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

### 5.2 Confirm Executor Slot 1 (pool) is Unset

At this point, `setPool` has NOT been called, so `sload(1)` should return
`0x0000...0000`:

```bash
cast storage "$EXECUTOR_ADDRESS" 1 --rpc-url "$BASE_SEPOLIA_RPC"
# Expected: 0x0000000000000000000000000000000000000000000000000000000000000000
```

### 5.3 Set Pool (Multisig Action)

The operator (or multisig signer) must execute the following transaction **from
the multisig**:

```bash
# Encode setPool(address) call — selector 0xa51b62c1
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
- [ ] Verify slot 1 now holds the pool address:
  ```bash
  cast storage "$EXECUTOR_ADDRESS" 1 --rpc-url "$BASE_SEPOLIA_RPC"
  # Expected: 0x00000000000000000000000007eA79F68B2B3df56440f754B6aA0E498b9B75Fb
  ```
- [ ] Verify `executeOperation` now accepts the pool as caller (auth gate
  checks `caller() == sload(1)`):
  ```bash
  # Read back via staticcall to confirm encoding
  cast call "$EXECUTOR_ADDRESS" "setPool(address)" "$CHIMERA_AAVE_POOL" \
      --from "$CHIMERA_MULTISIG" --rpc-url "$BASE_SEPOLIA_RPC"
  # No revert expected (staticcall shows result; actual tx would set storage)
  ```

### 5.4 FundDistributor Ownership Acceptance (Multisig Action)

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

### 6.1 Non-Owner `exec(bytes)` Reverts

```bash
# exec(bytes) selector: 0x55f86501
# Call from any non-owner EOA (use a throwaway address or the deployer)
ALICE_ADDR="0x0000000000000000000000000000000000000001"
cast call "$EXECUTOR_ADDRESS" "exec(bytes)" "0x" \
    --from "$ALICE_ADDR" --rpc-url "$BASE_SEPOLIA_RPC"
```

- [ ] Reverts with `Unauthorized()` — selector `0x82b42900`.
  Confirm the revert data first 4 bytes match:
  ```bash
  cast call "$EXECUTOR_ADDRESS" "exec(bytes)" "0x" \
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

- [ ] Reverts with `InvalidPool()` — selector `0xd0363b78`. Even if the Aave
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

### 6.6 All Custom Error Selectors Confirmed

- [ ] `ProfitGateFailed()` — `0x2e5a0d02`
- [ ] `AtomicFail()` — `0x5fe2e75c`
- [ ] `Unauthorized()` — `0x82b42900`
- [ ] `InvalidDexRouter()` — `0x8d4f59a9`
- [ ] `InvalidPool()` — `0xd0363b78`
- [ ] `WithdrawFailed()` — `0xf1620b3e`

These selectors are defined in `Executor.yul:50-56` and tested in
`Executor.t.sol:68-73`. Confirm the test file matches the Yul source.

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

The Executor expects `StrategyParams` as a tightly packed 288-byte (9×32) blob
in the `params` field of the `executeOperation` callback. Use the encoded
format documented at `Executor.yul:99-107` and `core/src/config.rs:558-594`.

The Executor's `executeOperation` can only be called by the Aave Pool as a
flash-loan callback. To trigger this atomically:

1. **Flash-loan the debt amount from Aave** — call `flashLoanSimple` on the
   Aave Pool with the Executor as the receiver.
2. **The Aave Pool calls `executeOperation`** on the Executor with the
   StrategyParams encoded in the `params` bytes.
3. **The Executor** liquidates, swaps, repays the flash loan, and emits
   `Profit`.

### 7.3 Execute the Smoke Test Transaction

Using `cast`, construct and send the flash-loan initiation transaction from a
worker EOA (the "initiator" in Worker-as-Executor model):

```bash
# Address of the Worker EOA that will act as the initiator
WORKER_EOA="0x<worker-eoa-address>"

# Build StrategyParams (288 bytes, 9×32 words)
# Format: collateralAsset, userToLiquidate, debtToCover, receiveAToken,
#         dexRouter, amountOutMin, minProfit, tip, deadline

# Example encoding — operator must fill in actual values:
# (This is illustrative — use the Rust StrategyParams::encode() in production,
#  or construct manually with abi.encodePacked-like assembly)

# For the smoke test, the simplest approach is to call flashLoanSimple
# directly from cast:
AMOUNT_TO_FLASH_LOAN="<debt-token-amount-in-wei>"

# Encode the flash loan call
cast send "$CHIMERA_AAVE_POOL" \
    "flashLoanSimple(address,address,uint256,bytes,uint16)" \
    "$EXECUTOR_ADDRESS" \
    "$DEBT_ASSET_ADDRESS" \
    "$AMOUNT_TO_FLASH_LOAN" \
    "$STRATEGY_PARAMS_288_BYTES" \
    "0" \
    --private-key "$WORKER_PRIVATE_KEY" \
    --rpc-url "$BASE_SEPOLIA_RPC"
```

> **Note:** The exact `cast` command depends on the constructed 288-byte
> StrategyParams. In practice, the Rust orchestrator binary handles this
> encoding via `StrategyParams::encode()`. For a manual smoke test, the
> operator can:
> - Use a Rust helper script that calls `encode()` and prints hex.
> - Use `cast abi-encode` with careful manual construction.
> - Deploy a Solidity helper contract that constructs and forwards the call.

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
- [ ] `owner()` == multisig (confirmed in §5.1).
- [ ] All auth gates reject unauthorized callers (confirmed in §6).
- [ ] Deploy script assertions passed (confirmed in §4.2 output).
- [ ] Run the Rust binary in shadow mode for 60 seconds against Base Sepolia:
  ```bash
  CHIMERA_EXECUTE_MODE=shadow \
  CHIMERA_CHAIN_ID=84532 \
  CHIMERA_EXECUTOR_ADDRESS="$EXECUTOR_ADDRESS" \
  target/release/chimera &
  CHIMERA_PID=$!
  sleep 60
  kill $CHIMERA_PID
  ```
  Check logs for `PASS`/no `panic` and that the orchestrator successfully
  detected testnet Aave reserves without errors.

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
- [ ] `pools.toml` updated with Base Sepolia Aave V3 addresses
- [ ] `routing.yaml` restricted to V2-only venues for base chain
- [ ] `pacing.yaml` chain_id=84532, execute_mode=shadow
- [ ] `risk.yaml` unchanged, parses
- [ ] Re-run `cargo test -p chimera-core` after config changes — green

### Deployment
- [ ] Executor deployed, deployed address recorded
- [ ] FundDistributor deployed, deployed address recorded
- [ ] `owner()` == multisig (Executor)
- [ ] Multisig code.length > 0
- [ ] `setPool(aaveV3Pool)` executed by multisig, slot 1 confirmed

### Auth Tests
- [ ] Non-owner `exec()` → `Unauthorized()` (0x82b42900)
- [ ] Non-owner `withdraw()` → `Unauthorized()` (0x82b42900)
- [ ] Non-pool `executeOperation` → `InvalidPool()` (0xd0363b78)
- [ ] Non-owner `setPool()` → `Unauthorized()` (0x82b42900)
- [ ] Owner `withdraw()` staticcall passes

### Smoke Test
- [ ] End-to-end liquidation on Base Sepolia OR shadow-mode boot test
- [ ] `Profit` event confirmed or no panic in shadow boot

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
| `ProfitGateFailed()` | `0x2e5a0d02` | Executor.yul:51 |
| `AtomicFail()` | `0x5fe2e75c` | Executor.yul:52 |
| `Unauthorized()` | `0x82b42900` | Executor.yul:53 |
| `InvalidDexRouter()` | `0x8d4f59a9` | Executor.yul:54 |
| `InvalidPool()` | `0xd0363b78` | Executor.yul:55 |
| `WithdrawFailed()` | `0xf1620b3e` | Executor.yul:56 |

## C. Troubleshooting Quick Reference

| Symptom | Likely Cause | Check |
| --- | --- | --- |
| `forge script` fails "multisig must be a contract" | `CHIMERA_MULTISIG` is an EOA or unset | `cast code $CHIMERA_MULTISIG --rpc-url $BASE_SEPOLIA_RPC` |
| `forge script` fails "executor deploy failed" | CREATE reverted (gas, nonce, or bytecode) | Increase gas multiplier; check deployer balance |
| `cast call owner()` returns wrong address | Constructor arg not appended correctly | Re-run deploy script with correct `CHIMERA_MULTISIG` |
| `setPool` call reverts | Caller is not the multisig | Confirm `cast call` uses `--from $CHIMERA_MULTISIG` |
| Slither fails on `Executor.yul` | Slither cannot analyze Yul | Already excluded in `slither.config.json` `filter_paths` |
| `cargo test` fails after config edits | Config validation rejected a value | Check pacing.yaml against invariants in `config.rs::validate()` |
| Cannot verify Yul contract on Basescan | Yul not natively supported | Use bytecode verification tab; provide source inline |
