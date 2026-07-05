# Project Chimera — Funding & Testing Runbook (code-grounded)

> **Purpose:** A practical, click-by-click path to *test* the engine now and to
> *fund* it later, in the correct order. Every step below is grounded in the
> actual source (file:line), verified by a recursive multi-agent code analysis —
> not the marketing framing. Where this runbook and
> [`docs/first-run-onboarding.md`](first-run-onboarding.md) disagree, **this file
> is the corrected version** (see [Corrections](#corrections-to-stale-docs)).

---

## TL;DR — read this before touching a wallet

**Do not make a deposit yet. There is nothing safe to fund today.**

- `config/eoa_pool.json` ships **placeholder addresses** — `0x1111…1111` through
  `0xaaaa…aaaa`, balances hardcoded `"0"` (`config/eoa_pool.json:6-105`, self-labeled
  "Template only" at `:3`).
- `scripts/fund_eoa.py` **signs and broadcasts real ETH transfers** to whatever
  addresses it finds in that file (`scripts/fund_eoa.py:175-181`,
  `w3.eth.send_raw_transaction`). Running it today sends `0.02 ETH × 10` to burn
  addresses — **unrecoverable**.
- `config/pacing.yaml` has `executor_address`, `treasury_address`,
  `treasury_keystore`, `worker_keystore_dir` all **empty** (`:39-42`). There is no
  deployed contract and no "account" to deposit into.
- `execute_mode: shadow` (`config/pacing.yaml:24`) and `core/state/mode.json` does
  not exist, so the **mandatory 7-day soak clock has not started**. `toggle_shadow.py
  --set-live` will refuse until 7 days after your first shadow run
  (`scripts/toggle_shadow.py:138-176`; enforced in Rust at
  `core/src/config.rs:219-246`, called at boot `core/src/main.rs:176`).

The engine is **not a harmless toy** — the live path is genuinely wired (it will
sign and broadcast real liquidations once the gates are satisfied). So the correct
posture is: **do every free step first, and treat the money steps as the last mile.**

---

## What "funding the account" actually means here

There is **no custodial account and no deposit endpoint.** "Funding" is you, from
your own treasury wallet, sending native ETH on an L2 (Base is the default,
`chain_id 8453` — `config/pacing.yaml:27`) to addresses you control. Three distinct,
non-interchangeable money locations:

| Location | Role | Amount | Code |
|---|---|---|---|
| **Worker EOAs** | Hot signing wallets; hold **gas only** | `~0.02 ETH` each (floor `0.01`) | `scripts/fund_eoa.py:97-98,151-153` |
| **Treasury** | Your own wallet: funds *from* it, sweeps *back* to it | Your capital | `fund_eoa.py:123,175`; `sweep_profits.py:343` |
| **Executor contract** | Where the flash-loan / liquidation economics run; profit lands here | Zero standing balance (atomic) | `contracts/src/Executor.yul:64-175` |

**Going live is not a config toggle.** Even with `execute_mode: live`, boot is
refused unless **both**: (a) a real 7-day shadow soak is satisfied
(`core/src/config.rs:219-246`), and (b) treasury keystore + worker keystore dir +
`CHIMERA_KEYSTORE_PASSWORD` + ≥1 valid worker keystore + an EOA pool whose addresses
map to loaded signers are all present (`core/src/signer_registry/mod.rs:80-160`).
Shadow (the default) forces the submitter into dry-run and **never broadcasts**
(`core/src/main.rs:328`).

---

## Track A — Begin testing TODAY (free, zero money at risk)

Every step here is safe: no keys, no deposits, no chain writes. Doing this list is
the honest way to "begin to test," and step A7 **starts the 7-day soak clock at no
cost** — which live mode will require anyway.

```bash
cd /home/user/project-chimera

# A0. Fetch the forge-std submodule (empty today — REQUIRED for forge tests).
#     contracts/lib/forge-std currently has 0 files (.gitmodules:1-3).
git submodule update --init --recursive

# A1. Toolchains. Rust stable is present (1.94.1). Foundry is NOT — install it:
curl -L https://foundry.paradigm.xyz | bash && foundryup
#   (Python: only needed for LIVE snapshots + rich tooling; shadow bring-up is stdlib-only)

# A2. Build the shadow binary  ->  target/release/chimera   (core/Cargo.toml:45-47)
cargo build --release --locked

# A3. Rust test suite (169 tests incl. shadow_e2e_test.rs, config_sync_test.rs)
cargo test -p chimera-core

# A4. Contract tests (ownership / withdrawal / flash-loan). Needs A0 + A1.
./run_forge_tests.sh          # == forge test --root contracts/ -vvv

# A5. Generate a MOCK snapshot into the binary's default load path (no web3 needed)
python3 scripts/snapshot_generator.py --chain base --mock --output config/snapshot.json
#   binary auto-loads config/snapshot.json  (core/src/main.rs:205-207)

# A6. Shadow preflight — asserts execute_mode==shadow, fails loudly otherwise
python3 scripts/dry_run.py

# A7. Start in SHADOW. Dry-run submitter, never broadcasts (config/pacing.yaml:24, main.rs:328).
#     This stamps shadow_since=now into a fresh core/state/mode.json -> starts the 7-day clock.
export BASE_RPC_URL="https://mainnet.base.org"     # resolution: BASE_RPC_URL -> RPC_URL -> localhost:8545
./target/release/chimera
#   (Arbitrum: export ARB_RPC_URL=... ; ./target/release/chimera --chain-id 42161)
```

### A8. The three startup health checks

The metrics port is **`9100 + (chain_id % 1000)`** — for Base that's **`9553`**, not
the 9100 default (`core/src/main.rs:150-151`). Pass the real port:

```bash
python3 scripts/health_check.py --metrics-port 9553 --rpc https://mainnet.base.org
```

Confirm, in the running node:

1. **Orchestrator started** — stdout line `Orchestrator starting chain_id=8453`.
2. **Metrics alive** — `curl http://localhost:9553` returns Prometheus text
   (`chimera_...`).
3. **Breaker not tripped** — metrics show `chimera_breaker_state 0`.

### A9. Rehearse the kill switch (still free)

```bash
# Trip it — orchestrator polls every 3s and trips the breaker (orchestrator.rs:38,256-269)
python3 scripts/emergency_pause.py --state-file core/state/emergency.flag --reason "drill"
# Clear the flag
python3 scripts/emergency_pause.py --state-file core/state/emergency.flag --resume
```

> **Important two-step reality:** clearing the flag does **not** clear a *tripped
> breaker*. Resuming trading additionally requires the token-gated `clear_breaker`
> path with `CHIMERA_OPERATOR_TOKEN` (`core/src/pacing_engine.rs:529-552`). Use the
> **same** flag path the binary watches — `core/state/emergency.flag` or
> `$CHIMERA_EMERGENCY_FLAG` — or you'll pause nothing.

### A10. Soak

Keep the shadow process running/restarting for **≥7 real days**, watching metrics on
`:9553`. Verify shadow decisions look sane and no unexpected breaker trips. The clock
only advances in shadow; it does not fast-forward.

---

## Track B — The funding path (money at risk; only after a real soak)

Do **not** start Track B until Track A has run for a genuine 7-day soak. In order:

### B1. Deploy contracts — testnet first (Base Sepolia)

The Yul `Executor` must exist on-chain before any live liquidation can succeed
(Aave's flash-loan callback reverts otherwise — `contracts/src/Executor.yul:64-175`).
Ownership is **multisig-enforced**: `Deploy.s.sol` reverts unless the owner is a
contract (`contracts/script/Deploy.s.sol:57-61`).

```bash
cd contracts
forge build                                  # compiles Yul -> out/Executor.yul/Executor.json
export CHIMERA_MULTISIG=0x<multisig_contract> # MUST be a contract or the script reverts
export DEPLOYER_PRIVATE_KEY=0x<funded_testnet_key>
forge script script/Deploy.s.sol \
  --rpc-url "$BASE_SEPOLIA_RPC" --broadcast \
  --private-key "$DEPLOYER_PRIVATE_KEY" --gas-estimate-multiplier 120 -vvvv
```

Post-deploy, **from the multisig** (not the deployer):
`Executor.setPool(aaveV3Pool)` and `FundDistributor.acceptOwnership()`
(`Deploy.s.sol:112-127`).

### B2. Create real keystores and wire config

- Create encrypted keystores for the treasury and each worker; set
  `CHIMERA_KEYSTORE_PASSWORD`.
- In `config/pacing.yaml`, set `executor_address`, `treasury_address`,
  `treasury_keystore`, `worker_keystore_dir` (all empty today).
- Replace **all placeholder addresses** in `config/eoa_pool.json` with your real
  worker EOA public addresses.

> **Never commit private keys.** `.gitignore` already excludes `*.key` and
> `private_keys/`. `config/eoa_pool.json` holds **public addresses only**.

### B3. Fund worker EOAs with gas (this is the actual "deposit")

Always dry-run first. Worker EOAs need gas only (`~0.02 ETH` each), never trading
capital.

```bash
pip install -r requirements.txt   # web3 is REQUIRED to send; absent today (fund_eoa.py:34-37,103-108)

# 1) offline preview — no web3, no RPC, no key, sends nothing
python scripts/fund_eoa.py --treasury-key x --rpc x --dry-run-offline

# 2) live-balance dry-run — needs web3 + RPC + real key; computes but sends nothing
python scripts/fund_eoa.py --treasury-key "$TREASURY_KEY" --rpc https://mainnet.base.org \
  --min-balance 0.01 --fund-amount 0.02 --dry-run

# 3) REAL funding — tops up every non-excluded wallet below the floor, waits for receipts
python scripts/fund_eoa.py --treasury-key "$TREASURY_KEY" --rpc https://mainnet.base.org \
  --min-balance 0.01 --fund-amount 0.02 --wait

# confirm balances
python scripts/check_balances.py --rpc https://mainnet.base.org --chain base --min-balance 0.01
```

> **Key handling caveat:** `fund_eoa.py`/`sweep_profits.py` accept keys **only as
> plaintext CLI args** — there is no keystore/env loader in these scripts despite the
> docstrings (`fund_eoa.py:224`; `sweep_profits.py --keystore-dir` is an explicit
> no-op at `:398-399`). Expand `$TREASURY_KEY` from your shell env; never paste a
> literal key into a command that lands in shell history.

### B4. Transition to live (gates still enforce)

```bash
python scripts/toggle_shadow.py --show       # confirm soak age / _soak_satisfied
python scripts/toggle_shadow.py --set-live    # refuses unless shadow_since >= 7 days
# then set execute_mode: live in config/pacing.yaml (a separate, deliberate edit)
```

Profit recovery back to the treasury is `sweep_profits.py` (fully functional; signs
and broadcasts by default — `scripts/sweep_profits.py:150-152,216-225`), keys via
`--keys-file`/`--worker-key`, destination `--treasury`.

---

## Hard blockers before any deposit

| # | Blocker | Evidence | Fix |
|---|---|---|---|
| 1 | web3.py not installed | `fund_eoa.py:34-37,103-108`; `requirements.txt:2` | `pip install -r requirements.txt` |
| 2 | Worker pool is placeholders | `config/eoa_pool.json:6-105` | Replace with real worker addresses |
| 3 | Executor not deployed | `pacing.yaml:39`; no `contracts/out/`, no broadcast records | Deploy (B1) |
| 4 | **Worker-as-Executor mismatch** | `orchestrator.rs:661-674`; `strategy/assembler.rs:66-83` | Confirm Executor bytecode lives at the worker/receiver address (CREATE-at-address or EIP-7702), else live txs **revert** |
| 5 | Foundry absent + forge-std empty | `which forge` → none; `contracts/lib/forge-std` = 0 files | A0 + A1 |
| 6 | No keystores; plaintext-key-only funding | `signer_registry/mod.rs:80-97`; `fund_eoa.py:224` | Create keystores; handle keys via env |
| 7 | Soak clock not started | `core/state/` absent; `config.rs:237-242` | Run shadow ≥7 days (A7) |
| 8 | Multisig + testnet pool addrs undecided | `CHIMERA_MULTISIG` env only; `pools.toml` mainnet-only | Choose before deploy |
| 9 | Live snapshot generator broken | `snapshot_generator.py:30-34` imports removed `geth_poa_middleware` | Fix before live snapshots (`--mock` unaffected) |

---

## Corrections to stale docs

The recursive analysis found `docs/first-run-onboarding.md` overstates immaturity in
two places:

- **§9 "Live execution path — Not fully implemented":** *Incorrect.* Live execution
  is fully wired. `Orchestrator::execute_live` builds a real EIP-1559
  `flashLoanSimple`, signs it with a keystore-decrypted `PrivateKeySigner`, and
  broadcasts via `send_raw_transaction` (`core/src/orchestrator.rs:533-535,681-707`).
  It is *gated*, not *absent*.
- **§10 "sweep_profits.py … is currently a skeleton — prints a ready message":**
  *Incorrect.* Its `__main__` dispatches to `run_sweep`, which signs and broadcasts
  real native + ERC20 sweeps by default (`scripts/sweep_profits.py:426-434,150-152,
  216-225`). Print-only behavior is opt-in behind `--dry-run` / `--dry-run-offline`.

The safety-critical claims in that doc (shadow-first default, 7-day soak, keep worker
balances small, never commit keys) are all **confirmed correct**.
