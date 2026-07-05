# Plan: Worker-Wallet Provisioning Service + Testnet-First Practice Harness

**Status**: DRAFT — awaiting operator approval. No code written yet.
**Author**: agent session, 2026-07-05
**Scope**: Two components. (1) `scripts/provision_wallets.py` — encrypted worker/treasury
keystore generation + `eoa_pool.json` public-address sync. (2) `scripts/testnet_harness.py`
— Base Sepolia (chain_id 84532) generate → faucet → fund → shadow practice loop with zero
real funds.

---

## 0. Grounding: what the code actually does (code > docs)

Every design decision below traces to one of these verified facts:

| # | Fact | Source |
|---|------|--------|
| G1 | `SignerRegistry::load` decrypts keystores with alloy `PrivateKeySigner::decrypt_keystore(path, password)`; password comes only from `CHIMERA_KEYSTORE_PASSWORD` | core/src/signer_registry/mod.rs:64,103,138 |
| G2 | Shadow mode + **empty** keystore paths → empty registry, no password needed. Shadow mode + **populated** paths → keystores are decrypted (password required) but nothing is ever signed/sent. Live mode additionally requires every non-excluded, non-zero pool address to map to a decrypted signer | core/src/signer_registry/mod.rs:69-78, 93-97, 156-171, 205-237 |
| G3 | `validate_against_eoa_pool` skips `excluded: true` entries and `Address::ZERO`; any other unmapped address is a hard startup failure in live mode | core/src/signer_registry/mod.rs:217-235 |
| G4 | The binary hardcodes `PacingConfig::load_with_env("config/pacing.yaml")`; every field is overridable via `CHIMERA_*` env vars (`CHIMERA_CHAIN_ID`, `CHIMERA_EOA_POOL_PATH`, `CHIMERA_WORKER_KEYSTORE_DIR`, `CHIMERA_TREASURY_KEYSTORE`, ...) | core/src/main.rs:134, core/src/config.rs:248-301 |
| G5 | Invariant #1 requires `config/pacing.yaml` to keep matching `config.rs` defaults + `valid_yaml()` fixture — i.e. `chain_id: 8453`, `execute_mode: shadow`, empty `treasury_keystore` / `worker_keystore_dir` / `executor_address` **must stay as-is in the repo** | AGENTS.md; core/src/config.rs:118-155, 315-349; config/pacing.yaml:24,27,39-42 |
| G6 | `eoa_pool.json` v2.0 schema: top-level `version`, `_comment`, `wallets[]`; each wallet has exactly `address`, `label`, `chain_balances{base,arbitrum}`, `last_used`, `use_count`, `excluded`. On-disk balances intentionally zeroed | config/eoa_pool.json:1-15 |
| G7 | `rotate_eoa.py` owns rotation semantics (24h cooldown, least-`use_count` then oldest-`last_used`, skip excluded). **Its serializer is lossy**: `EOAPool.to_dict()` drops `_comment` and `Wallet(**w)` would crash on unknown keys — so the provisioning tool must round-trip the pool as raw JSON, not through rotate_eoa's dataclasses | scripts/rotate_eoa.py:56-70, 118-157 |
| G8 | `rotate_wallet.py` is a thin delegator (sys.path insert + import) — the established pattern for reusing script logic without forking | scripts/rotate_wallet.py:33-43 |
| G9 | `fund_eoa.py` exposes importable `load_wallet_addresses()` / `fund_wallets()` with once-up-front nonce handling; takes the treasury key as a **function argument** (the CLI passes it via argv, which we will avoid for the harness). `check_balances.py` exposes `collect_balances()` / `print_table()` and runs fully offline via `--mock` | scripts/fund_eoa.py:50-196, scripts/check_balances.py:159-237 |
| G10 | Invariant #5 pattern: `try: from web3 import Web3 except ImportError: Web3 = None`; `--help` must work web3-less | scripts/check_balances.py:32-35, fund_eoa.py:34-37 |
| G11 | Keystore format in production use: "Web3 Secret Storage JSON, scrypt (n=2^18, r=8, p=1) + aes-128-ctr", created outside the repo; all keystores share one password | docs/runbook-keystore-multisig-go-live.md §2.1-2.3 |
| G12 | Testnet target: Base Sepolia, chain_id 84532; Aave V3 Pool `0x07eA79F68B2B3df56440f754B6aA0E498b9B75Fb`, WETH `0x4200...0006`, USDC `0x036C...F7e`; no Chainlink ETH/USD feed at the mainnet address → `eth_price_usd_fallback: 1800` engages | docs/runbook-testnet-deploy.md §3.1, §3.3, Appendix A |
| G13 | `.gitignore` already excludes `wallets/`, `private_keys/`, `*.key`, `*.pem`, `.env` | .gitignore:33-40 |
| G14 | Mode-transition gate lives in `toggle_shadow.py` + `PacingConfig::validate_mode_transition` (7-day soak). We do not touch either | core/src/config.rs:219-246; docs/runbook-keystore-multisig-go-live.md §6 |

**Two discrepancies found while grounding (docs corrected by code):**

1. The referenced `docs/funding-and-testing-runbook.md` **does not exist**. The real
   test-first sequence is split across `docs/runbook-testnet-deploy.md` (testnet, shadow
   first, config via env/sed, smoke last) and `docs/runbook-keystore-multisig-go-live.md`
   (keystores → funding → gate). This plan grounds on those two files.
2. The request says "EIP-2335 Web3 Secret Storage". These are two different specs.
   **EIP-2335 is the BLS12-381 validator keystore format and would NOT decrypt with
   alloy's `decrypt_keystore`** (G1). The correct, code-compatible format is the
   **Web3 Secret Storage Definition (V3)**. See §4 for the formal decision.

---

## 1. Hard constraints → design responses

| Constraint | Design response |
|---|---|
| No private key in repo, logs, or git | Keys generated in memory only; encrypted keystores written **outside the repo** (guard refuses any target dir under the repo root); pool file receives addresses only; log statements are structurally unable to reference key material (address/label/path only); dry-run performs **zero key generation**; pytest greps every produced byte for key hex (§7) |
| Do not modify pacing engine, breakers, mode gate, execute mode | Zero edits to `core/`, `contracts/`, `config/pacing.yaml`, `toggle_shadow.py`, `emergency_pause.py`. New code is additive: 2 scripts, 2 config templates, 1 doc, tests, 1-line `.gitignore` hardening |
| Shadow stays default; no path flips to live | Neither new script reads, writes, or sets `execute_mode`/`CHIMERA_EXECUTE_MODE` to anything but the literal `shadow` (harness exports `CHIMERA_EXECUTE_MODE=shadow` explicitly, redundantly). A static test asserts the string `live` is never assigned to any mode variable in the new scripts (§7, T-12) |
| Custody human-gated | Password only via `CHIMERA_KEYSTORE_PASSWORD` (never a CLI flag → never in shell history/process list). No decrypt-and-print, no export flag exists at all. Real-fund actions remain the manual mainnet runbook path; the harness's automated funding is chain-guarded to 84532 so it can only ever move testnet ETH |
| Reuse rotation/cooldown/pool schema | Rotation metadata (`last_used`, `use_count`, `excluded`) is preserved verbatim; the tool never selects or rotates wallets (that stays `rotate_eoa.py`'s job); harness funds via imported `fund_eoa.fund_wallets` and reports via imported `check_balances.collect_balances` (G8 delegator pattern). No parallel pool, no parallel cooldown |
| Invariant #1 (pacing.yaml frozen) | Testnet configuration flows **exclusively** through documented `CHIMERA_*` env overrides (G4). `config/pacing.yaml` is not touched |
| Invariant #5 (import w/o web3, `--help` works) | Both scripts use the G10 try/except pattern for `eth_account`/`web3`; `--help`, `--dry-run`, `plan`, and pool-only operations run stdlib-only |
| Invariants #2, #3, #4, #6 | No Rust, no contracts, no snapshot schema changes → trivially unaffected |

---

## 2. File list

### New files

| Path | Purpose |
|---|---|
| `scripts/provision_wallets.py` | Component 1: keypair generation → encrypted V3 keystores (outside repo) → pool sync (addresses only). Idempotent, `--dry-run`, `--verify` |
| `scripts/testnet_harness.py` | Component 2: Base Sepolia practice loop (`plan` / `provision` / `faucet` / `fund` / `status` / `shadow-env` subcommands). Imports provision_wallets, fund_eoa, check_balances |
| `config/testnet.base-sepolia.json` | Testnet constants (chain_id 84532, RPC, faucet URLs, Aave/token addresses, funding targets). JSON so it parses with stdlib only. Schema documented inline via `_comment` keys |
| `config/eoa_pool.sepolia.json` | Testnet EOA pool template — same v2.0 schema as G6, 3 placeholder workers (small pool keeps faucet demand within one grant) |
| `docs/runbook-wallet-provisioning.md` | Operator runbook: provisioning usage, custody rules, full faucet flow, shadow practice sequence, troubleshooting. Cross-referenced from the two existing runbooks' style |
| `tests/test_provision_wallets.py` | pytest suite incl. the dry-run no-plaintext proof (§7) |
| `tests/test_testnet_harness.py` | pytest suite: chain guard, offline behavior, no-live-flip static check |

### Modified files

| Path | Change |
|---|---|
| `.gitignore` | Append `keystores/` (belt-and-suspenders; default target is outside the repo already) |

### Explicitly untouched

`core/**` (incl. `signer_registry/mod.rs`, `config.rs`, `pacing_engine`), `contracts/**`,
`config/pacing.yaml`, `config/eoa_pool.json` *as committed* (the tool mutates it only when
an operator runs it — the committed template is not changed by this work),
`scripts/rotate_eoa.py`, `scripts/rotate_wallet.py`, `scripts/fund_eoa.py`,
`scripts/check_balances.py`, `scripts/toggle_shadow.py`, `scripts/sweep_profits.py`.

---

## 3. Component 1 — `scripts/provision_wallets.py`

### 3.1 Behavior

```
python scripts/provision_wallets.py --dry-run                # plan only, stdlib-only, generates nothing
python scripts/provision_wallets.py                          # provision workers to target count
python scripts/provision_wallets.py --role treasury          # single treasury keystore, pool untouched
python scripts/provision_wallets.py --verify                 # parity check mirroring SignerRegistry
```

Pipeline (worker role):

1. **Resolve keystore dir** (first non-empty wins):
   `--keystore-dir` → `$CHIMERA_WORKER_KEYSTORE_DIR` → `worker_keystore_dir` in
   `config/pacing.yaml` (read by a stdlib line-regex, no YAML dep; currently empty per G5)
   → default `~/.chimera/keystores/workers`.
2. **Repo-containment guard**: `resolve()` the dir; if it is inside the repository root
   (detected by walking up from `__file__` to the dir containing `.git`), exit 2 before
   creating anything. Not overridable. Enforces "keystores outside the repository" (G11).
3. **Inventory existing keystores** by reading each JSON file's public `address` field
   (V3 keystores carry the address in plaintext — no decryption, no password needed).
4. **Load pool as raw dict** (preserves `version`, `_comment`, unknown keys — avoids
   rotate_eoa's lossy serializer, G7).
5. **Classify each wallet entry**:
   - `REAL` — address has a matching keystore file → never touched.
   - `PLACEHOLDER` — no matching keystore AND `use_count == 0` AND `last_used == 0`
     AND not `excluded` (covers the committed `0x1111...` template rows) → replaceable.
   - `FOREIGN` — no keystore but has rotation history → **never touched**, loud warning
     (operator may hold that key elsewhere; destroying the entry would break live validation
     the other way).
   - `EXCLUDED` — never touched, per G3 semantics.
6. **Build plan**: keep all REAL; replace PLACEHOLDERs in order (preserving each row's
   `label` and resetting nothing — placeholder rows are already `0/0/false`); append new
   rows `chimera-eoa-NN` only if pool has fewer than `--count` non-excluded entries.
7. **Execute** (skipped entirely under `--dry-run`):
   a. Read password from `CHIMERA_KEYSTORE_PASSWORD`; refuse empty; warn if < 12 chars.
   b. For each planned wallet: `eth_account.Account.create()` (OS CSPRNG) in memory →
      `Account.encrypt(key, password, kdf="scrypt")` (n=2^18, r=8, p=1, aes-128-ctr —
      matches G11 and alloy's decrypt support) → atomic write
      (`os.open(..., O_CREAT|O_EXCL|O_WRONLY, 0o600)` + write + `os.replace` from a
      same-dir temp file; permissions best-effort on Windows) as
      `<label>--<0xAddress>.json`. Local key variable deleted immediately after encrypt.
   c. **Keystores first, pool second** (crash ordering: an orphan keystore is harmless;
      a pool row without a keystore fails live validation, G3).
   d. Rewrite pool atomically (temp file + `os.replace`), emitting exactly the G6 schema:
      new rows get `chain_balances: {"base": "0", "arbitrum": "0"}`, `last_used: 0`,
      `use_count: 0`, `excluded: false`. `version: "2.0"` and `_comment` pass through
      byte-preserved (`json.dump(..., indent=2)` matching existing formatting).
8. **Report**: table of kept/created addresses (addresses and labels only), the exact
   env lines the operator needs (`CHIMERA_WORKER_KEYSTORE_DIR=...`), and a reminder that
   `config/pacing.yaml` stays untouched per Invariant #1.

Treasury role (`--role treasury`): steps 1-2 and 7a-7b only, single file
(`treasury.json` under `~/.chimera/keystores/` or `$CHIMERA_TREASURY_KEYSTORE`'s parent),
pool never read or written. Prints `CHIMERA_TREASURY_KEYSTORE=...` line.

`--verify` mode: decrypts every keystore in the dir in memory (requires `eth_account`;
graceful G10 fallback otherwise) and re-implements `validate_against_eoa_pool` in Python:
every non-excluded, non-zero pool address must map to a derived signer address, and every
derived address should appear in the pool (extra keystores → warning, matching
SignerRegistry's tolerance of extra signers). Exit 0 = the Rust `SignerRegistry::load`
live-mode check will pass; exit 1 names the offending address using the same message shape
as mod.rs:230-233.

### 3.2 Secrecy guarantees (mechanical, not aspirational)

- Private key bytes exist only between `Account.create()` and `Account.encrypt()` inside
  one function scope; the only value that escapes is `acct.address`.
- No f-string/log call in the module ever receives the key object; the logger is handed
  addresses, labels, counts, and paths exclusively (compile-time greppable, tested T-4/T-5).
- Password: env-only. Never argv (process lists), never logged, never echoed back.
- `--dry-run` short-circuits before password read and before any `Account` call — proven
  by monkeypatch sentinel in T-4.
- No key-export path exists: no flag prints, copies, or re-encrypts a decrypted key.
  `--verify` derives addresses in memory and discards signers.

### 3.3 Idempotency contract

Running the tool N times with the same inputs yields: identical pool bytes after run 1,
identical keystore file set (names + content hashes) after run 1, exit 0 with
"0 wallets to create" thereafter. Guaranteed by the REAL/PLACEHOLDER classification in
3.1(5) — presence of a matching keystore is the fixpoint condition. Tested by T-6.

---

## 4. Keystore-format decision

**Decision: Web3 Secret Storage Definition, version 3** — scrypt KDF (n=2^18, r=8, p=1,
dklen=32) + `aes-128-ctr` cipher, standard JSON layout with plaintext `address` field.
Produced by `eth_account.Account.encrypt(..., kdf="scrypt")`.

Rationale (all code-grounded):

1. **The consumer dictates the format.** `SignerRegistry` loads via alloy
   `PrivateKeySigner::decrypt_keystore` (mod.rs:103,138), backed by the `eth-keystore`
   crate, which decrypts **only** Web3 Secret Storage V3 (scrypt or pbkdf2 + aes-128-ctr).
2. **The request's "EIP-2335" label is corrected, not followed.** EIP-2335 is the
   BLS12-381 keystore for consensus-layer validators (different KDF envelope, `pubkey`/
   `path` fields, no secp256k1 semantics). An EIP-2335 file in `worker_keystore_dir`
   would be skipped as "unreadable keystore" (mod.rs:146-152) and live mode would then
   fail pool validation. Since the same request requires "SignerRegistry load validating
   cleanly" and instructs us to trust code over documentation, V3 is the only admissible
   choice.
3. **Operational precedent.** The go-live runbook (G11) already standardizes V3 scrypt
   n=2^18/r=8/p=1 + aes-128-ctr, and `cast wallet import` / `eth_account.Account.encrypt`
   both emit it. Files produced by this tool are interchangeable with cast-produced ones.
4. **Idempotency for free.** V3 keeps the public address in plaintext JSON, letting the
   tool inventory existing keystores without ever prompting for the password (§3.1(3)).

Parameters: scrypt over pbkdf2 (stronger memory-hardness; alloy supports both so this is
a policy choice, matching G11). One shared password for treasury + workers, per
SignerRegistry's single-password design (mod.rs:64,93-97).

---

## 5. Component 2 — `scripts/testnet_harness.py`

### 5.1 Shape

`argparse` subcommands; every subcommand honors `--config config/testnet.base-sepolia.json`
and `--eoa-pool config/eoa_pool.sepolia.json`. Style clones `check_balances.py` (module
logger, G10 optional-web3 import, exit codes, `--json` where meaningful).

| Subcommand | Network | Action |
|---|---|---|
| `plan` (default) | offline | Print the full practice sequence, resolved paths, env block, and faucet instructions. Works with zero deps installed |
| `provision` | offline | Calls `provision_wallets` functions (imported per G8 pattern): testnet treasury keystore + N worker keystores into `~/.chimera/keystores/testnet/{treasury.json,workers/}`, syncing `config/eoa_pool.sepolia.json` |
| `faucet` | online (read-only) | Print treasury address + faucet URLs; with `--wait`, poll balance every 15s until `funding.treasury_target_eth` reached |
| `fund` | online (testnet writes) | Decrypt testnet treasury **in memory** (password from env), then delegate to imported `fund_eoa.fund_wallets(treasury_key=<in-memory>, rpc=..., eoa_pool_path=<sepolia pool>, min_balance, fund_amount, wait=True, dry_run=args.dry_run)` — reusing its nonce-safe loop verbatim (G9). Key passed as function argument, never argv |
| `status` | online or `--mock` | Delegate to imported `check_balances.collect_balances` + `print_table` against the sepolia pool |
| `shadow-env` | offline | Print copy-paste env blocks (PowerShell and POSIX) to boot the existing binary in shadow on Base Sepolia; optionally `--emit-dotenv <path outside repo>` |

### 5.2 The non-negotiable chain guard

Every online subcommand's first RPC call is `eth_chainId`; if it differs from the config's
`chain_id` (84532) the harness exits 3 **before any other call**. `fund` additionally
re-asserts the guard immediately before the first `send_raw_transaction`. There is no
override flag. Consequence: the harness is structurally incapable of moving mainnet funds,
which is what makes automated testnet funding compatible with the "real-fund actions stay
manual" custody rule.

Secondary guards in `fund`:
- refuses `--eoa-pool config/eoa_pool.json` (the mainnet pool) with exit 3;
- refuses if the resolved keystore root is not under the `testnet/` subtree, so a mainnet
  treasury keystore can never be decrypted by the harness.

### 5.3 Shadow practice run (zero-real-funds proof of the whole chain)

`shadow-env` emits exactly:

```
CHIMERA_EXECUTE_MODE=shadow            # explicit restatement of the default; never any other value
CHIMERA_CHAIN_ID=84532
CHIMERA_EOA_POOL_PATH=config/eoa_pool.sepolia.json
CHIMERA_WORKER_KEYSTORE_DIR=<home>/.chimera/keystores/testnet/workers
CHIMERA_TREASURY_KEYSTORE=<home>/.chimera/keystores/testnet/treasury.json
# CHIMERA_KEYSTORE_PASSWORD=<set manually by operator — never emitted by tooling>
```

Why this exercises the real code path with zero risk (G2): in shadow mode with populated
keystore paths, `SignerRegistry::load` proceeds past the empty-registry short-circuit,
requires the password, and decrypts every worker + treasury keystore — identical to the
live-mode decryption path — while `execute_mode: shadow` keeps all transaction paths dead.
Operator verifies with `grep "signer loaded"` exactly as the go-live runbook §2.5 does.
Expected boot output: 1× "Treasury signer loaded", 3× "Worker signer loaded", plus the
known benign `main.rs:554` legacy warning about `CHIMERA_KEYSTORE_PATH` (documented in the
new runbook; we do not modify main.rs).

`config/pacing.yaml` is never edited: `chain_id` stays 8453 and `execute_mode` stays
`shadow` in the file (G5); the env overrides above are the entire testnet delta (G4).
The Chainlink feed address remains the mainnet one, which has no code on Sepolia — the
engine then uses `eth_price_usd_fallback: 1800` by design (G12); the runbook documents the
corresponding log line so the operator recognizes it as expected.

### 5.4 Documented faucet flow (in `docs/runbook-wallet-provisioning.md`)

Sizing: sepolia pool = 3 workers × 0.02 ETH top-up + gas ⇒ treasury target 0.1 ETH —
inside a single grant from any faucet below. (10-worker mainnet-sized practice optional,
target 0.5 ETH, needs 2-3 grants or the bridge path.)

| # | Faucet | URL | Notes (verify amounts at run time) |
|---|---|---|---|
| 1 | Coinbase Developer Platform | https://portal.cdp.coinbase.com/products/faucet | Base Sepolia native; CDP account; ~0.1 ETH/day |
| 2 | Alchemy Base Sepolia | https://www.alchemy.com/faucets/base-sepolia | Alchemy account; mainnet-balance gated |
| 3 | Superchain (Optimism) | https://console.optimism.io/faucet | GitHub/onchain identity gate |
| 4 | thirdweb | https://thirdweb.com/base-sepolia-testnet | Wallet login |
| 5 | Fallback: bridge | Google Cloud / sepoliafaucet → https://bridge.base.org | L1 Sepolia ETH bridged to Base Sepolia (~minutes) |

Runbook sequence (each step names the exact command):

1. `provision` (offline) → treasury + 3 worker keystores, sepolia pool synced.
2. `faucet --wait` → operator pastes the printed treasury address into faucet #1 (or any);
   harness polls until ≥ 0.1 ETH.
3. `fund --dry-run` then `fund` → treasury fans out 0.02 ETH per worker via
   `fund_eoa.fund_wallets` (chain-guarded).
4. `status --min-balance 0.01` → all 3 workers `ok` (reuses check_balances gating).
5. `provision --verify` → Python parity check green (predicts SignerRegistry acceptance).
6. `shadow-env` → boot `target/release/chimera` with the env block; confirm
   "signer loaded" ×4 and clean shutdown.
7. Optional rotation drill: `rotate_eoa.py --config config/eoa_pool.sepolia.json --chain base --dry-run`
   (cooldown machinery exercised on the testnet pool, zero writes with `--dry-run`).

Total real funds used: **0**.

---

## 6. Function signatures

### 6.1 `scripts/provision_wallets.py`

```python
#!/usr/bin/env python3
"""provision_wallets.py — Project Chimera worker/treasury keystore provisioning.

INVARIANT (AGENTS.md #5): imports without web3/eth_account; --help and --dry-run
run stdlib-only. Private keys exist only in memory between create() and encrypt().
"""
from __future__ import annotations
import argparse, json, logging, os, re, secrets, sys, tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

try:
    from eth_account import Account          # ships with web3>=6 (requirements.txt)
except ImportError:                          # Invariant #5 fallback
    Account = None

DEFAULT_POOL_PATH   = "config/eoa_pool.json"
DEFAULT_COUNT       = 10                     # == pacing clean_eoa_pool_size default
DEFAULT_LABEL_FMT   = "chimera-eoa-{:02d}"
DEFAULT_HOME_DIR    = Path.home() / ".chimera" / "keystores"
SCRYPT_N, SCRYPT_R, SCRYPT_P = 2**18, 8, 1   # runbook §2.1 / alloy-compatible
EXIT_OK, EXIT_FAIL, EXIT_USAGE = 0, 1, 2

@dataclass
class PoolEntry:            # mirror of the six-field v2.0 wallet object (G6)
    address: str
    label: str
    chain_balances: dict[str, str]
    last_used: int
    use_count: int
    excluded: bool

@dataclass
class ProvisionPlan:
    keep:    list[str]                  # addresses with matching keystores (REAL)
    replace: list[tuple[str, str]]      # (placeholder_address, label) to regenerate
    append:  list[str]                  # new labels to add to reach --count
    foreign: list[str]                  # history-bearing entries w/o keystore (warn only)
    excluded: list[str]                 # untouched by contract

def find_repo_root(start: Path) -> Path | None
def resolve_keystore_dir(cli_value: str | None, role: str) -> Path
    # precedence: --keystore-dir > $CHIMERA_WORKER_KEYSTORE_DIR / $CHIMERA_TREASURY_KEYSTORE
    #             > pacing.yaml line-regex (stdlib) > DEFAULT_HOME_DIR subpath
def assert_outside_repo(keystore_dir: Path, repo_root: Path | None) -> None
    # raises SystemExit(EXIT_USAGE) if inside the repo — not overridable
def read_password_from_env() -> str
    # CHIMERA_KEYSTORE_PASSWORD only; SystemExit(EXIT_USAGE) if missing/empty; warn <12 chars
def load_pool_raw(pool_path: Path) -> dict[str, Any]
    # raw json.load; preserves version/_comment/unknown keys (deliberately NOT rotate_eoa.EOAPool, G7)
def inventory_keystores(keystore_dir: Path) -> dict[str, Path]
    # lowercased 0x-address -> file, from each JSON's plaintext "address" field; no password
def classify_entry(entry: dict[str, Any], have_keystore: bool) -> str
    # returns "REAL" | "PLACEHOLDER" | "FOREIGN" | "EXCLUDED"  (rules in §3.1(5))
def build_plan(pool: dict[str, Any], inventory: dict[str, Path], count: int) -> ProvisionPlan
def generate_keystore(keystore_dir: Path, label: str, password: str) -> str
    # Account.create() -> Account.encrypt(kdf="scrypt") -> atomic 0o600 write
    # "<label>--<0xAddress>.json"; returns checksummed address; key never leaves scope
def apply_plan(pool: dict[str, Any], created: list[tuple[str, str]]) -> dict[str, Any]
    # (label, address) pairs -> replace placeholders / append rows; six-field shape only
def save_pool_atomic(pool: dict[str, Any], pool_path: Path) -> None
    # same-dir NamedTemporaryFile + os.replace; indent=2 to match existing formatting
def provision_treasury(keystore_dir: Path, password: str) -> str
def verify_parity(pool_path: Path, keystore_dir: Path, password: str) -> int
    # python mirror of SignerRegistry::validate_against_eoa_pool (G3):
    # skip excluded + zero-address; every other pool address must have a decrypting
    # keystore whose derived address matches; extra keystores -> warning, exit stays 0
def render_report(plan: ProvisionPlan, created: list[tuple[str, str]],
                  keystore_dir: Path, as_json: bool) -> str
def build_parser() -> argparse.ArgumentParser
    # --eoa-pool, --keystore-dir, --count, --label-prefix, --role {worker,treasury},
    # --dry-run, --verify, --json, --log-level  (no password flag exists, by design)
def main() -> int
```

### 6.2 `scripts/testnet_harness.py`

```python
#!/usr/bin/env python3
"""testnet_harness.py — Base Sepolia practice loop (zero real funds).

INVARIANT (AGENTS.md #5): imports without web3; `plan`, `provision`, `shadow-env`
and every --help run offline. All online subcommands are chain-id guarded to the
configured testnet (84532) with no override.
"""
from __future__ import annotations
import argparse, json, logging, sys, time
from pathlib import Path
from typing import Any

try:
    from web3 import Web3
except ImportError:
    Web3 = None
try:
    from eth_account import Account
except ImportError:
    Account = None

_SCRIPT_DIR = Path(__file__).resolve().parent      # G8 delegator pattern
sys.path.insert(0, str(_SCRIPT_DIR))
import provision_wallets                            # noqa: E402
import fund_eoa                                     # noqa: E402  (reuse, never fork)
import check_balances                               # noqa: E402

DEFAULT_TESTNET_CONFIG = "config/testnet.base-sepolia.json"
DEFAULT_TESTNET_POOL   = "config/eoa_pool.sepolia.json"
TESTNET_KEYSTORE_ROOT  = Path.home() / ".chimera" / "keystores" / "testnet"
EXIT_OK, EXIT_FAIL, EXIT_USAGE, EXIT_CHAIN_GUARD = 0, 1, 2, 3

def load_testnet_config(path: Path) -> dict[str, Any]        # stdlib json + shape check
def make_guarded_web3(rpc: str, expected_chain_id: int) -> Any
    # connect, then SystemExit(EXIT_CHAIN_GUARD) unless eth_chainId == expected (§5.2)
def assert_testnet_pool(pool_path: Path) -> None
    # SystemExit(EXIT_CHAIN_GUARD) if pool_path resolves to config/eoa_pool.json
def decrypt_treasury_in_memory(keystore_path: Path, password: str) -> str
    # eth_account.Account.decrypt -> hex key string held in memory only; caller must not log
def cmd_plan(args) -> int
def cmd_provision(args) -> int      # provision_wallets funcs; treasury + N workers; sepolia pool
def cmd_faucet(args) -> int         # print address + faucet table; --wait polls balance
def cmd_fund(args) -> int           # guards, decrypt in memory, fund_eoa.fund_wallets(...)
def cmd_status(args) -> int         # check_balances.collect_balances + print_table / --json
def cmd_shadow_env(args) -> int     # emit §5.3 env block (posix + powershell); optional --emit-dotenv
def build_parser() -> argparse.ArgumentParser
def main() -> int
```

### 6.3 `config/testnet.base-sepolia.json` (new config schema, documented inline)

```json
{
  "_comment": "Project Chimera testnet practice constants. Base Sepolia only. Addresses sourced from docs/runbook-testnet-deploy.md §3.1/App.A — operator must re-verify against Aave docs before use. Public data only; no secrets belong in this file.",
  "chain": "base-sepolia",
  "chain_id": 84532,
  "rpc_url": "https://sepolia.base.org",
  "explorer": "https://sepolia.basescan.org",
  "faucets": [
    {"name": "Coinbase Developer Platform", "url": "https://portal.cdp.coinbase.com/products/faucet"},
    {"name": "Alchemy Base Sepolia", "url": "https://www.alchemy.com/faucets/base-sepolia"},
    {"name": "Superchain Faucet", "url": "https://console.optimism.io/faucet"},
    {"name": "thirdweb", "url": "https://thirdweb.com/base-sepolia-testnet"}
  ],
  "aave": {
    "pool": "0x07eA79F68B2B3df56440f754B6aA0E498b9B75Fb",
    "pool_data_provider": "0x86Fc51D0f8A8cA1e27Ee2b9b47368d0A48EB7b9C",
    "oracle": "0x4B679FE601b16583Fc6FF04f2F008dC1eFfa3B1E"
  },
  "tokens": {
    "weth": "0x4200000000000000000000000000000000000006",
    "usdc": "0x036CbD53842c5426634e7929541eC2318f3dCF7e"
  },
  "funding": {
    "treasury_target_eth": 0.1,
    "worker_topup_eth": 0.02,
    "min_worker_balance_eth": 0.01
  },
  "workers": 3
}
```

`config/eoa_pool.sepolia.json`: identical shape to `config/eoa_pool.json` (G6),
`version: "2.0"`, `_comment` noting it is the testnet practice pool, 3 placeholder
wallets labeled `chimera-sepolia-01..03` with `0x1111…/0x2222…/0x3333…` placeholder
addresses, all-zero metadata.

---

## 7. Test plan

Runner: `pytest` (already in requirements.txt; introduces `tests/` python suite —
none exists today). All tests offline; network is never required. web3/eth_account-dependent
tests carry `@pytest.mark.skipif(Account is None, ...)` so the suite passes on a
web3-less machine (Invariant #5 both ways).

### 7.1 `tests/test_provision_wallets.py`

| ID | Test | Proves |
|---|---|---|
| T-1 | `test_help_works_without_web3` — subprocess with an import-blocker shim (`PYTHONPATH` stub raising `ImportError` for `web3`/`eth_account`) runs `provision_wallets.py --help` → exit 0 | Invariant #5, "--help always works" |
| T-2 | `test_imports_without_eth_account` — import module under blocker; `Account is None`; non-dry-run `main()` exits 2 with actionable message | graceful fallback |
| T-3 | `test_generation_requires_password` — env unset → exit 2, no files created | custody gate |
| **T-4** | **`test_dry_run_writes_nothing_and_leaks_nothing`** (the required dry-run proof): (a) tmp workspace with a copy of the template pool; (b) record sha256 of pool + full recursive dir listing; (c) monkeypatch `Account.create`/`Account.encrypt` with sentinels that `raise AssertionError("key material touched in dry-run")`; (d) run `main(["--dry-run", "--count", "3", ...])` capturing stdout/stderr/log records; (e) assert exit 0, sentinels never fired, keystore dir was never created, pool hash unchanged, dir listing unchanged, and combined output has **zero** matches of `(?i)(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])` (64-hex = candidate secp256k1 key) — only 40-hex addresses may appear | dry-run provably never generates, writes, or prints key material |
| T-5 | `test_real_run_no_plaintext_on_disk` (skipif no eth_account): provision 3 workers into tmp dir with test password; then, test-side, decrypt each keystore to recover the actual private keys; byte-scan **every file** under the tmp tree (keystores, pool, temp litter) and the captured logs for each key hex (with/without `0x`, both cases) → zero hits; assert keystore JSON `version == 3`, `crypto.cipher == "aes-128-ctr"`, `crypto.kdf == "scrypt"`, POSIX mode 0600 (skip on Windows); assert derived address == pool address | encrypted-at-rest, correct format, address-only pool |
| T-6 | `test_idempotent_second_run` — run twice; second run: exit 0, plan reports 0 create, pool bytes identical, keystore filename+hash set identical | idempotency contract §3.3 |
| T-7 | `test_preserves_schema_and_rotation_metadata` — pool containing: one REAL wallet with `use_count=7,last_used=1750000000`, one excluded, one FOREIGN (history, no keystore), placeholders; after run: REAL/excluded/FOREIGN entries byte-identical, `version=="2.0"` and `_comment` intact, every wallet has exactly the six G6 fields, FOREIGN triggered a warning not a replacement | schema + rotation reuse, no parallel system |
| T-8 | `test_refuses_keystore_dir_inside_repo` — target under repo root → exit 2, nothing written | keys can never enter the repo |
| T-9 | `test_verify_parity_mirrors_signer_registry` (skipif no eth_account): green case exits 0; delete one worker keystore → exit 1 naming that address; excluded entry with no keystore → still 0; `0x000...0` address → skipped — the same four behaviors as `validate_against_eoa_pool` (mod.rs:217-235) incl. its tests `test_validate_eoa_pool_missing_signer` / `..._excluded_entry_skipped` | "SignerRegistry load validating cleanly" predicted from Python |
| T-10 | `test_treasury_role_never_touches_pool` — `--role treasury` with pool sha256 before/after identical; single keystore file created | role separation |

### 7.2 `tests/test_testnet_harness.py`

| ID | Test | Proves |
|---|---|---|
| T-11 | `test_chain_guard_blocks_wrong_chain` — fake w3 returning `chain_id=8453` → `fund`/`status` exit 3 before any account/balance call (spy asserts call order) | mainnet is unreachable by construction |
| T-12 | `test_no_live_flip_path_exists` — static source scan of **both new scripts**: `execute_mode`/`CHIMERA_EXECUTE_MODE` is never assigned/exported with any value other than the literal `shadow`; `toggle_shadow` is never imported or invoked; string `--set-live` absent | shadow default untouched; no live path added |
| T-13 | `test_plan_and_shadow_env_offline` — under the web3 import blocker, `plan` and `shadow-env` exit 0; `shadow-env` output contains `CHIMERA_CHAIN_ID=84532`, `CHIMERA_EXECUTE_MODE=shadow`, the sepolia pool path, and **no** password value | offline usability + env-block correctness |
| T-14 | `test_fund_refuses_mainnet_pool` — `--eoa-pool config/eoa_pool.json` → exit 3 | pool cross-contamination guard |
| T-15 | `test_fund_delegates_to_fund_eoa` — monkeypatch `fund_eoa.fund_wallets` sentinel; assert harness calls it (correct kwargs, in-memory key object, `dry_run` passthrough) and never reimplements a send loop | reuse-not-fork of funding mechanics |

### 7.3 Regression + validation gate (unchanged code must stay green)

- `cargo test -p chimera-core` — must pass untouched (no Rust edits); the pre-existing
  `signer_registry` and `config` tests are the Rust-side contract this plan codes against.
- `forge test --root contracts/` — untouched, must stay green (gate requirement).
- `python -m compileall scripts ai-audit/scripts` — now also compiles the two new scripts.
- `slither contracts --config-file slither.config.json` — unaffected.
- `pytest tests/` — new suite, all green.

### 7.4 Manual acceptance (operator, Base Sepolia, zero real funds)

Executes §5.4 steps 1-7 end-to-end. Evidence to capture: harness transcripts,
`status --json` output, and the shadow-boot log excerpt showing
`Treasury signer loaded` ×1 + `Worker signer loaded` ×3.

### 7.5 Acceptance checklist (definition of done)

- [ ] T-1 … T-15 green; `pytest tests/` green on machines with and without web3 installed
- [ ] T-4 demonstrates: dry-run generated nothing, wrote nothing, printed no 64-hex string
- [ ] T-5 demonstrates: no plaintext key byte-sequence exists in any written file or log
- [ ] `git grep -iE '(?:0x)?[0-9a-f]{64}'` over the diff returns no key-like material; the
      only new hex are documented contract addresses (40-hex)
- [ ] `config/eoa_pool.json` semantics preserved: schema v2.0, `_comment`, six fields,
      rotation metadata untouched for REAL/FOREIGN/EXCLUDED entries
- [ ] `provision_wallets.py --verify` exit 0 ⇒ live-mode `SignerRegistry::load` pool
      validation would pass (parity proven by T-9 against mod.rs:217-235 behaviors)
- [ ] Shadow boot on Base Sepolia decrypts all keystores with zero transactions sent
      (chain explorer shows no outgoing txs from workers other than harness testnet funding)
- [ ] `config/pacing.yaml` byte-identical to pre-plan state (Invariant #1); `core/`,
      `contracts/`, `toggle_shadow.py`, `emergency_pause.py`, `rotate_eoa.py`,
      `fund_eoa.py`, `check_balances.py` all byte-identical
- [ ] No new flag, env var, or code path sets any mode to `live` (T-12)
- [ ] Password never appears in argv, files, or logs; no key-export capability exists
- [ ] Full validation gate (AGENTS.md) green: cargo test, forge test, compileall, slither

---

## 8. Rollout sequence (after approval)

1. `.gitignore` hardening + `config/testnet.base-sepolia.json` + `config/eoa_pool.sepolia.json`.
2. `scripts/provision_wallets.py` + `tests/test_provision_wallets.py` (T-1…T-10 green).
3. `scripts/testnet_harness.py` + `tests/test_testnet_harness.py` (T-11…T-15 green).
4. `docs/runbook-wallet-provisioning.md` (usage + faucet flow + shadow drill).
5. Full validation gate + manual acceptance §7.4 on Base Sepolia.

## 9. Open questions for the operator (answer before implementation)

1. **Testnet pool size**: 3 workers proposed (single faucet grant). Prefer a full
   10-worker dress rehearsal instead (needs ~0.5 ETH via bridge/multiple grants)?
2. **Default keystore root**: `~/.chimera/keystores/` (workers/, testnet/, treasury.json)
   acceptable, or do you have a mandated secure mount (e.g. encrypted volume path)?
3. **Doc placement**: standalone `docs/runbook-wallet-provisioning.md` (proposed), or
   fold into `docs/runbook-keystore-multisig-go-live.md` §2 as a tooling subsection?
4. **KDF work factor**: scrypt n=2^18 matches the runbook but costs ~1s + ~256 MiB per
   keystore decryption at startup (×11 files). Acceptable for boot latency, or prefer
   n=2^17 (documented deviation)?
