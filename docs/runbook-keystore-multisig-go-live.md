# Project Chimera — Keystore, Multisig & Go-Live Runbook

**Version**: 1.0
**Last Updated**: 2026-07-04
**Audience**: Solo operator with access to treasury keys and the deployed multisig.
**Prerequisites**: Completed `docs/deployment-checklist.md` §1–4 (Pre-flight, Deployment, Operational Setup, Shadow Soak).

> **Operator-executed runbook — do not execute during authoring.**
>
> Every step below is intended to be performed by the operator on a secure,
> isolated workstation. Commands marked with `[LIVE]` mutate on-chain state or
> boot the live engine. Read every step before executing it.

---

## 1. Purpose & Prerequisites

This runbook gates the path from a completed 7-day shadow soak to live on-chain
execution. It covers:

1. Encrypted keystore creation (`treasury_keystore` + `worker_keystore_dir`)
2. Multisig ownership verification (Executor + FundDistributor)
3. Config updates for live mode
4. Worker EOA funding
5. Go-live gate execution (`toggle_shadow.py --set-live`)
6. First-operations watch
7. First sweep verification
8. Rollback procedure

**Before starting, you must have:**

- [ ] Shadow soak completed (≥ 7 continuous days), evidence archived
- [ ] Contracts deployed to target L2 (Executor + FundDistributor) per `docs/deployment-checklist.md` §2
- [ ] Multisig wallet address known and accessible
- [ ] Treasury address known and its private key available (for keystore creation)
- [ ] Worker EOA private keys available (for keystore creation)
- [ ] `docs/emergency-procedures.md` reviewed by the operator on duty
- [ ] All pre-flight checks green from `docs/deployment-checklist.md` §1

**Cross-references (read first):**
- `docs/deployment-checklist.md` — full deployment and soak checklist
- `docs/emergency-procedures.md` — breaker conditions and recovery
- `docs/operator-manual.md` — daily operational procedures
- `docs/first-run-onboarding.md` — system overview and safety rules

---

## 2. Encrypted Keystore Creation

Chimera uses encrypted keystore files for all signing wallets. The `SignerRegistry`
(`core/src/signer_registry/mod.rs`) expects:

- **Treasury keystore**: a single encrypted JSON file pointed to by
  `treasury_keystore` in `config/pacing.yaml`.
- **Worker keystores**: one encrypted JSON file per worker EOA, placed in the
  directory pointed to by `worker_keystore_dir`.
- **Password**: a single password for all keystores, loaded from the
  `CHIMERA_KEYSTORE_PASSWORD` environment variable.

### 2.1 Keystore Format

Keystores use the standard Ethereum encrypted keystore format (`scrypt` key
derivation + `aes-128-ctr` cipher). They are decrypted at startup by Alloy's
`PrivateKeySigner::decrypt_keystore(path, password)`.

| Property | Value |
|---|---|
| Key derivation | scrypt (n=2^18, r=8, p=1 default) |
| Cipher | aes-128-ctr |
| Format | Web3 Secret Storage JSON (RFC-compatible) |

### 2.2 Create Treasury Keystore

Create the treasury keystore **outside the repository** to a secure directory
that is never tracked by git. The `.gitignore` already excludes `*.pem`,
`*.key`, `wallets/`, `private_keys/`, `clean_eoa_seeds.txt`, `.env`, and
all of `core/state/`.

**Recommended: use `cast wallet` (Foundry) or the `alloy` CLI to create keystores.**

```bash
# Set a strong keystore password
export CHIMERA_KEYSTORE_PASSWORD='<generate-a-strong-random-password>'

# Create a secure directory outside the repo
mkdir -p /secure/path/chimera-keystores

# Create treasury keystore from its private key
# Replace 0xYOUR_TREASURY_PRIVATE_KEY with the actual key.
cast wallet import treasury --private-key 0xYOUR_TREASURY_PRIVATE_KEY \
    --keystore-dir /secure/path/chimera-keystores

# The resulting file will be named something like:
#   /secure/path/chimera-keystores/treasury-<timestamp>-<uuid>
# Rename it clearly:
mv /secure/path/chimera-keystores/treasury-* /secure/path/chimera-keystores/treasury.json
```

If you do not have `cast` available, use `alloy` CLI or any tool that produces
Web3 Secret Storage compatible keystore files.

```bash
# Alternative: use Python (web3.py)
python3 -c "
from eth_account import Account
acct = Account.from_key('0xYOUR_TREASURY_PRIVATE_KEY')
encrypted = Account.encrypt(acct.key, '$CHIMERA_KEYSTORE_PASSWORD')
import json
with open('/secure/path/chimera-keystores/treasury.json', 'w') as f:
    json.dump(encrypted, f)
print('Keystore written.')
"
```

### 2.3 Create Worker Keystores

Create one keystore file per worker EOA in a **separate directory** (pointed to
by `worker_keystore_dir`). The directory must contain **only** keystore files
— no subdirectories, no other file types. The `SignerRegistry` reads every file
in this directory and attempts to decrypt each one with
`CHIMERA_KEYSTORE_PASSWORD`.

```bash
# Create worker keystore directory
mkdir -p /secure/path/chimera-keystores/workers

# For each worker private key, create a keystore:
# Repeat for worker_1, worker_2, ...
cast wallet import worker_1 --private-key 0xWORKER_1_PRIVATE_KEY \
    --keystore-dir /secure/path/chimera-keystores/workers

cast wallet import worker_2 --private-key 0xWORKER_2_PRIVATE_KEY \
    --keystore-dir /secure/path/chimera-keystores/workers

# ... repeat for all workers
```

**All keystores must use the same `CHIMERA_KEYSTORE_PASSWORD`.** The
`SignerRegistry` applies the same password to every file.

### 2.4 Password Management

**Never commit `CHIMERA_KEYSTORE_PASSWORD` to the repository or any tracked file.**

The password is loaded from the environment. Set it in the host environment
where the Chimera binary runs:

```bash
# Add to the host's secure env file (e.g., /etc/chimera/env or systemd EnvironmentFile)
# NEVER add it to ~/.bashrc or any file in the repo.
export CHIMERA_KEYSTORE_PASSWORD='<the-password-you-set-above>'
```

For systemd-based deployment, use an `EnvironmentFile` with mode `0600`:

```ini
# /etc/chimera/env (chmod 600, chown root:root)
CHIMERA_KEYSTORE_PASSWORD=<the-password>
```

### 2.5 Verify Keystore Integrity

Before relying on keystores, verify that SignerRegistry can decrypt them and
the derived addresses match your expected addresses:

```bash
# Set the password in the environment
export CHIMERA_KEYSTORE_PASSWORD='<the-password>'

# Verify the config has correct paths
grep -E 'treasury_keystore|worker_keystore_dir' config/pacing.yaml

# Run a quick smoke test: the binary validates keystores at startup in live mode.
# If it starts without error, keystores are valid.
# (Do this in shadow mode first to avoid accidental transactions.)
RUST_LOG=chimera=info ./target/release/chimera 2>&1 | grep -i "signer loaded"
# Expected output for each signer:
#   INFO chimera::signer_registry: Treasury signer loaded address=0x...
#   INFO chimera::signer_registry: Worker signer loaded address=0x...
```

If you see `Skipping unreadable keystore file`, that file's decryption failed
(password mismatch or corrupt file).

In live mode, `SignerRegistry` also runs `validate_against_eoa_pool()`:
every non-excluded wallet in `config/eoa_pool.json` must have a matching
keystore. If a wallet is missing a keystore, startup fails with:

```
Live mode: EOA pool entry 0x... has no registered worker signer
```

### 2.6 Security Checklist

- [ ] Keystore files reside **outside the repository directory**
- [ ] `treasury_keystore` and `worker_keystore_dir` paths in `pacing.yaml` are absolute paths
- [ ] Keystore file permissions are restrictive (`chmod 600` on Linux)
- [ ] `CHIMERA_KEYSTORE_PASSWORD` is set only in a secure env file, never in shell history or tracked config
- [ ] `.gitignore` excludes `*.pem`, `*.key`, `.env`, `secrets.env`, `wallets/`, `private_keys/`, `core/state/` — note: only the exact filename `.env` is excluded (not `*.env`); any `.env`-suffixed file will be tracked
- [ ] No private key material exists anywhere under the repository root
- [ ] All worker keystore files decrypt successfully with the same password
- [ ] Derived addresses match `config/eoa_pool.json` entries

---

## 3. Multisig Ownership Setup

The Executor contract and FundDistributor contract both use a multisig as their
owner. Verify the ownership chain before going live.

### 3.1 Verify Executor Ownership

The Executor owner is set **at construction** by `Deploy.s.sol` via an appended
32-byte constructor argument. The deploy script enforces `multisig.code.length > 0`
to prevent handing ownership to an EOA.

```bash
# Verify Executor owner on-chain (cast from Foundry)
cast call <EXECUTOR_ADDRESS> "owner()(address)" --rpc-url <RPC_URL>

# Expected: the multisig address
```

Confirm on the block explorer:
- [ ] `owner()` returns the multisig address
- [ ] The multisig is a deployed contract (`multisig.code.length > 0`)

### 3.2 FundDistributor Two-Step Ownership Transfer

`FundDistributor` uses a two-step ownership transfer:

1. Deployer calls `transferOwnership(multisig)` — sets `pendingOwner`
2. Multisig must call `acceptOwnership()` — makes the multisig the actual `owner`

`Deploy.s.sol` initiates step 1 at deploy time. Step 2 **must be completed by
the multisig before going live**.

```bash
# Verify current state
cast call <FUND_DISTRIBUTOR_ADDRESS> "owner()(address)" --rpc-url <RPC_URL>
# Expected after Deploy.s.sol: the deployer address (not yet multisig)

cast call <FUND_DISTRIBUTOR_ADDRESS> "pendingOwner()(address)" --rpc-url <RPC_URL>
# Expected: the multisig address

# ACTION: from the multisig, call acceptOwnership()
# (The exact method depends on your multisig — Safe, Gnosis, etc.)
# Example via cast (if you have the deployer key for testing):
#   cast send <FUND_DISTRIBUTOR_ADDRESS> "acceptOwnership()" \
#       --from <MULTISIG_ADDRESS> --rpc-url <RPC_URL>

# After acceptance:
cast call <FUND_DISTRIBUTOR_ADDRESS> "owner()(address)" --rpc-url <RPC_URL>
# Expected: the multisig address
```

### 3.3 Set the Aave Pool on Executor

The Executor contract must be told which Aave V3 pool to use. This is an
owner-gated call (`setPool(address)`), so it **must** come from the multisig.

```bash
# Base mainnet Aave V3 Pool address (from config/pools.toml):
#   0xA238Dd80C259a72e81d7e4664a9801593F98d1c5
# Verify this against config/pools.toml and Aave V3 docs before executing.

# [LIVE] From the multisig, call:
# Executor.setPool(<AAVE_V3_POOL_ADDRESS>)
```

- [ ] Multisig calls `setPool(<aaveV3Pool>)` on the Executor
- [ ] Verify via cast: `cast storage <EXECUTOR_ADDRESS> 1` returns the pool address (stored in slot 1)
- [ ] Confirm the pool address matches the Aave V3 deployment on the target chain

### 3.4 Post-Deployment Ownership Checklist

- [ ] `Executor.owner() == multisig`
- [ ] `multisig.code.length > 0` (multisig is a contract, not an EOA)
- [ ] `FundDistributor.owner() == multisig`
- [ ] `FundDistributor.pendingOwner() == address(0)` (transfer complete)
- [ ] `cast storage <EXECUTOR_ADDRESS> 1` returns the Aave V3 pool address
- [ ] Both contracts verified on the block explorer

---

## 4. Config Updates for Live Mode

Edit `config/pacing.yaml` to set the live-mode paths and addresses. These
fields are empty by default and **must** be set for the binary to start in live
mode.

### 4.1 Required YAML Changes

Edit `config/pacing.yaml` (or override via `CHIMERA_*` env vars — see §4.3):

```yaml
# --- LIVE MODE: uncomment/set these fields ---

# Keystore paths (absolute paths recommended)
treasury_keystore: /secure/path/chimera-keystores/treasury.json
worker_keystore_dir: /secure/path/chimera-keystores/workers

# Deployed contract addresses
executor_address: "0x<EXECUTOR_DEPLOYED_ADDRESS>"
treasury_address: "0x<MULTISIG_TREASURY_ADDRESS>"

# EOA pool (should already point to config/eoa_pool.json with valid addresses)
eoa_pool_path: config/eoa_pool.json
```

### 4.2 Pacing Caps for First Live Window

The default caps are conservative by design. **Do not raise them for the first
live window.** Confirm these values are set in `config/pacing.yaml`:

```yaml
max_daily_net_usd: 2000
max_weekly_net_usd: 7500
max_single_transfer_usd: 1000
max_daily_loss_eth: 0.005
min_profit_multiplier: 2.5
auto_halt_on_reverts: 3
max_gas_gwei: 300
min_worker_balance_eth: 0.01
refund_topup_eth: 0.05
sweep_min_keep_eth: 0.005
```

- [ ] All caps reviewed and set to conservative defaults
- [ ] No cap raised above its default for the first live window

### 4.3 Env Var Overrides (Alternative)

Any `config/pacing.yaml` field can be overridden via a `CHIMERA_*` environment
variable. This is defined in `core/src/config.rs:166-299`. The prefix convention
is: uppercase the field name, prefix with `CHIMERA_`, replace `.` with `_`.

Key env vars for go-live:

| Env Var | Overrides | Default |
|---|---|---|
| `CHIMERA_EXECUTE_MODE` | `execute_mode` | `shadow` |
| `CHIMERA_TREASURY_KEYSTORE` | `treasury_keystore` | `""` |
| `CHIMERA_WORKER_KEYSTORE_DIR` | `worker_keystore_dir` | `""` |
| `CHIMERA_EXECUTOR_ADDRESS` | `executor_address` | `""` |
| `CHIMERA_TREASURY_ADDRESS` | `treasury_address` | `""` |
| `CHIMERA_CHAIN_ID` | `chain_id` | `8453` |
| `CHIMERA_KEYSTORE_PASSWORD` | (password) | (required) |
| `CHIMERA_MAX_DAILY_NET_USD` | `max_daily_net_usd` | `2000` |
| `CHIMERA_MAX_WEEKLY_NET_USD` | `max_weekly_net_usd` | `7500` |
| `CHIMERA_WS_ENDPOINT` | `ws_endpoint` | `""` |
| `CHIMERA_OPERATOR_TOKEN` | (breaker clear gate) | (must be set) |

If using env vars, do **not** also set the corresponding YAML fields — the env
var wins. Prefer setting all required values explicitly in `pacing.yaml` for
auditability.

### 4.4 Config Validation

```bash
# Verify the YAML loads correctly
cargo test -p chimera-core test_load_valid_config 2>&1 | grep -E "daily=|weekly=|single=|mode="

# Check that keystore paths resolve
ls -la /secure/path/chimera-keystores/treasury.json
ls -la /secure/path/chimera-keystores/workers/

# Verify eoa_pool.json has valid addresses
python3 -m json.tool config/eoa_pool.json
```

- [ ] `treasury_keystore` points to a valid, readable file
- [ ] `worker_keystore_dir` points to a directory containing keystore files
- [ ] `executor_address` is the correct deployed Executor contract address
- [ ] `treasury_address` is the multisig treasury address
- [ ] `eoa_pool_path` points to `config/eoa_pool.json` with non-excluded wallet addresses
- [ ] `CHIMERA_KEYSTORE_PASSWORD` is set in the environment (non-empty)
- [ ] `CHIMERA_OPERATOR_TOKEN` is set (gates `clear_breaker`)

---

## 5. EOA Pool Funding

Worker EOAs need a minimum native ETH balance to pay for gas. Fund them **before**
going live, otherwise transactions will revert immediately.

### 5.1 Determine Required Balances

From `config/pacing.yaml` defaults:

| Parameter | Value | Purpose |
|---|---|---|
| `min_worker_balance_eth` | 0.01 ETH | Minimum balance threshold |
| `refund_topup_eth` | 0.05 ETH | Treasury→worker refund amount |
| Recommended initial funding | ~0.02 ETH/worker | Covers first ~100 transactions at ~20k gas each |

### 5.2 Fund via `fund_eoa.py`

`scripts/fund_eoa.py` reads `config/eoa_pool.json`, checks each non-excluded
wallet's on-chain balance, and sends ETH from the treasury to any wallet below
the threshold.

```bash
# Dry-run first (offline, no RPC needed):
python3 scripts/fund_eoa.py \
    --treasury-key 0xYOUR_TREASURY_KEY \
    --rpc placeholder \
    --dry-run-offline

# Dry-run with live balance check (reads balances, does not send):
python3 scripts/fund_eoa.py \
    --treasury-key 0xYOUR_TREASURY_KEY \
    --rpc <RPC_URL> \
    --dry-run

# [LIVE] Fund under-funded workers:
python3 scripts/fund_eoa.py \
    --treasury-key 0xYOUR_TREASURY_KEY \
    --rpc <RPC_URL> \
    --min-balance 0.01 \
    --fund-amount 0.02 \
    --wait
```

**Before using `fund_eoa.py`, verify the JSON shape:** the script reads the
`wallets` key from `eoa_pool.json` (`scripts/fund_eoa.py:67`). The pool file
at `config/eoa_pool.json` uses the key `wallets`. If the script and pool file
disagree on the key name, resolve the mismatch by editing the pool file or the
script. The current `fund_eoa.py` (July 2026) uses `wallets`.

**Nonce safety:** `fund_eoa.py` fetches the treasury nonce once before the send
loop (`scripts/fund_eoa.py:131-132`) and increments locally per dispatched
transaction (`scripts/fund_eoa.py:155`). This avoids nonce collisions.

### 5.3 Manual Funding Alternative

If you prefer not to use `fund_eoa.py`, manually send ~0.02 ETH from the
treasury to each worker EOA address:

```bash
# For each worker:
cast send <WORKER_EOA_ADDRESS> --value 0.02ether \
    --private-key 0xYOUR_TREASURY_KEY \
    --rpc-url <RPC_URL>
```

### 5.4 Verify Balances

```bash
# Check balances for all workers in eoa_pool.json
python3 -c "
import json
with open('config/eoa_pool.json') as f:
    pool = json.load(f)
for w in pool.get('wallets', []):
    if not w.get('excluded'):
        print(w['address'])
" | while read addr; do
    cast balance "$addr" --rpc-url <RPC_URL>
done
```

- [ ] Treasury funded with sufficient ETH to cover all worker top-ups
- [ ] Every non-excluded worker EOA has ≥ 0.01 ETH
- [ ] No worker holds more than ~0.05 ETH (excess should be swept to treasury)

---

## 6. Go-Live Gate Execution

The go-live gate requires TWO separate actions:

1. `toggle_shadow.py --set-live` — manages `core/state/mode.json` (soak gate)
2. Edit `config/pacing.yaml: execute_mode` to `live` — flips the binary to live execution

Both changes are required. `toggle_shadow.py` explicitly warns that it does
**not** edit `pacing.yaml` (`scripts/toggle_shadow.py:15-19`).

### 6.1 Pre-Flight Checks

Before running the go-live gate:

- [ ] Soak evidence archived (7-day metrics export + log summary)
- [ ] All deployment-checklist §1–4 checks green
- [ ] Multisig ownership confirmed for both contracts (§3)
- [ ] Emergency procedures reviewed by operator on duty
- [ ] Rollback path documented and operator can execute it from memory (§9)
- [ ] Keystore paths valid and keystores decrypt successfully (§2)
- [ ] Worker balances confirmed (§5)
- [ ] Monitoring stack up and reachable (Prometheus + Grafana)
- [ ] RPC endpoint responsive

### 6.2 Verify Soak Clock

```bash
# Show the current mode.json state
python3 scripts/toggle_shadow.py --show

# Expected output (after ≥ 7 days shadow):
# {
#   "previous_mode": "shadow",
#   "shadow_since": <unix_timestamp>,
#   "_exists": true,
#   "_shadow_age": "Xd Yh Zm",
#   "_soak_satisfied": true     <-- must be true
# }

# If _soak_satisfied is false, do NOT proceed. The soak is incomplete.
```

### 6.3 Execute the Gate

```bash
# Step 1: Run toggle_shadow.py --set-live
python3 scripts/toggle_shadow.py --set-live

# This enforces the 7-day rule (604800 seconds).
# If the soak is satisfied, it writes:
#   {"previous_mode": "live", "shadow_since": <preserved_timestamp>}
# If the soak is NOT satisfied, it prints the remaining time and exits 1.
# DO NOT bypass this check.

# Verify the state file:
cat core/state/mode.json
# Expected: {"previous_mode": "live", "shadow_since": <timestamp>}

# Step 2: Edit config/pacing.yaml
# Change this line:
#   execute_mode: shadow
# To:
#   execute_mode: live
```

- [ ] `toggle_shadow.py --set-live` succeeded (exit code 0)
- [ ] `core/state/mode.json` now has `"previous_mode": "live"`
- [ ] `execute_mode: live` in `config/pacing.yaml`
- [ ] Keystore paths set in `config/pacing.yaml` (non-empty, valid)
- [ ] `executor_address` and `treasury_address` set in config

### 6.4 Restart the Binary

```bash
# Ensure the env vars are set:
echo $CHIMERA_KEYSTORE_PASSWORD  # should print the password (non-empty)
echo $CHIMERA_OPERATOR_TOKEN     # should be set

# Start the binary in live mode:
RUST_LOG=chimera=info ./target/release/chimera

# Verify live mode is active:
# Look for these log lines:
#   INFO chimera::signer_registry: Treasury signer loaded address=0x...
#   INFO chimera::signer_registry: Worker signer loaded address=0x...
#   INFO chimera::config: execute_mode=live

# Confirm the breaker is not tripped:
# Port formula: base_port(9100) + (chain_id % 1000). For Base (chain_id=8453): 9100 + 453 = 9553.
curl -s http://localhost:9553 | grep chimera_breaker_state
# Expected: chimera_breaker_state 0

# Confirm the binary did NOT fall back to shadow mode:
# If you see "Running read-only shadow", the keystore config is incomplete.
# If the binary exits immediately, check the error message.
```

**If startup fails with errors like:**

| Error | Cause | Fix |
|---|---|---|
| `live mode requires treasury_keystore` | `treasury_keystore` empty in config | Set path in `pacing.yaml` |
| `live mode requires worker_keystore_dir` | `worker_keystore_dir` empty | Set path in `pacing.yaml` |
| `CHIMERA_KEYSTORE_PASSWORD env var required` | Env var not set | Export the password |
| `failed to decrypt treasury keystore` | Wrong password or corrupt file | Recreate keystore |
| `Live mode: EOA pool entry 0x... has no registered worker signer` | Keystore missing for a worker | Create keystore or mark `excluded: true` |
| `execute_mode=live requires CHIMERA_KEYSTORE_PATH and CHIMERA_KEYSTORE_PASSWORD` | Legacy path check (`main.rs:554`) | Ensure keystore paths are set |

### 6.5 Gate Completion Checklist

- [ ] Binary running with `execute_mode=live`
- [ ] All signers loaded (treasury + all non-excluded workers)
- [ ] `chimera_breaker_state 0`
- [ ] Metrics endpoint responding on `:9553`
- [ ] No ERROR logs in the first 60 seconds

---

## 7. First-Operations Watch

The first live window is a **supervised ramp-up period**. Monitor continuously.

### 7.1 Watch the First ~10 Operations

```bash
# Stream logs to a file for later review
RUST_LOG=chimera=debug ./target/release/chimera 2>&1 | tee logs/go-live-$(date +%Y%m%d-%H%M).log

# In a separate terminal, watch metrics:
watch -n 10 'curl -s http://localhost:9553/metrics | grep -E "chimera_candidates|chimera_sims|chimera_breaker|chimera_profit|chimera_revert|chimera_sweep"'
```

### 7.2 What to Monitor

| Metric | Healthy | Alarm |
|---|---|---|
| `chimera_candidates_seen_total` | Incrementing | Stalled = snapshot/RPC issue |
| `chimera_sims_run_total{result="ok"}` | Incrementing | None passing = strategy issue |
| `chimera_revert_total{reason="execution_revert"}` | 0 or slow growth | Any spike = investigate calldata |
| `chimera_profit_usd` | Positive, within caps | Negative or zero = strategy issue |
| `chimera_sweep_total` | Incrementing during sweeps | Stalled = sweep scheduler issue |
| `chimera_breaker_state` | 0 | 1 = stop immediately |
| `chimera_revert_total` | ≤ 2 | ≥ 3 trips the breaker |
| Stdout logs | `INFO` lines, no `ERROR` | Any ERROR = investigate |

### 7.3 Reduced-Sizing Verification

The first ~10 live ops execute at the conservative caps set in §4.2. Verify:

- [ ] No single operation exceeds `max_single_transfer_usd` ($1,000)
- [ ] Daily net flow stays under `max_daily_net_usd` ($2,000)
- [ ] Gas price stays under `max_gas_gwei` (300 gwei)
- [ ] No breaker auto-trips (0 consecutive reverts)
- [ ] Inclusion rate > 85% (on L2, most transactions should land in the next block)

### 7.4 If Anything Goes Wrong

```bash
# Immediate halt:
python3 scripts/emergency_pause.py \
    --state-file core/state/emergency.flag \
    --reason "<describe what you saw>"

# Verify pause:
cat core/state/emergency.flag

# Binary will detect the flag and halt within one loop iteration.
# Then follow §9 (Rollback Procedure).
```

---

## 8. First Sweep Verification

Sweep consolidates profits from worker EOAs back to the treasury. The built-in
`SweepScheduler` handles this automatically at `sweep_interval_secs` (default
300s), but for the **first live window**, manually verify the sweep path.

### 8.1 The Sweep Path

```
Worker EOA  ──(native ETH sweep)──▶  Treasury (multisig)
Worker EOA  ──(ERC20 sweep)──────▶  Treasury (multisig)
```

The treasury signer signs refund transactions locally (covers worker gas
top-ups at `refund_interval_secs` = 3600s).

### 8.2 Manual Sweep for the First Window

`scripts/sweep_profits.py` supports both native ETH and ERC20 token sweeps.
It is present but the first-run onboarding guide (`docs/first-run-onboarding.md`
§10) notes it should be treated as "verify/extend before relying on." For the
first live window, prefer a manual sweep path.

```bash
# Step 1: Check worker balances
for addr in $(python3 -c "
import json
with open('config/eoa_pool.json') as f:
    pool = json.load(f)
for w in pool.get('wallets', []):
    if not w.get('excluded'):
        print(w['address'])
"); do
    echo "=== $addr ==="
    cast balance "$addr" --rpc-url <RPC_URL>
done

# Step 2: For each worker with balance > min_keep, manually sweep:
# (Keep sweep_min_keep_eth = 0.005 ETH for gas on each worker)
cast send <TREASURY_ADDRESS> \
    --value $(echo "$WORKER_BALANCE - 0.005" | bc)ether \
    --private-key 0xWORKER_PRIVATE_KEY \
    --rpc-url <RPC_URL>

# Step 3: Verify treasury received the funds
cast balance <TREASURY_ADDRESS> --rpc-url <RPC_URL>
```

### 8.3 If Using `sweep_profits.py`

```bash
# Dry-run first to see what would be swept:
python3 scripts/sweep_profits.py \
    --rpc <RPC_URL> \
    --treasury <TREASURY_ADDRESS> \
    --keys-file /secure/path/worker-keys.txt \
    --dry-run

# [LIVE] Execute sweep:
python3 scripts/sweep_profits.py \
    --rpc <RPC_URL> \
    --treasury <TREASURY_ADDRESS> \
    --keys-file /secure/path/worker-keys.txt \
    --wait
```

- [ ] Worker balances are at or below `sweep_min_keep_eth` (0.005 ETH) after sweep
- [ ] Treasury balance increased by the swept amount
- [ ] No worker was drained below the gas reserve
- [ ] Sweep transactions confirmed (receipt status = 1)

---

## 9. Rollback Procedure

Any operator must be able to stop the system and return to safe shadow mode
quickly. Practice this before the go-live gate.

### 9.1 Immediate Halt

```bash
# Write the emergency pause flag
python3 scripts/emergency_pause.py \
    --state-file core/state/emergency.flag \
    --reason "manual go-live rollback"

# Optionally dispatch a webhook alert:
python3 scripts/emergency_pause.py \
    --state-file core/state/emergency.flag \
    --reason "manual go-live rollback" \
    --alert-url https://hooks.slack.com/services/xxx
```

The Rust binary monitors `core/state/emergency.flag` (or the path set via
`CHIMERA_EMERGENCY_FLAG` env var) and halts execution within one loop iteration
(`core/src/orchestrator.rs:138`).

### 9.2 Return to Shadow Mode

```bash
# Step 1: Set mode.json back to shadow
# toggle_shadow.py --set-shadow preserves the soak clock if already set,
# so subsequent re-go-live does not need a new soak.
python3 scripts/toggle_shadow.py --set-shadow

# Step 2: Edit config/pacing.yaml
# Change: execute_mode: live → shadow

# Step 3: If the breaker is tripped, do NOT clear it until root cause is resolved.
# Clearing the breaker requires CHIMERA_OPERATOR_TOKEN:
# (The breaker reset in the binary is gated by this env var;
#  see core/src/pacing_engine.rs:528-533.)

# Step 4: Restart the binary in shadow mode:
RUST_LOG=chimera=info ./target/release/chimera
```

- [ ] Binary running in shadow mode (logs only, no transactions)
- [ ] `chimera_breaker_state 0` (or investigate if 1)
- [ ] Incident documented using the template in `docs/emergency-procedures.md`

### 9.3 Withdraw Funds via Multisig

If funds need to be recovered from the contracts:

```bash
# Executor withdraw (owner-gated, from the multisig):
#   withdraw(address token, uint256 amount)
#     token = 0x0000...0000 means native ETH
#     amount = 0 means withdraw entire balance of that token

# FundDistributor emergency withdraw (owner-gated, from the multisig):
#   emergencyWithdraw() — sends all native ETH to the owner (multisig)

# From the multisig, execute one or both as needed.
```

**Multisig actions (from the Safe/Gnosis UI or script):**

1. Executor `withdraw(address(0), 0)` — withdraws all ETH
2. Executor `withdraw(<token>, <amount>)` — withdraws specific ERC20
3. FundDistributor `emergencyWithdraw()` — withdraws all ETH

### 9.4 Resume from Shadow (After Incident Resolution)

Once the root cause is resolved and recovery checks pass:

```bash
# 1. Review shadow-mode logs
# 2. Confirm strategy parameters
# 3. Run toggle_shadow.py --set-live (soak clock preserved from original stamp)
python3 scripts/toggle_shadow.py --set-live

# 4. Flip execute_mode back to live
# 5. Restart binary
# 6. Resume with reduced sizing for the first 10 transactions
```

- [ ] Incident logged with root cause, resolution, and countermeasures
- [ ] `docs/emergency-procedures.md` recovery checklist complete
- [ ] `health_check.py --rpc <URL> --metrics-port 9553` passes
- [ ] Dry-run path validated before resuming live

---

## A. Quick-Reference Commands

```bash
# Show mode state
python3 scripts/toggle_shadow.py --show

# Set shadow mode (preserves soak clock)
python3 scripts/toggle_shadow.py --set-shadow

# Set live mode (enforces 7-day soak)
python3 scripts/toggle_shadow.py --set-live

# Emergency pause
python3 scripts/emergency_pause.py --state-file core/state/emergency.flag --reason "..."

# Emergency resume
python3 scripts/emergency_pause.py --state-file core/state/emergency.flag --resume

# Check breaker state
curl -s http://localhost:9553 | grep chimera_breaker_state

# Verify keystore decryption (start in shadow with keystore paths set)
RUST_LOG=chimera=debug ./target/release/chimera 2>&1 | grep -i "signer loaded\|failed to decrypt"

# Fund workers (dry-run)
python3 scripts/fund_eoa.py --treasury-key 0x... --rpc <URL> --dry-run-offline

# Sweep profits (dry-run)
python3 scripts/sweep_profits.py --rpc <URL> --treasury 0x... --keys-file keys.txt --dry-run

# Check a contract owner
cast call <ADDRESS> "owner()(address)" --rpc-url <RPC_URL>

# Check chain ID
cast chain-id --rpc-url <RPC_URL>
```

## B. Cross-Reference Index

| Document | Purpose | When to Read |
|---|---|---|
| `docs/deployment-checklist.md` | Full deployment & soak gates | Before this runbook |
| `docs/emergency-procedures.md` | Breaker conditions & recovery | Before go-live; during any incident |
| `docs/operator-manual.md` | Daily operational procedures | After go-live (ongoing) |
| `docs/first-run-onboarding.md` | System overview & safety rules | Before first shadow run |
| `docs/architecture.md` | System design & module boundaries | For deep understanding |
| `docs/threat-model.md` | Security assumptions & risks | Before go-live |
| `docs/snapshot-schema.md` | Snapshot format spec | If snapshot generation fails |
| `config/pacing.yaml` | Live pacing caps & addresses | Edited during go-live (§4) |
| `core/src/signer_registry/mod.rs` | Keystore loading logic | If keystore decryption fails |
| `core/src/config.rs` | Config loading & env overrides | If config parsing fails |
| `scripts/toggle_shadow.py` | Mode transition gate manager | Used in §6 and §9 |
| `scripts/emergency_pause.py` | Emergency stop script | Used in §9 |
| `scripts/fund_eoa.py` | Worker EOA funding | Used in §5 |
| `scripts/sweep_profits.py` | Profit consolidation | Used in §8 |
| `contracts/script/Deploy.s.sol` | Deploy script & ownership setup | Reference for §3 |
| `contracts/src/FundDistributor.sol` | Two-step ownership contract | Reference for §3 |
