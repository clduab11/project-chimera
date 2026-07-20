# Project Chimera — Wallet Provisioning & Testnet Practice Runbook

**Version**: 1.1
**Last Updated**: 2026-07-09
**Audience**: Solo operator provisioning worker/treasury keystores and rehearsing the full funding loop on Base Sepolia.
**Prerequisites**: Repository checked out; Python 3.11+ with `requirements.txt` installed (web3/eth_account needed only for generation and online steps — `--help`, `--dry-run`, `plan`, and `shadow-env` run stdlib-only).

> **Operator-executed runbook — do not execute during authoring.**
>
> Every command below is written for the operator on a provisioned workstation.
> Read the full runbook before starting. Total real (mainnet) funds used by this
> runbook: **0**. All on-chain activity is Base Sepolia testnet ETH.
>
> Cross-references:
> - Testnet deployment: `docs/runbook-testnet-deploy.md`
> - Keystore/multisig/go-live: `docs/runbook-keystore-multisig-go-live.md`
> - Shadow soak: `docs/runbook-7day-soak.md`
> - Invariants and validation gate: `AGENTS.md`

---

## 1. Purpose & Scope

This runbook covers two tools and one rehearsal:

| Component | Tool | Outcome |
| --- | --- | --- |
| Keystore provisioning | `scripts/provision_wallets.py` | Encrypted worker/treasury keystores generated outside the repo; `config/eoa_pool.json` (or the sepolia pool) synced with public addresses only |
| Testnet practice loop | `scripts/testnet_harness.py` | Base Sepolia (chain_id 84532) provision → faucet → fund → status → verify → shadow boot, end to end |

**In scope:** keystore generation, pool synchronization, verification parity
with `SignerRegistry`, faucet funding on Base Sepolia, worker fan-out funding,
and a shadow-mode boot drill that decrypts every keystore with zero risk.

**Out of scope:** the `toggle_shadow.py --set-live` mode step is **unchanged**
by this tooling and is not exercised here. (The 7-day soak gate was de-listed
by operator decision on 2026-07-20.) Live go-live remains the manual path in
`docs/runbook-keystore-multisig-go-live.md` §6. Nothing in this runbook sets
any mode to `live`.

The live model uses these worker EOAs only to sign ordinary EIP-1559 calls to
the standalone Executor's `execute(bytes)` entrypoint. There is no EIP-7702 or
delegation. Before live use, the Executor-owner multisig must authorize every
active worker with `setWorker(worker, true)`.

---

## 2. Custody Rules (Mechanical, Not Aspirational)

These rules are enforced by the tools themselves, not by operator discipline:

1. **Password via environment only.** The keystore password is read exclusively
   from `CHIMERA_KEYSTORE_PASSWORD`. There is no password CLI flag (nothing to
   leak via shell history or process lists), and the password is never logged
   or echoed back. Set it manually:

   PowerShell:
   ```powershell
   $env:CHIMERA_KEYSTORE_PASSWORD = "<set by operator>"
   ```

   POSIX:
   ```bash
   export CHIMERA_KEYSTORE_PASSWORD='<set by operator>'
   ```

   Verify presence without printing it: `test -n "$CHIMERA_KEYSTORE_PASSWORD"`.

2. **Keystore format.** Web3 Secret Storage Definition V3: scrypt KDF
   (n=2^18, r=8, p=1) + `aes-128-ctr` cipher, plaintext `address` field.
   This is the same format documented in
   `docs/runbook-keystore-multisig-go-live.md` §2.1 and is decryptable by
   alloy's `PrivateKeySigner::decrypt_keystore` — the exact call
   `SignerRegistry::load` makes at boot (`core/src/signer_registry/mod.rs`).
   Files are interchangeable with `cast wallet import` output.

3. **Keystores live outside the repository.** Default root:

   ```
   ~/.chimera/keystores/
   ├── workers/          # mainnet worker keystores
   ├── testnet/          # testnet subtree (workers/ + treasury.json)
   └── treasury.json     # mainnet treasury keystore
   ```

   Both tools resolve the target directory and **refuse (exit 2) any keystore
   directory that resolves to a path inside the repository root**. This guard
   is not overridable.

4. **No key-export path exists.** No flag prints, copies, or re-encrypts a
   decrypted key. `--verify` derives addresses in memory and discards signers.
   Private key bytes exist only between generation and encryption inside one
   function scope.

5. **Only public addresses enter config files.** The pool files
   (`config/eoa_pool.json`, `config/eoa_pool.sepolia.json`) receive addresses,
   labels, and rotation metadata — never key material.

---

## 3. Provisioning Workers & Treasury — `scripts/provision_wallets.py`

### 3.1 Commands

```bash
# Plan only — stdlib-only, reads no password, generates NOTHING
python scripts/provision_wallets.py --dry-run

# Provision workers to the target count (default pool + default keystore dir)
python scripts/provision_wallets.py

# Provision the single treasury keystore (pool never read or written)
python scripts/provision_wallets.py --role treasury

# Verify keystore/pool parity (see §3.3)
python scripts/provision_wallets.py --verify
```

Useful flags: `--eoa-pool <path>`, `--keystore-dir <path>`, `--count <n>`,
`--json`. There is deliberately no password flag.

### 3.2 What a worker run does

1. Resolves the keystore dir (`--keystore-dir` > `$CHIMERA_WORKER_KEYSTORE_DIR`
   > `worker_keystore_dir` in `config/pacing.yaml` > `~/.chimera/keystores/workers`)
   and refuses any dir inside the repo (exit 2).
2. Inventories existing keystores by their plaintext `address` field (no
   password needed).
3. Classifies each pool entry: entries with a matching keystore are kept
   untouched; committed placeholder rows (zero history) are replaced; entries
   with rotation history but no keystore are warned about and never touched;
   `excluded: true` entries are never touched.
4. Generates only the missing wallets, writes keystores atomically with
   restrictive permissions, then rewrites the pool (keystores first, pool
   second — an orphan keystore is harmless; a pool row without a keystore
   would fail live validation).
5. Reports created addresses/labels and the exact `CHIMERA_*` env lines you
   need. `config/pacing.yaml` is never edited (Invariant #1).

### 3.3 `--verify`: predicting SignerRegistry acceptance

`--verify` decrypts every keystore in memory and re-implements
`SignerRegistry::validate_against_eoa_pool` in Python: every non-excluded,
non-zero pool address must map to a decrypted keystore address; extra
keystores produce a warning only.

- **Exit 0** ⇒ the Rust live-mode pool validation in `SignerRegistry::load`
  will pass.
- **Exit 1** ⇒ names the offending address using the same message shape as the
  Rust error (`Live mode: EOA pool entry 0x... has no registered worker signer`).

### 3.4 Idempotency contract

Running the tool N times with the same inputs yields: identical pool bytes
after run 1, an identical keystore file set (names + content hashes) after
run 1, and exit 0 with "0 wallets to create" thereafter. Presence of a
matching keystore is the fixpoint condition — re-running is always safe.

---

## 4. Testnet Harness — `scripts/testnet_harness.py`

### 4.1 Subcommands

All subcommands honor `--config config/testnet.base-sepolia.json` and
`--eoa-pool config/eoa_pool.sepolia.json` (the defaults).

| Subcommand | Network | Action |
| --- | --- | --- |
| `plan` (default) | offline | Prints the full practice sequence, resolved paths, env block, and faucet instructions. Works with zero deps installed |
| `provision` | offline | Testnet treasury keystore + 3 worker keystores into `~/.chimera/keystores/testnet/{treasury.json,workers/}`, syncing the sepolia pool |
| `faucet` | online (read-only) | Prints the treasury address + faucet table; `--wait` polls the balance every 15s until the target is reached |
| `fund` | online (testnet writes) | Decrypts the testnet treasury in memory and fans out ETH to workers via `fund_eoa.fund_wallets` (chain-guarded); supports `--dry-run` |
| `status` | online or `--mock` | Balance table via `check_balances.collect_balances`; supports `--min-balance` and `--json` |
| `shadow-env` | offline | Prints copy-paste env blocks (PowerShell + POSIX) for the shadow drill (§6) |

### 4.2 The non-negotiable chain guard

Every online subcommand's **first** RPC call is `eth_chainId`. If the result
is not `84532`, the harness exits 3 before making any other call. `fund`
re-asserts the guard immediately before the first `send_raw_transaction`.
**There is no override flag.** The harness is structurally incapable of moving
mainnet funds.

Secondary guards on `fund`:

- `--eoa-pool config/eoa_pool.json` (the mainnet pool) is refused with exit 3.
- The resolved keystore root must be under `~/.chimera/keystores/testnet/`;
  a mainnet treasury keystore can never be decrypted by the harness.

---

## 5. Faucet Flow & Practice Sequence

### 5.1 Sizing

- 3 workers × 0.02 ETH top-up (+ gas) ⇒ treasury target **0.1 ETH** — inside a
  single grant from any faucet below.
- Total real (mainnet) funds used: **0**.

### 5.2 Faucets (verify amounts at run time)

| # | Faucet | URL | Notes (verify amounts at run time) |
|---|---|---|---|
| 1 | Coinbase Developer Platform | https://portal.cdp.coinbase.com/products/faucet | Base Sepolia native; CDP account; ~0.1 ETH/day |
| 2 | Alchemy Base Sepolia | https://www.alchemy.com/faucets/base-sepolia | Alchemy account; mainnet-balance gated |
| 3 | Superchain (Optimism) | https://console.optimism.io/faucet | GitHub/onchain identity gate |
| 4 | thirdweb | https://thirdweb.com/base-sepolia-testnet | Wallet login |
| 5 | Fallback: bridge | Google Cloud / sepoliafaucet → https://bridge.base.org | L1 Sepolia ETH bridged to Base Sepolia (~minutes) |

### 5.3 The 7-step sequence

Set `CHIMERA_KEYSTORE_PASSWORD` first (§2 item 1). Then:

1. **Provision** (offline) — treasury + 3 worker keystores, sepolia pool synced:
   ```bash
   python scripts/testnet_harness.py provision
   ```
2. **Faucet** — paste the printed treasury address into faucet #1 (or any);
   the harness polls until the balance reaches 0.1 ETH:
   ```bash
   python scripts/testnet_harness.py faucet --wait
   ```
3. **Fund** — dry-run first, then fan out 0.02 ETH per worker (chain-guarded):
   ```bash
   python scripts/testnet_harness.py fund --dry-run
   python scripts/testnet_harness.py fund
   ```
4. **Status** — all 3 workers must report `ok`:
   ```bash
   python scripts/testnet_harness.py status --min-balance 0.01
   ```
5. **Verify** — Python parity check green (predicts SignerRegistry acceptance):
   ```bash
   python scripts/provision_wallets.py --verify --eoa-pool config/eoa_pool.sepolia.json
   ```
6. **Shadow boot** — emit the env block, boot `target/release/chimera`, and
   confirm exactly 1× `Treasury signer loaded` + 3× `Worker signer loaded`
   plus a clean shutdown (details in §6):
   ```bash
   python scripts/testnet_harness.py shadow-env
   ```
7. **Optional rotation drill** — exercise the cooldown machinery on the
   testnet pool with zero writes:
   ```bash
   python scripts/rotate_eoa.py --config config/eoa_pool.sepolia.json --chain base --dry-run
   ```

Capture evidence per the plan's manual acceptance: harness transcripts,
`status --json` output, and the shadow-boot log excerpt showing the four
"signer loaded" lines.

---

## 6. Shadow Practice Drill (Zero-Real-Funds Proof of the Whole Chain)

### 6.1 The env block

`shadow-env` emits exactly this delta — **`config/pacing.yaml` is never edited
(Invariant #1)**; these env overrides are the entire testnet configuration:

PowerShell:

```powershell
$env:CHIMERA_EXECUTE_MODE = "shadow"   # explicit restatement of the default; never any other value
$env:CHIMERA_CHAIN_ID = "84532"
$env:CHIMERA_EOA_POOL_PATH = "config/eoa_pool.sepolia.json"
$env:CHIMERA_WORKER_KEYSTORE_DIR = "$HOME\.chimera\keystores\testnet\workers"
$env:CHIMERA_TREASURY_KEYSTORE = "$HOME\.chimera\keystores\testnet\treasury.json"
# $env:CHIMERA_KEYSTORE_PASSWORD = "<set by operator>"  # set manually — never emitted by tooling
```

POSIX:

```bash
export CHIMERA_EXECUTE_MODE=shadow     # explicit restatement of the default; never any other value
export CHIMERA_CHAIN_ID=84532
export CHIMERA_EOA_POOL_PATH=config/eoa_pool.sepolia.json
export CHIMERA_WORKER_KEYSTORE_DIR="$HOME/.chimera/keystores/testnet/workers"
export CHIMERA_TREASURY_KEYSTORE="$HOME/.chimera/keystores/testnet/treasury.json"
# export CHIMERA_KEYSTORE_PASSWORD='<set by operator>'  # set manually — never emitted by tooling
```

Then boot: `target/release/chimera` (or `chimera.exe` on Windows).

### 6.2 Why this exercises the real code path with zero risk

Per `core/src/signer_registry/mod.rs` shadow semantics: in shadow mode with
**populated** keystore paths, `SignerRegistry::load` proceeds past the
empty-registry short-circuit, requires `CHIMERA_KEYSTORE_PASSWORD`, and
decrypts every worker + treasury keystore — the identical decryption path used
in live mode — while `execute_mode: shadow` keeps all transaction paths dead.
Nothing is ever signed or sent.

### 6.3 Expected boot output

```
INFO chimera::signer_registry: Treasury signer loaded address=0x...   # exactly 1
INFO chimera::signer_registry: Worker signer loaded address=0x...     # exactly 3
```

Verify exactly as `docs/runbook-keystore-multisig-go-live.md` §2.5 does:

```bash
RUST_LOG=chimera=info target/release/chimera 2>&1 | grep -i "signer loaded"
```

**Two expected, benign log lines:**

1. An ETH price fallback line: the configured Chainlink ETH/USD feed address
   is the mainnet one, which has no code on Base Sepolia, so the engine uses
   `eth_price_usd_fallback: 1800` by design (see
   `docs/runbook-testnet-deploy.md` §3.3). This is expected, not an error.

---

## 7. Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| `CHIMERA_KEYSTORE_PASSWORD` missing/empty error (exit 2) | Password env var not set | Set it per §2 item 1; never pass it as a flag (no flag exists) |
| Refusal with exit 2 before anything is written | Keystore dir resolves inside the repository root | Use a dir outside the repo (default `~/.chimera/keystores/...`); this guard is not overridable |
| Harness exits 3 immediately | Chain guard: RPC's `eth_chainId` != 84532, or `--eoa-pool config/eoa_pool.json` (mainnet pool) was passed, or keystore root is outside the `testnet/` subtree | Point at a Base Sepolia RPC; use `config/eoa_pool.sepolia.json`; keep testnet keystores under `~/.chimera/keystores/testnet/` |
| `--verify` exits 1 naming an address | That non-excluded pool address has no decrypting keystore | Re-run provisioning for the missing wallet, fix the password, or mark the entry `excluded: true` (see `runbook-keystore-multisig-go-live.md` §2.5) |
| Faucet dry spell (no grant available) | Daily limits / identity gates on faucets 1-4 | Use the fallback bridge (row 5 of §5.2): obtain L1 Sepolia ETH, bridge via https://bridge.base.org, then resume at step 2 with `faucet --wait` |
| `Skipping unreadable keystore file` at boot | Password mismatch or corrupt/foreign-format file | Confirm the shared password; regenerate the keystore (V3 scrypt + aes-128-ctr only) |

## 8. Mainnet Hand-Off

This runbook proves keystore and gas-funding mechanics only. Mainnet live
startup additionally requires:

- `CHIMERA_EXECUTOR_ADDRESS`, `CHIMERA_TREASURY_ADDRESS`,
  `CHIMERA_TREASURY_KEYSTORE`, `CHIMERA_WORKER_KEYSTORE_DIR`, and
  `CHIMERA_EOA_POOL_PATH` set to deployment-local values;
- `BASE_RPC_URL` or `RPC_URL` set;
- Executor bytecode present, `pool()` equal to the canonical Aave Pool, and
  `isWorker(address)` true for every active worker;
- treasury signer/address parity, active signer/pool parity, treasury refund
  ETH, and every worker at or above `min_worker_balance_eth`;
- the `toggle_shadow.py --set-live` mode-state step (7-day soak gate de-listed 2026-07-20);
- explicit acceptance or remediation of standard-RPC submission exposure,
  because private/protected submission is not wired.

Worker and treasury funding is gas ETH only. Aave supplies liquidation capital.
Debt-token profit remains in Executor and is withdrawn by the owner multisig.
The Rust scheduler sweeps/refunds worker native ETH but does not sweep ERC20s;
`sweep_tokens` is currently unused. `scripts/sweep_profits.py` is a legacy/manual
raw-key helper, not the Executor-profit or primary scheduled path.

---

## 9. Cross-Reference Index

| Document | Relationship |
| --- | --- |
| `docs/runbook-keystore-multisig-go-live.md` | Mainnet keystore creation, live-mode config, go-live gate. This runbook's keystores use the same V3 format and the same `CHIMERA_KEYSTORE_PASSWORD` convention (§2) |
| `docs/runbook-testnet-deploy.md` | Base Sepolia contract deployment, addresses (§3.1, Appendix A), and the `eth_price_usd_fallback` behavior referenced in §6.3 |
| `docs/runbook-7day-soak.md` | Optional shadow rehearsal — the mandatory soak was de-listed by operator decision on 2026-07-20 |
| `AGENTS.md` | Invariant #1 (`config/pacing.yaml` frozen — env overrides only) and Invariant #5 (scripts importable without web3); validation gate |
| `core/src/signer_registry/mod.rs` | Keystore decryption, shadow semantics, and `validate_against_eoa_pool` mirrored by `--verify` |
