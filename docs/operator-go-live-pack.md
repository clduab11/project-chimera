# Project Chimera — Operator Go-Live Pack

**Version**: 1.0 · **Date**: 2026-07-20 · **Audience**: solo operator (WSL bash)
**Scope**: Base mainnet (chain_id 8453). Arbitrum notes marked where they differ.

**Decision record (2026-07-20):** the 7-day shadow-soak gate was de-listed by
explicit operator decision with in-session risk acceptance. `SHADOW_SOAK_SECONDS = 0`
(`scripts/toggle_shadow.py:65`); `validate_mode_transition()` keeps only a
future-timestamp corruption guard (`core/src/config.rs:311-332`). Committed
config remains `execute_mode: shadow` (`config/pacing.yaml:24`) and CI
`shadow-guard` still blocks any committed `execute_mode: live`
(`.github/workflows/ci.yml:138-147`). **The only live flip is
`CHIMERA_EXECUTE_MODE=live` in deployment-local env.** Nothing in code flips
mode automatically (verified: only `config.rs:394` assigns `execute_mode`).

This pack is the single sequential source. Detail runbooks:
`docs/runbook-keystore-multisig-go-live.md` (custody depth),
`docs/runbook-wallet-provisioning.md` (keystore tooling),
`docs/runbook-7day-soak.md` (optional stability rehearsal),
`docs/emergency-procedures.md` (incidents).

---

## A. Live readiness audit (verified 2026-07-20)

### A.1 MUST-HAVE — code-enforced (live startup fails closed without these)

| # | Gate | Evidence | State |
|---|---|---|---|
| 1 | `executor_address`, `treasury_address`, `treasury_keystore`, `worker_keystore_dir`, `eoa_pool_path` non-empty, non-zero; `min_worker_balance_eth > 0`; `refund_topup_eth > 0` | `core/src/config.rs:226-254` (`validate_live_fields`) | ⚠ committed fields empty by design — supply via env |
| 2 | Live paths exist (keystore file/dir, pool file) | `core/src/config.rs:256-309` (`validate_live_paths`, `require_dir`) | ⚠ env-dependent |
| 3 | `CHIMERA_KEYSTORE_PASSWORD` set | `core/src/signer_registry/mod.rs:104-108` | ⚠ env-dependent |
| 4 | Treasury keystore decrypts; decrypted address == `CHIMERA_TREASURY_ADDRESS` | `core/src/signer_registry/mod.rs:114-140` | ⚠ operator action (§B.2) |
| 5 | Treasury is not also a worker; no duplicate workers | `core/src/signer_registry/mod.rs:172-183` | ⚠ operator action |
| 6 | Every non-excluded EOA-pool address has a decrypting worker keystore | `core/src/signer_registry/mod.rs:304-317` (`Live mode: EOA pool entry … has no registered worker signer`) | ⚠ `provision_wallets.py --verify` predicts this |
| 7 | Executor has deployed bytecode on-chain | `core/src/main.rs:590-594` | ⚠ §B.3 |
| 8 | Executor `pool()` == canonical Aave Pool `0xA238Dd80C259a72e81d7e4664a9801593F98d1c5` (Base default `main.rs:511`, pinned by test `main.rs:799`, `config/pools.toml:7`) | `core/src/main.rs:602-606` | ⚠ §B.4 |
| 9 | `isWorker(worker)` == true for every active worker | `core/src/main.rs:614-618` | ⚠ §B.4 |
| 10 | Treasury ≥ `refund_topup_eth` (0.01 ETH); every worker ≥ max(`min_worker_balance_eth`, 0.002 ETH) (`MIN_GAS_BUDGET_WEI`, `core/src/executor/balance.rs:12`) | `core/src/main.rs:631-683` | ⚠ §B.5 |
| 11 | `execute_mode` ∈ {shadow, live}; transition state valid (future `shadow_since` rejected) | `core/src/config.rs:201-205`, `core/src/config.rs:311-332` | ✅ |
| 12 | Mode flip only via env; committed config shadow | `config/pacing.yaml:24`, CI guard `ci.yml:138-147` | ✅ |
| 13 | Pacing caps / ProfitGate / breaker intact (2.5× profit-gas, $2k/day, $7.5k/week, $1k/op, 3-revert halt, 300 gwei, 0.005 ETH daily loss) | `config/pacing.yaml:5-21`; risk gates `config/risk.yaml` | ✅ untouched |
| 14 | Correct Chainlink ETH/USD feed `0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70` everywhere (9 occurrences, 7 files; zero wrong-address matches) | `config/pacing.yaml:35`, `core/src/config.rs:86`, `core/tests/fixtures/pacing_canonical.yaml:20` | ✅ config_sync_test green |

### A.2 MUST-HAVE — operator judgment (not code-enforced)

| # | Item | Evidence | State |
|---|---|---|---|
| 15 | Multisig (e.g. Safe) deployed on Base; `code.length > 0`; enforced as Executor owner at deploy | `contracts/script/Deploy.s.sol:64-68,97-103` | ⚠ §B.2 |
| 16 | Multisig calls `setPool` + `setWorker(worker,true)` post-deploy (script deliberately does not) | `contracts/script/Deploy.s.sol:121-144` | ⚠ §B.4 |
| 17 | `config/eoa_pool.json` placeholder addresses replaced or excluded; never funded | `config/eoa_pool.json:3` (`_comment`) | ⚠ `provision_wallets.py` handles |
| 18 | Real (non-`--mock`) snapshot before live start | `scripts/snapshot_generator.py:863-868` (mock bypasses RPC) | ⚠ §B.6 |
| 19 | RPC keys rotated if ever pasted into logs; single provider | operator security | ⚠ §B.0 |
| 20 | Standard-RPC frontrun exposure explicitly accepted (private/protected submission NOT wired; raw EIP-1559 to standard RPC) | `README.md:62`, `docs/runbook-keystore-multisig-go-live.md:29-31` | ⚠ explicit acceptance required |
| 21 | `CHIMERA_OPERATOR_TOKEN` set (else `clear_breaker` refused) | `core/src/pacing_engine.rs:542-546` | ⚠ env |
| 22 | Keystores outside repo, `chmod 600`, repo guard (exit 2 if inside repo) | `docs/runbook-wallet-provisioning.md:77-89` | ⚠ §B.2 |

### A.3 OPTIONAL — recommended, not blocking

| # | Item | Reference |
|---|---|---|
| 23 | Shadow stability rehearsal (drills, evidence archive) | `docs/runbook-7day-soak.md` (optional since 2026-07-20) |
| 24 | Base Sepolia end-to-end rehearsal (zero real funds; chain-guarded 84532, exit 3 on wrong chain) | `docs/runbook-testnet-deploy.md`, `scripts/testnet_harness.py` |
| 25 | Monitoring stack (Grafana `:3002`, Prometheus `:9090`); fix Prometheus target `chimera-core:9553` → `host.docker.internal:9553` for host-run binary | `docker/docker-compose.yml`, `monitoring/prometheus.yml:29` |
| 26 | `status.py` dashboard (needs `rich`+PyYAML — unguarded imports, `status.py:45-50`) | `scripts/status.py` |
| 27 | Docker image build of chimera-core (currently broken on old Cargo edition2024 — build on host instead) | environment note |

---

## B. Sequential go-live runbook (WSL)

> Copy-paste safe from repo root. `<…>` = fill in. Never paste real keys into
> chat/logs. Commands marked **[LIVE]** change on-chain state or start live
> execution. Estimated RPC cost: a few dozen calls total — no 24/7 bot needed.

### B.0 RPC credit rules (read once)

- Use **one** provider (Alchemy *or* Infura) for both HTTPS and WSS.
- No continuous engine runs before go-live; every command below is one-shot.
- Rotate keys that were ever pasted into any log/transcript.
- Keep the real env file outside git (`~/chimera.env`) or as gitignored `.env.live`.

### B.1 Environment file

```bash
cp .env.live.example ~/chimera.env && chmod 600 ~/chimera.env
$EDITOR ~/chimera.env          # fill placeholders (template comments document each var)
set -a; source ~/chimera.env; set +a
test -n "$CHIMERA_KEYSTORE_PASSWORD" && test -n "$CHIMERA_OPERATOR_TOKEN" && echo "secrets present"
```

Two WSL gotchas, both observed in practice:

- Use `.venv/bin/python` for every script below (plain `python` is not
  installed on Ubuntu/WSL; `python3` also works but the venv is canonical).
- If you keep the file inside the repo as `.env.live` and edit it from a
  Windows editor, it will pick up CRLF line endings — bash then chokes on
  blank lines and silently poisons values. Fix once: `sed -i 's/\r$//' .env.live`
  (or set the editor to LF). The example template uses only quoted empty
  strings, so it is sourceable even before every value exists; a leftover
  `<...>` placeholder anywhere else is a shell syntax error — check with
  `grep -n '<' ~/chimera.env` (must return nothing) before sourcing. A parse
  error aborts sourcing at that line: every variable defined below it never
  gets set.

Keep `CHIMERA_EXECUTE_MODE=shadow` until §B.8. Verified env surface in §C.

### B.2 Multisig + keystore provisioning (offline; no RPC)

```bash
# 1. Create/confirm the multisig on Base (Safe UI) — record its address. It must
#    be a deployed contract; Deploy.s.sol enforces code.length > 0.
# 2. Provision workers + treasury keystores OUTSIDE the repo:
python scripts/provision_wallets.py                      # workers → ~/.chimera/keystores/workers, syncs config/eoa_pool.json (public addresses only)
python scripts/provision_wallets.py --role treasury      # → ~/.chimera/keystores/treasury.json
# 3. Record the treasury address printed; set CHIMERA_TREASURY_ADDRESS in ~/chimera.env.
# 4. Parity check (predicts Rust SignerRegistry acceptance; exit 0 = live validation will pass):
python scripts/provision_wallets.py --verify
```

Exit codes: `0` ok · `1` fail (parity/pool) · `2` usage (password unset / keystore dir inside repo — non-overridable guard).

### B.3 Executor deploy (one-shot broadcast)

```bash
export PATH="$HOME/.foundry/bin:$PATH"
forge build --root contracts/                            # produces out/Executor.yul/Executor.json (loaded by the script)
export CHIMERA_MULTISIG=0x<safe_address>
# Deployer key: gas-only EOA, NEVER treasury/worker/multisig keys; unset after use.
read -s DEPLOYER_PRIVATE_KEY && export DEPLOYER_PRIVATE_KEY
[LIVE] forge script contracts/script/Deploy.s.sol --rpc-url "$BASE_RPC_URL" --broadcast
unset DEPLOYER_PRIVATE_KEY
```

Script behavior (`Deploy.s.sol`): deploys Yul Executor with the multisig as
constructor-appended owner, asserts `owner() == multisig`, deploys
`FundDistributor` (two-step ownership to multisig), then prints ACTION REQUIRED
for `setPool`/`setWorker`. Record the Executor address → `CHIMERA_EXECUTOR_ADDRESS`.

```bash
cast code "$CHIMERA_EXECUTOR_ADDRESS" --rpc-url "$BASE_RPC_URL" | head -c 20   # non-empty bytecode
cast call "$CHIMERA_EXECUTOR_ADDRESS" "owner()(address)" --rpc-url "$BASE_RPC_URL"   # == multisig
```

Recommended first: full rehearsal on Base Sepolia per
`docs/runbook-testnet-deploy.md` (`python scripts/testnet_harness.py plan`) —
zero real funds, chain-guarded to 84532.

### B.4 setPool + setWorker from the multisig [LIVE]

Generate reviewable calldata (no signing locally), paste into the Safe tx builder:

```bash
cast calldata "setPool(address)" 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5
cast calldata "setWorker(address,bool)" 0x<worker_address> true    # repeat per active worker
```

Verify (must all pass — these mirror startup gates A.1 #8/#9):

```bash
cast call "$CHIMERA_EXECUTOR_ADDRESS" "pool()(address)" --rpc-url "$BASE_RPC_URL"
cast call "$CHIMERA_EXECUTOR_ADDRESS" "isWorker(address)(bool)" 0x<worker_address> --rpc-url "$BASE_RPC_URL"
```

### B.5 Gas funding only (no strategy capital — Aave flash loans supply it)

Fund: treasury ≥ 0.01 ETH (`refund_topup_eth`), each worker ≥ 0.002 ETH
(effective minimum, `core/src/main.rs:649-652`). Keep balances gas-sized; the
Rust SweepScheduler refunds/tops up automatically (`sweep_interval_secs: 300`).

```bash
cast balance 0x<treasury> --rpc-url "$BASE_RPC_URL"
python scripts/check_balances.py --chain base --min-balance 0.002 --rpc "$BASE_RPC_URL"
```

### B.6 Live market snapshot (one-shot; ~a few RPC calls)

`--output` has **no default** — pass it explicitly. Write is atomic
(tmp + rename). `--mock` is pipeline-testing only, never production state.

```bash
python scripts/snapshot_generator.py --chain base --rpc-url "$BASE_RPC_URL" --output config/snapshot.json
python -c "import json; s=json.load(open('config/snapshot.json')); print(f\"reserves={len(s['reserves'])} users={len(s['users'])} ts={s['timestamp']}\")"
```

### B.7 Preflight (shadow, never broadcasts)

```bash
python scripts/dry_run.py --rpc "$BASE_RPC_URL"          # must end: Preflight PASS (execute_mode=shadow)
python scripts/health_check.py --rpc "$BASE_RPC_URL" --metrics-port 9553
```

### B.8 Mode flip (local env only — committed config stays shadow)

```bash
python scripts/toggle_shadow.py --show                   # stamped mode.json required (stamped 2026-07-20)
python scripts/toggle_shadow.py --set-live               # writes previous_mode=live; preserves shadow_since
$EDITOR ~/chimera.env                                    # CHIMERA_EXECUTE_MODE=live
set -a; source ~/chimera.env; set +a
```

### B.9 First live start, watch, halt [LIVE]

```bash
RUST_LOG=chimera=info ./target/release/chimera
# Expected: chain_id=8453, signer loaded lines (1 treasury + N workers), no ERROR.
# Live startup re-runs every A.1 gate and fails closed on any miss.
```

Watch (second terminal; metrics port = 9100 + 8453%1000 = **9553**):

```bash
watch -n 10 'curl -s http://localhost:9553/metrics | grep -E "chimera_breaker_state|chimera_candidates|chimera_sims|chimera_profit|chimera_revert"'
python scripts/status.py --chain base      # needs rich+PyYAML
```

Halt immediately if anything is unexpected:

```bash
python scripts/emergency_pause.py --state-file core/state/emergency.flag --reason "<what you saw>"
python scripts/toggle_shadow.py --set-shadow    # and set CHIMERA_EXECUTE_MODE=shadow in ~/chimera.env
```

### B.10 Profit consolidation (multisig only)

Debt-token profit stays on Executor. `amount=0` withdraws the full token
balance to the multisig caller; `token=0` = native ETH.

```bash
cast calldata "withdraw(address,uint256)" 0x<debt_token> 0        # → Safe tx builder
```

Never use `scripts/sweep_profits.py` for Executor profit (legacy raw-key
helper, worker balances only — `runbook-keystore-multisig-go-live.md` §8).

### B.11 Rollback / abort (any time)

1. `emergency_pause.py --state-file core/state/emergency.flag --reason "<…>"` (engine trips within its flag poll)
2. `toggle_shadow.py --set-shadow` + `CHIMERA_EXECUTE_MODE=shadow` in env; stop process, preserve logs
3. Multisig: `setWorker(worker,false)` per affected worker; `setPool(0x0)` disables execution
4. Multisig `withdraw(token, amount)` recovers Executor-held assets
5. `clear_breaker` requires `CHIMERA_OPERATOR_TOKEN` — do not clear before root cause
6. Incident template: `docs/emergency-procedures.md`

### B.12 Post-go-live ops

**Daily (10 min):** `status.py --chain base`; `health_check.py --rpc "$BASE_RPC_URL" --metrics-port 9553`;
`check_balances.py --chain base --min-balance 0.002 --rpc "$BASE_RPC_URL"`;
`grep -iE "error|panic|BREAKER" logs/chimera.log`; confirm breaker 0.

**Weekly:** `python scripts/update_venues.py`; review `core/state/outcomes.jsonl`
PnL + breaker events; refresh snapshot (B.6); export outcomes for tax records
(`docs/monetization.md` §6); review inclusion rate vs standard-RPC exposure (A.2 #20).

---

## C. Env var matrix (verified against source)

| Variable | Required for live | Default / source | Evidence |
|---|---|---|---|
| `BASE_RPC_URL` | yes (Base reads it first) | falls back `RPC_URL` → `http://localhost:8545` | `core/src/main.rs:473-494` |
| `CHIMERA_WS_ENDPOINT` | recommended | else public `wss://mainnet.base.org` (405-prone), else polling | `core/src/main.rs:388-398`, `core/src/config.rs:445-447` |
| `CHIMERA_EXECUTE_MODE` | **yes — the only mode flip** | `shadow` (`config/pacing.yaml:24`) | `core/src/config.rs:393-395` |
| `CHIMERA_EXECUTOR_ADDRESS` | yes | empty in committed config | `core/src/config.rs:408-410` |
| `CHIMERA_TREASURY_ADDRESS` | yes | empty | `core/src/config.rs:411-413` |
| `CHIMERA_TREASURY_KEYSTORE` | yes | empty | `core/src/config.rs:414-416` |
| `CHIMERA_WORKER_KEYSTORE_DIR` | yes | empty | `core/src/config.rs:417-419` |
| `CHIMERA_EOA_POOL_PATH` | yes | `config/eoa_pool.json` | `core/src/config.rs:402-404` |
| `CHIMERA_KEYSTORE_PASSWORD` | yes | none | `core/src/signer_registry/mod.rs:104-108` |
| `CHIMERA_OPERATOR_TOKEN` | for `clear_breaker` only | none | `core/src/pacing_engine.rs:542-546` |
| `CHIMERA_SNAPSHOT_PATH` | no | `config/snapshot.json` | `core/src/main.rs:224-226` |
| `CHIMERA_EMERGENCY_FLAG` | no | `core/state/emergency.flag` | `core/src/orchestrator.rs:156-159` |
| `CHIMERA_METRICS_PORT` | no | 9100 base → **9553** on Base (`9100 + chain_id%1000`) | `core/src/main.rs:158-167` |
| `CHIMERA_CHAIN_ID` | no | 8453 | `core/src/config.rs:386` |
| `CHIMERA_AAVE_POOL` / `CHIMERA_AAVE_ORACLE` / `CHIMERA_POOL_DATA_PROVIDER` / `CHIMERA_ETH_ORACLE_ASSET` | no | Base address book hardcoded | `core/src/main.rs:499-545` |
| `CHIMERA_ETH_USD_FEED_ADDRESS` | no | correct Base feed committed | `core/src/config.rs:399-401` |
| `RUST_LOG` | no | `info` (`chimera=info` forced) | `core/src/main.rs:134-136` |

Arbitrum differences: `ARB_RPC_URL` read first for chain_id 42161
(`main.rs:474-478`); Aave address book differs (`main.rs:525-536`); metrics
port 9261. Base Sepolia (84532) is hard-rejected by the main binary
(`main.rs:537-539`) — testnet practice uses `scripts/testnet_harness.py`.

---

## D. DO NOT go live until…

- …A.1 #1–10 all pass (the binary enforces them — treat any startup ERROR as a blocked go-live, never bypass).
- …multisig is a deployed contract and owns the Executor (`owner()` verified).
- …EOA-pool placeholders are replaced/excluded and were never funded.
- …`config/snapshot.json` is a real RPC snapshot from B.6, not `--mock`.
- …leaked RPC keys (if any) are rotated; the env file holds no committed secrets.
- …standard-RPC frontrun exposure is explicitly accepted (private submission unwired).
- …`dry_run.py` + `health_check.py` pass with zero FAIL.
