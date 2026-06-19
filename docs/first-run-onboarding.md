# Project Chimera ΓÇö First-Run Onboarding Guide

> **Who this is for:** Someone who has just cloned this repository and wants to
> understand what Chimera does, how it makes money, how to get it running safely,
> and what to watch out for before touching real funds.
>
> **What this is not:** A shortcut to live trading. Every section below is
> grounded in the actual current code. Where something is planned but not yet
> fully wired, this guide says so explicitly.

---

## 1. What Is Chimera?

Chimera is an automated on-chain liquidation bot targeting the Aave V3 lending
protocol on Base and Arbitrum. It watches for undercollateralized loans ΓÇö users
whose collateral has fallen below Aave's minimum health threshold ΓÇö and, when it
finds one, steps in to repay part of their debt in exchange for a discounted
share of their collateral.

The profit comes from the discount Aave gives the liquidator. Chimera uses a
flash loan to execute the entire sequence atomically:

1. Borrow the repayment amount via a flash loan (no upfront capital required)
2. Call Aave's `liquidationCall` to repay the borrower's debt and receive
   discounted collateral
3. Swap that collateral back to the borrowed token via a DEX
4. Repay the flash loan plus fee
5. Keep the remainder as profit

If the math does not work out ΓÇö if profit after gas and fees is below the
configured multiplier threshold ΓÇö Chimera does nothing. It never takes a position
it cannot close in the same transaction.

---

## 2. How the Money Works

**Revenue source:** The liquidation bonus Aave pays. On most assets this is
5ΓÇô10% of the repaid debt. Chimera targets only those where the expected net
return exceeds gas cost by at least `min_profit_multiplier` (default 2.5x, set
in `config/pacing.yaml`).

**Cost sources:**
- L2 execution gas (gwei-priced, capped at `max_gas_gwei`)
- L1 data fee for posting the transaction to Ethereum
- DEX swap slippage (capped at `slippage_max_bps` in `config/risk.yaml`)

**Capital flow (simplified):**
```
Treasury wallet (cold)
        |
   fund_eoa.py
        |
Worker EOAs (hot, small balances ΓÇö enough for gas)
        |
  Chimera binary (flash-loan execution ΓÇö no capital at risk per-op)
        |
  Profit lands in worker EOA
        |
  sweep_profits.py
        |
Treasury wallet (cold)
```

**Pacing guardrails (from `config/pacing.yaml`):**

| Guard | Default | Purpose |
|---|---|---|
| `max_daily_net_usd` | $2,000 | Hard cap on net daily flow |
| `max_weekly_net_usd` | $7,500 | Hard cap on net weekly flow |
| `max_single_transfer_usd` | $1,000 | No single op exceeds this |
| `max_daily_loss_eth` | 0.005 ETH | Halt if daily gas loss hits this |
| `min_profit_multiplier` | 2.5x | Min profit-to-gas ratio |
| `auto_halt_on_reverts` | 3 | Trip breaker after 3 reverts |

These guards run **in Rust before any on-chain action**. Exceeding any one of
them trips the circuit breaker and halts the engine without operator action.

---

## 3. Shadow Mode vs Live Mode

**The current repository status is shadow mode only.**

`execute_mode: shadow` (the default in `config/pacing.yaml`) means the engine
runs the full detection and pacing logic but does not submit transactions on-chain.
It is safe to run in this mode without any funds at risk.

`execute_mode: live` is the mode where transactions are submitted. A 7-day shadow
period is required before the engine will allow a shadow-to-live transition (this
is enforced in `PacingConfig::validate_mode_transition()`). The current binary
entrypoint does not implement the full live execution path yet ΓÇö see the notes in
Section 9.

**Do not change `execute_mode` to `live` until you have:**
- Run shadow mode for at least 7 days
- Verified the metrics look healthy (Section 7)
- Confirmed EOA funding and rotation work correctly
- Read `docs/emergency-procedures.md`

---

## 4. What You Need Before You Start

### Required toolchain

| Tool | Purpose | Check with |
|---|---|---|
| Rust (stable, 1.74+) | Build the core binary | `cargo --version` |
| Docker + Docker Compose | Monitoring stack | `docker --version` |
| Python 3.10+ | Operational scripts | `python3 --version` |
| `pip install -r requirements.txt` | Python deps | (run once from repo root) |

> **Note:** `web3.py` is optional. Python scripts gracefully skip web3-dependent
> paths when it is not installed (Invariant #5 in `AGENTS.md`). Install it if you
> want to use the funding/sweep helpers.

### Accounts you will need
- A **treasury wallet** (cold) ΓÇö never touches the engine directly
- At least one **worker EOA** (hot, small balance) ΓÇö the engine's signing wallet
- A **Base or Arbitrum RPC URL** (Alchemy, Infura, or your own node)

> **Never store private keys in this repository.** The `config/eoa_pool.json`
> file holds public addresses only. Private keys belong in a separate encrypted
> keystore outside this repo ΓÇö never in any file tracked by git.

---

## 5. Step-by-Step: First Run (Shadow Mode)

### Step 1 ΓÇö Clone and verify the repo

```bash
git clone <repo-url> project-chimera
cd project-chimera
git log --oneline -5
```

You should see at least the three commits from the initial build phases.

---

### Step 2 ΓÇö Build the Rust binary

```bash
cargo build -p chimera-core --release
```

This compiles the `chimera` binary into `target/release/chimera`. Expect the
first build to take 2ΓÇô5 minutes while dependencies compile.

If the build fails:
- Confirm your Rust toolchain: `rustup show`
- The project pins stable Rust ΓÇö check `core/rust-toolchain.toml`

---

### Step 3 ΓÇö Review and confirm your pacing config

Open `config/pacing.yaml` in a text editor. Confirm:

- `execute_mode: shadow` ΓÇö **must be `shadow` for your first run**
- `max_daily_net_usd` ΓÇö set conservatively (default 2000 is fine)
- `metrics_port: 9100` ΓÇö this is where the metrics server will listen
- `eoa_pool_path: config/eoa_pool.json` ΓÇö points to your wallet pool file

Do not change anything else yet. The defaults are conservative by design.

---

### Step 4 ΓÇö Prepare your EOA pool

Open `config/eoa_pool.json`. It ships with placeholder public addresses.
Replace the `address` fields with your actual worker EOA public addresses.

```json
{
  "wallets": [
    {
      "address": "0xYOUR_WORKER_EOA_ADDRESS",
      "chain_balances": { "base": 0, "arbitrum": 0 },
      "last_used": null,
      "use_count": 0,
      "status": "clean"
    }
  ]
}
```

> **Important:** `chain_balances` in this file is metadata only. The rotation
> script currently reads this value from the file rather than from live RPC.
> Keep it updated manually or via `scripts/rotate_eoa.py` until live RPC balance
> checking is wired in.

---

### Step 5 ΓÇö Generate a mock snapshot (optional but recommended)

```bash
python scripts/snapshot_generator.py --chain base --mock
```

This produces a synthetic Aave V3 market state snapshot without requiring a live
RPC connection. Useful for verifying the snapshot pipeline before connecting to
a real chain.

If `web3.py` is installed and you have a real RPC URL, you can also run:

```bash
python scripts/snapshot_generator.py --chain base --rpc-url <YOUR_RPC_URL>
```

---

### Step 6 ΓÇö Set your RPC URL

The binary currently reads a single env var:

```bash
export RPC_URL=https://base-mainnet.g.alchemy.com/v2/YOUR_KEY
```

> **Note:** `docker-compose.yml` defines `BASE_RPC_URL` and `ARB_RPC_URL`, but
> the current `main.rs` entrypoint reads only `RPC_URL`. Use `RPC_URL` when
> running the binary directly. The multi-chain env var wiring is planned but not
> yet implemented in the entrypoint.

---

### Step 7 ΓÇö Start the binary

```bash
./target/release/chimera
```

Or with explicit logging:

```bash
RUST_LOG=chimera=debug ./target/release/chimera
```

The default log level is `chimera=info`. All logs go to **stdout only** ΓÇö no
log files are written to disk by the current binary.

---

### Step 8 ΓÇö Verify startup (the 3 things to check)

**1. Look for this line in stdout:**
```
INFO chimera::orchestrator: Orchestrator starting chain_id=8453
```
This confirms config loaded, metrics bound, and the main loop started.
If you do not see this, the config load or port bind failed ΓÇö check stdout for
the error.

**2. Confirm the metrics endpoint responds:**
```bash
curl http://localhost:9100
```
You should get a `200 OK` response beginning with Prometheus text like:
```
# HELP chimera_candidates_seen_total ...
```
Any response means the metrics server is alive. Port `9100` is the default;
adjust if you changed `metrics_port` in `config/pacing.yaml`.

**3. Confirm the breaker is not tripped:**
In the metrics output, look for:
```
chimera_breaker_state 0
```
A value of `0` means the pacing engine is healthy. A value of `1` means the
circuit breaker was tripped ΓÇö check the logs for the reason before proceeding.

---

## 6. Starting the Monitoring Stack (Optional)

Chimera ships a full Prometheus + Grafana + Qdrant stack via Docker Compose.

```bash
docker compose -f docker/docker-compose.yml up -d
```

| Service | Address |
|---|---|
| Grafana dashboard | http://localhost:3002 |
| Prometheus UI | http://localhost:9090 |
| Qdrant vector store | http://localhost:6333 |

> **Port note:** The operator manual mentions port `3003` for Grafana. That is
> incorrect. The Docker Compose file maps `3002:3002`. Use `localhost:3002`.

Prometheus is pre-configured to scrape `chimera-core:9100` (the engine metrics
port) inside the Docker network. If you run the binary outside Docker, add
`localhost:9100` as a scrape target in `monitoring/prometheus.yml` or run the
binary inside the compose stack.

A pre-built Grafana dashboard JSON is mounted from
`monitoring/grafana/dashboards/` ΓÇö it should auto-provision on first start.

---

## 7. What Healthy Looks Like

After the engine has been running for a few minutes in shadow mode, a healthy
state looks like this:

| Signal | Healthy value | What to do if not |
|---|---|---|
| `chimera_breaker_state` | `0` | Check logs for breaker reason |
| `chimera_candidates_seen_total` | Incrementing | Check RPC connection |
| `chimera_sims_run_total{result="ok"}` | Incrementing | Check simulation config |
| Stdout logs | `INFO` lines, no `ERROR` | Check error text |
| Binary still running | Yes | Check exit code; config errors exit at startup |

> **Shadow mode note:** `chimera_profit_usd` and `chimera_revert_total` will
> stay near zero in shadow mode because no transactions are submitted. This is
> expected.

---

## 8. How to Fund Worker EOAs

> **Do not fund worker EOAs until you have confirmed shadow mode is running
> cleanly for at least a day.**

The funding helper is `scripts/fund_eoa.py`. It uses `web3.py` to send ETH from
a treasury wallet to each worker EOA that falls below a threshold.

**Before using it, verify the JSON shape.** The script currently expects a
`workers` key in the EOA pool JSON, but `config/eoa_pool.json` uses a `wallets`
key. Until this mismatch is resolved, either:

- Edit the script to read `wallets`, or
- Manually send small amounts (0.02 ETH per worker) from your treasury wallet
  directly

Worker EOAs need only enough ETH to cover gas ΓÇö roughly `0.02 ETH` per wallet
is the configured default. They should never hold significant balances.

For EOA rotation, use:

```bash
python scripts/rotate_eoa.py --dry-run
```

Review what it would do before running it live. The `--dry-run` flag prevents
writes to `config/eoa_pool.json`.

---

## 9. What Is Not Fully Wired Yet

Be aware of the following before assuming docs describe a fully runnable system:

| Feature | Status |
|---|---|
| Multi-chain RPC wiring (`BASE_RPC_URL` / `ARB_RPC_URL`) | Planned; `main.rs` reads `RPC_URL` only |
| JSONL audit trail / state file on disk | Library ready; not wired in `main.rs` yet |
| EOA pool loaded at startup | Library ready; not wired in `main.rs` yet |
| Live execution path | Not implemented in current binary |
| `scripts/check_balances.py` | Referenced in operator manual; does not exist |
| Keystore integrity test | Mentioned in docs; keystore directory not confirmed present |
| `fund_eoa.py` compatible with `eoa_pool.json` shape | Mismatch ΓÇö see Section 8 |

These are not blockers for shadow-mode operation. They matter when you move
toward live execution.

---

## 10. How to Sweep Profits (When Live)

`scripts/sweep_profits.py` is the profit sweep helper. It moves the balance of
each worker EOA back to the treasury, keeping a small gas reserve per worker.

This script is present but is currently a skeleton ΓÇö the `__main__` block prints
a ready message rather than executing a full sweep. Do not rely on it for
automated sweeps without reviewing and extending it first.

For now, sweep manually: send everything above `0.005 ETH` from each worker EOA
to your treasury wallet using your own wallet software.

---

## 11. Safety Rules ΓÇö Read These Before Touching Funds

1. **Never store private keys in this repo.** Not in `config/`, not in `scripts/`,
   not anywhere tracked by git.

2. **Never reuse a worker EOA after it has been used for a liquidation without
   running rotation first.** The rotation system exists for wallet hygiene ΓÇö it
   prevents forensic linkability between operations.

3. **Keep worker balances small.** Each worker needs `~0.02 ETH` for gas. More
   than that is unnecessary exposure.

4. **Do not change `execute_mode` to `live` without a 7-day shadow period.**
   The code enforces this, but the check only runs at config load ΓÇö it does not
   continuously monitor. Respect the intent.

5. **If the circuit breaker trips, stop and investigate before clearing it.**
   The breaker exists because something unexpected happened. Clearing it blindly
   is how small issues become large losses.

6. **Use `emergency_pause.py` if something looks wrong.** It writes a pause flag
   the engine is designed to respect.

7. **Caps are there for a reason.** The pacing guardrails in `config/pacing.yaml`
   are conservative by design. Do not raise them until you understand why they are
   set where they are.

8. **Test with mock data first.** Run `snapshot_generator.py --mock` before
   connecting to a live chain. Confirm the metrics look right before funding.

---

## 12. Useful Commands at a Glance

```bash
# Build
cargo build -p chimera-core --release

# Run tests
cargo test -p chimera-core

# Run binary with debug logging
RUST_LOG=chimera=debug ./target/release/chimera

# Generate mock snapshot
python scripts/snapshot_generator.py --chain base --mock

# Start monitoring stack
docker compose -f docker/docker-compose.yml up -d

# Check metrics
curl http://localhost:9100

# Dry-run EOA rotation
python scripts/rotate_eoa.py --dry-run

# Emergency pause
python scripts/emergency_pause.py

# Run Foundry contract tests (requires foundry)
forge test --root contracts/
```

---

## 13. Where to Go Next

| You want to... | Read this |
|---|---|
| Understand the architecture more deeply | `docs/architecture.md` |
| Tune config parameters | `docs/operator-manual.md` (Section: Config Tuning) |
| Handle an emergency | `docs/emergency-procedures.md` |
| Understand the test strategy | `docs/testing-strategy-liquidations.md` |
| Understand the security model | `docs/security-research.md` |
| Understand the Aave V3 protocol | `docs/research/aave-v3-liquidation-compendium.md` |

---

*This document was generated from a recursive analysis of the current repository
state. Claims marked "Library ready; not wired in main.rs yet" reflect the code
as of the date above ΓÇö check `core/src/main.rs` directly to confirm whether
additional wiring has been added since.*
