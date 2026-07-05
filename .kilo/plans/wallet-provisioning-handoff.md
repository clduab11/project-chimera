# Handoff: Wallet Provisioning Implementation + Review Findings

**Date**: 2026-07-05
**Status**: Implementation COMPLETE and all tests green. Post-implementation code review found
1 CRITICAL, 5 WARNING, 6 SUGGESTION items — **none fixed yet**. Everything is uncommitted.
**Source of truth for the original work**: `.kilo/plans/wallet-provisioning.md` (plan, §7.5 acceptance checklist).
**This doc**: what was done, what was verified, and the exact remaining fix list for the next (cheaper) agent.

---

## 1. What this run accomplished (all six todos complete)

| # | Todo | Status | Notes |
|---|------|--------|-------|
| 1 | Install pytest+web3 into `.venv` | DONE | pytest 9.1.1, web3 7.16.0, eth_account 0.13.7 installed (requirements.txt listed them but venv lacked them) |
| 2 | Step 1: `.gitignore` append + testnet configs | DONE | `.gitignore` +1 line (`keystores/`); created `config/testnet.base-sepolia.json`, `config/eoa_pool.sepolia.json` |
| 3 | Step 2: `scripts/provision_wallets.py` + `tests/test_provision_wallets.py` | DONE | T-1…T-10 green. Scrypt kwarg: `Account.encrypt(key, pw, kdf="scrypt", iterations=SCRYPT_N)` (iterations == scrypt n; r=8/p=1 are eth_keyfile defaults) |
| 4 | Step 3: `scripts/testnet_harness.py` + `tests/test_testnet_harness.py` | DONE | T-11…T-15 green |
| 5 | Step 4: `docs/runbook-wallet-provisioning.md` | DONE | Faucet flow (§5.4) + shadow drill (§5.3) documented |
| 6 | Verification | DONE | See §2 |

### Files created/changed by this run
- Modified: `.gitignore` (one-line append `keystores/` — the only edit to an existing file)
- New: `config/testnet.base-sepolia.json`, `config/eoa_pool.sepolia.json`,
  `scripts/provision_wallets.py`, `scripts/testnet_harness.py`,
  `tests/test_provision_wallets.py`, `tests/test_testnet_harness.py`,
  `docs/runbook-wallet-provisioning.md`

### Pre-existing working-tree items NOT from this run (do not blame/revert without checking)
- `D PHASE6_VALIDATION_GATE.md` (deleted before this session; assessed non-critical — content duplicated in AGENTS.md/operator docs)
- Untracked `rustup-init.exe` (installer binary at repo root — see W-6 below)
- Untracked `.kilo/plans/wallet-provisioning.md` (the plan itself)

## 2. Verification results (all green as of this run)

- `.\.venv\Scripts\python.exe -m pytest tests\` → **15 passed** (T-1…T-15), offline
- `.\.venv\Scripts\python.exe -m compileall scripts ai-audit\scripts` → clean, exit 0
- `cargo test -p chimera-core` → **166 passed, 0 failed** (3 pre-existing Windows fsync ignores), zero Rust diffs
- `git diff --stat` → only `.gitignore` +1 (plus the pre-existing PHASE6 deletion); `config/pacing.yaml`,
  `config/eoa_pool.json`, `core/**`, `contracts/**`, and all "do not modify" scripts byte-identical
- Hex scan of all new/changed files for `(?i)(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])` → zero matches (no key-like material)
- NOT run (toolchain not verified on this machine, and `contracts/` has zero diffs): `forge test`, `slither`
- NOT done (manual operator step, §7.4): live Base Sepolia shadow boot / faucet drill

### §7.5 checklist status
All automated items PASS. Manual Base Sepolia acceptance (§7.4) pending operator. forge/slither pending a machine with those tools (no contract diffs, so expected green).

## 3. Review findings — the fix list

Review recommendation was **NEEDS CHANGES**. Fix in this order. After ANY fix, re-run:
`.\.venv\Scripts\python.exe -m pytest tests\` (must stay 15/15 green; update tests where the fix
intentionally changes behavior, e.g. C-1 needs a new test asserting the hardcoded allowlist).

### CRITICAL

**C-1. Chain guard not pinned to 84532** — `scripts/testnet_harness.py:138` (and `load_testnet_config` ~:112)
`make_guarded_web3` compares the RPC chain id to `config["chain_id"]` from the operator-editable
`--config` file. A config with `"chain_id": 8453` + mainnet RPC passes; `fund` would then move real funds.
This violates the plan's hard constraint ("hard chain-id guard = 84532 with no override").
FIX: add module constant `ALLOWED_TESTNET_CHAIN_IDS = {84532}`; reject in `load_testnet_config`
(config chain_id not in allowlist → exit 3) AND in `make_guarded_web3` (RPC-reported chain not in
allowlist → exit 3), independent of config contents. Add a test: config with chain_id 8453 → exit 3
even when the fake RPC also reports 8453.

### WARNING

**W-1. Mainnet-pool guard is a filename denylist** — `scripts/testnet_harness.py:159`
A renamed copy of the mainnet pool (`eoa_pool.prod.json`) passes `assert_testnet_pool`.
FIX: positive marker — add `"chain": "base-sepolia"` to `config/eoa_pool.sepolia.json` and require
that key in `assert_testnet_pool` (missing/mismatched → exit 3). Keep the existing filename refusal
as belt-and-suspenders. Update T-14 to also cover the marker path.

**W-2. `apply_plan` label-collision overwrites protected rows** — `scripts/provision_wallets.py:394-400`
Matches created wallets by label over ALL pool rows; a REAL/FOREIGN/EXCLUDED row sharing a label, or
an appended label colliding with an existing one (`next_index = len(wallets)+1` doesn't check
uniqueness), gets its address silently overwritten.
FIX: pass `plan.replace` identities through — replace placeholder rows by matching their
`placeholder_address` (not label); append unconditionally for planned appends; in `build_plan`,
skip generated append labels already present in the pool. Add a regression test: pool with a FOREIGN
row whose label equals a placeholder label → FOREIGN row byte-identical after run.

**W-3. Treasury path env override unguarded in `faucet`/`shadow-env`** — `scripts/testnet_harness.py:188-192`
`_treasury_keystore_path()` prefers `CHIMERA_TREASURY_KEYSTORE` (legitimately set to the MAINNET
treasury in the go-live workflow). `faucet` would print/poll the mainnet treasury as the faucet
target; `shadow-env` emits a non-testnet path. Only `fund` is guarded.
FIX: apply `assert_testnet_keystore()` to the resolved treasury path in `cmd_faucet` and
`cmd_shadow_env` too (or ignore the env var inside the harness entirely and always use
`TESTNET_KEYSTORE_ROOT/treasury.json`).

**W-4. Runbook step 5 verify command wrong** — `docs/runbook-wallet-provisioning.md:228` (7-step sequence, step 5)
`provision_wallets.py --verify --eoa-pool config/eoa_pool.sepolia.json` without `--keystore-dir`
resolves to the MAINNET worker dir (`~/.chimera/keystores/workers`) and exits 1 after a correct run.
FIX: docs-only — add `--keystore-dir "$HOME/.chimera/keystores/testnet/workers"` (and the PowerShell
equivalent) to the step-5 command.

**W-5 (was part of W-3 analysis). Same env-var inconsistency also means `provision` vs `faucet` can
resolve two different treasuries in one workflow** — covered by the W-3 fix; verify with a test that
sets `CHIMERA_TREASURY_KEYSTORE` to a non-testnet path and asserts `faucet` exits 3.

**W-6. `rustup-init.exe` not gitignored** — `.gitignore` / repo root
The whole change set is untracked files; `git add .` would commit a 10+ MB unauditable binary.
FIX: append `rustup-init.exe` to `.gitignore` (or delete the binary — it is a one-time installer).

### SUGGESTION (fix if budget allows, in this order)

**S-1. `faucet --wait` unbounded loop** — `scripts/testnet_harness.py:414`
Add `--timeout` (default ~30 min); exit 1 (`EXIT_FAIL`) on expiry.

**S-2. `--verify` decrypts every keystore at scrypt n=2^18** — `scripts/provision_wallets.py:444-479`
Load pool first; decrypt only keystores whose plaintext `address` field is in the non-excluded pool
set; report extras from the plaintext field without decryption.

**S-3. Forked env constants/password reader** — `scripts/testnet_harness.py:78-79, 199-208`
Replace with `PASSWORD_ENV = provision_wallets.PASSWORD_ENV`, `TREASURY_ENV =
provision_wallets.TREASURY_ENV`, and call `provision_wallets.read_password_from_env()` (the harness
copy already dropped the <12-char warning).

**S-4. Funding literals duplicated as silent fallbacks** — `scripts/testnet_harness.py:390, 439-440, 489`
Validate `funding.treasury_target_eth`, `funding.worker_topup_eth`,
`funding.min_worker_balance_eth` as REQUIRED keys in `load_testnet_config`; remove the
`funding.get(key, <literal>)` numeric fallbacks.

**S-5. `SCRYPT_R`/`SCRYPT_P` unenforced + misleading comment** — `scripts/provision_wallets.py:60-62, 368-370`
Only `SCRYPT_N` reaches `Account.encrypt`. Reword the comment: r/p are eth_keyfile defaults asserted
post-hoc by T-5, not passed at call time. (Do NOT try to pass r/p — eth_account's API doesn't take them.)

**S-6. Zero-metadata real wallets classify as PLACEHOLDER** — `scripts/provision_wallets.py:286-288`
Plan-compliant but risky on a machine without the local keystores. Optional hardening: only replace
entries whose address matches the `0x1111...`-style repeated-nibble template pattern, and/or write a
`.bak` of the pool before `os.replace`. If behavior changes, update T-7 and document in the runbook.

## 4. Hard constraints that MUST survive any fix (from the original task)

- Private keys never in repo/plaintext-disk/logs/git; password only via `CHIMERA_KEYSTORE_PASSWORD`
  (no CLI flag, no export/decrypt-print path).
- Keystores: Web3 Secret Storage V3, scrypt n=2^18 r=8 p=1 + aes-128-ctr (alloy-decryptable). NOT EIP-2335.
- `--dry-run` generates zero key material (T-4 enforces with sentinels).
- Do NOT modify: `core/**`, `contracts/**`, `config/pacing.yaml`, `config/eoa_pool.json` (committed
  template), `rotate_eoa.py`, `rotate_wallet.py`, `fund_eoa.py`, `check_balances.py`,
  `toggle_shadow.py`, `emergency_pause.py`, `sweep_profits.py`.
- Shadow stays default; no code path assigns "live" to any mode (T-12 static scan enforces — keep it green).
- Delegate, don't fork: fund via `fund_eoa.fund_wallets`, report via `check_balances.collect_balances`.
- Pool round-trips as raw dict preserving `version: "2.0"` + `_comment`.
- Invariant #5: both scripts import + `--help` without web3/eth_account (T-1/T-2/T-13 enforce).
- All tests offline. Do not commit/push/branch — leave uncommitted for operator review.

## 5. Verification commands (run after fixes)

```powershell
.\.venv\Scripts\python.exe -m pytest tests\ -v          # 15+ passed (more if you add regression tests)
.\.venv\Scripts\python.exe -m compileall scripts ai-audit\scripts
cargo test -p chimera-core                               # must stay green, zero Rust diffs
git diff --stat                                          # only .gitignore among tracked files
```
