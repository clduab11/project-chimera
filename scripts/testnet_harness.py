#!/usr/bin/env python3
"""testnet_harness.py — Base Sepolia practice loop (zero real funds).

Subcommands: plan (default) / provision / faucet / fund / status / shadow-env.
Delegates all real work to the existing scripts (provision_wallets, fund_eoa,
check_balances) per the rotate_wallet.py delegator pattern — nothing is forked.

INVARIANT (AGENTS.md #5): imports without web3/eth_account; `plan`,
`provision --dry-run`, `shadow-env` and every --help run offline. All online
subcommands are chain-id guarded to the configured testnet (84532) with NO
override flag: the first RPC call is eth_chainId, and a mismatch exits 3
before any other RPC/account/balance call. `fund` re-asserts the guard
immediately before delegating to fund_eoa.fund_wallets.

Custody: the keystore password comes ONLY from CHIMERA_KEYSTORE_PASSWORD.
The treasury key is decrypted in memory and passed as a function argument —
never argv, never logged, never printed. The only execute-mode value this
module ever emits is the literal "shadow".

Usage:
  python scripts/testnet_harness.py                    # plan (default)
  python scripts/testnet_harness.py provision
  python scripts/testnet_harness.py faucet --wait
  python scripts/testnet_harness.py fund --dry-run
  python scripts/testnet_harness.py status --min-balance 0.01
  python scripts/testnet_harness.py shadow-env
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import time
from pathlib import Path
from typing import Any

# Optional dependencies. Never required for import, --help, plan, or
# shadow-env (Invariant #5).
try:
    from web3 import Web3
except ImportError:  # pragma: no cover - environment dependent
    Web3 = None  # type: ignore[assignment]
try:
    from eth_account import Account
except ImportError:  # pragma: no cover - environment dependent
    Account = None  # type: ignore[assignment]

# Sibling-script imports per the rotate_wallet.py delegator pattern (G8).
_SCRIPT_DIR = Path(__file__).resolve().parent
if str(_SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPT_DIR))

import provision_wallets  # noqa: E402
import fund_eoa  # noqa: E402  (reuse, never fork)
import check_balances  # noqa: E402

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("testnet_harness")

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
DEFAULT_TESTNET_CONFIG = "config/testnet.base-sepolia.json"
DEFAULT_TESTNET_POOL = "config/eoa_pool.sepolia.json"
TESTNET_KEYSTORE_ROOT = Path.home() / ".chimera" / "keystores" / "testnet"
EXIT_OK, EXIT_FAIL, EXIT_USAGE, EXIT_CHAIN_GUARD = 0, 1, 2, 3

PASSWORD_ENV = "CHIMERA_KEYSTORE_PASSWORD"
TREASURY_ENV = "CHIMERA_TREASURY_KEYSTORE"
WEI_PER_ETH = 10**18
FAUCET_POLL_SECONDS = 15

_REPO_ROOT = provision_wallets.find_repo_root(Path(__file__).resolve())

_WEB3_MISSING_MSG = (
    "web3 is not installed; this subcommand needs it. Install project "
    "requirements (pip install -r requirements.txt) or use the offline "
    "subcommands (plan, shadow-env, provision --dry-run, status --mock)."
)


def _resolve_repo_path(value: str) -> Path:
    """Resolve a possibly repo-relative path (so defaults work from any cwd)."""
    path = Path(value).expanduser()
    if path.exists() or path.is_absolute() or _REPO_ROOT is None:
        return path
    candidate = _REPO_ROOT / value
    return candidate if candidate.exists() else path


def load_testnet_config(path: Path) -> dict[str, Any]:
    """stdlib json load + shape check (chain_id / rpc_url / funding).
    Chain ID is hardcoded to 84532 (Base Sepolia) — never accepts any other
    testnet, even if the config file declares a different chain_id."""
    if not path.exists():
        logger.error("Testnet config not found at %s", path)
        raise SystemExit(EXIT_FAIL)
    try:
        with open(path, "r", encoding="utf-8") as f:
            config = json.load(f)
    except (OSError, json.JSONDecodeError) as exc:
        logger.error("Could not parse testnet config %s: %s", path, exc)
        raise SystemExit(EXIT_FAIL)
    if not isinstance(config.get("chain_id"), int):
        logger.error("Testnet config %s: 'chain_id' must be an integer.", path)
        raise SystemExit(EXIT_FAIL)
    if config["chain_id"] != 84532:
        logger.error(
            "Testnet config %s: chain_id %s is not 84532 (Base Sepolia). "
            "This harness is hardcoded to Base Sepolia only.",
            path,
            config["chain_id"],
        )
        raise SystemExit(EXIT_FAIL)
    return config


# ---------------------------------------------------------------------------
# Guards (§5.2 — non-negotiable, no override flag exists)
# ---------------------------------------------------------------------------
def _build_web3(rpc: str) -> Any:
    """Construct the raw Web3 HTTP client. Isolated so tests can patch it."""
    if Web3 is None:  # pragma: no cover - guarded by callers
        logger.error(_WEB3_MISSING_MSG)
        raise SystemExit(EXIT_USAGE)
    return Web3(Web3.HTTPProvider(rpc, request_kwargs={"timeout": 15}))


def make_guarded_web3(rpc: str, expected_chain_id: int | None = None) -> Any:
    """Connect, then check eth chain_id FIRST; SystemExit(EXIT_CHAIN_GUARD) on
    mismatch before any other RPC/account/balance call. The chain guard is
    hardcoded to 84532 (Base Sepolia) regardless of the config or caller."""
    w3 = _build_web3(rpc)
    try:
        actual = w3.eth.chain_id  # the FIRST and only pre-guard RPC call
    except Exception as exc:  # noqa: BLE001 - surface provider errors cleanly
        logger.error("Failed to fetch chain id from %s: %s", rpc, exc)
        raise SystemExit(EXIT_FAIL)
    if actual != 84532:
        logger.error(
            "CHAIN GUARD: RPC %s reports chain_id %s but the testnet harness "
            "is hardcoded to 84532 (Base Sepolia). Refusing to proceed — "
            "there is no override.",
            rpc,
            actual,
        )
        raise SystemExit(EXIT_CHAIN_GUARD)
    return w3


def assert_testnet_pool(pool_path: Path) -> None:
    """SystemExit(EXIT_CHAIN_GUARD) if the pool path is the mainnet pool
    (config/eoa_pool.json). Checked before any network activity."""
    resolved = Path(pool_path).expanduser().resolve()
    mainnet = (
        (_REPO_ROOT / "config" / "eoa_pool.json").resolve()
        if _REPO_ROOT is not None
        else None
    )
    if resolved.name == "eoa_pool.json" or (mainnet is not None and resolved == mainnet):
        logger.error(
            "POOL GUARD: %s resolves to the mainnet EOA pool. fund only "
            "operates on the sepolia pool (%s).",
            pool_path,
            DEFAULT_TESTNET_POOL,
        )
        raise SystemExit(EXIT_CHAIN_GUARD)


def assert_testnet_keystore(keystore_path: Path) -> None:
    """SystemExit(EXIT_CHAIN_GUARD) unless the resolved keystore path is under
    the testnet keystore root — a mainnet treasury keystore can never be
    decrypted by this harness."""
    root = Path(TESTNET_KEYSTORE_ROOT).expanduser().resolve()
    resolved = Path(keystore_path).expanduser().resolve()
    if resolved != root and root not in resolved.parents:
        logger.error(
            "KEYSTORE GUARD: %s is not under the testnet keystore root %s. "
            "Refusing to touch a non-testnet keystore.",
            resolved,
            root,
        )
        raise SystemExit(EXIT_CHAIN_GUARD)


# ---------------------------------------------------------------------------
# Keystore helpers
# ---------------------------------------------------------------------------
def _treasury_keystore_path() -> Path:
    env_val = os.environ.get(TREASURY_ENV, "").strip()
    if env_val:
        return Path(env_val).expanduser()
    return Path(TESTNET_KEYSTORE_ROOT) / "treasury.json"


def _workers_keystore_dir() -> Path:
    return Path(TESTNET_KEYSTORE_ROOT) / "workers"


def read_password_from_env() -> str:
    password = os.environ.get(PASSWORD_ENV, "")
    if not password:
        logger.error(
            "%s is not set (or empty). Export the keystore password via that "
            "environment variable; there is deliberately no CLI flag for it.",
            PASSWORD_ENV,
        )
        raise SystemExit(EXIT_USAGE)
    return password


def read_treasury_address(keystore_path: Path) -> str:
    """Read the plaintext 'address' field of the V3 keystore JSON — no
    decryption, no password needed."""
    if not keystore_path.exists():
        logger.error(
            "Treasury keystore not found at %s. Run the `provision` "
            "subcommand first.",
            keystore_path,
        )
        raise SystemExit(EXIT_FAIL)
    try:
        with open(keystore_path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, json.JSONDecodeError) as exc:
        logger.error("Unreadable treasury keystore %s: %s", keystore_path, exc)
        raise SystemExit(EXIT_FAIL)
    address = str(data.get("address", "")).strip()
    if not address:
        logger.error("Treasury keystore %s has no 'address' field.", keystore_path)
        raise SystemExit(EXIT_FAIL)
    if not address.startswith("0x"):
        address = "0x" + address
    return address


def decrypt_treasury_in_memory(keystore_path: Path, password: str) -> str:
    """eth_account.Account.decrypt -> hex key string held in memory only.
    Callers must never log, print, or place the return value in argv."""
    if Account is None:
        logger.error(_WEB3_MISSING_MSG)
        raise SystemExit(EXIT_USAGE)
    if not keystore_path.exists():
        logger.error(
            "Treasury keystore not found at %s. Run the `provision` "
            "subcommand first.",
            keystore_path,
        )
        raise SystemExit(EXIT_FAIL)
    with open(keystore_path, "r", encoding="utf-8") as f:
        keystore_json = json.load(f)
    try:
        key = Account.decrypt(keystore_json, password)
    except ValueError as exc:
        logger.error("Could not decrypt treasury keystore %s: %s", keystore_path, exc)
        raise SystemExit(EXIT_FAIL)
    key_hex = key.hex()
    if not key_hex.startswith("0x"):
        key_hex = "0x" + key_hex
    del key
    return key_hex


# ---------------------------------------------------------------------------
# Shadow env block (§5.3 — the only mode value anywhere is "shadow")
# ---------------------------------------------------------------------------
_PASSWORD_COMMENT = (
    f"{PASSWORD_ENV} must be set manually by the operator "
    "(never emitted by tooling)"
)


def _env_pairs(config: dict[str, Any], pool_path: Path) -> list[tuple[str, str]]:
    return [
        ("CHIMERA_EXECUTE_MODE", "shadow"),
        ("CHIMERA_CHAIN_ID", str(config["chain_id"])),
        ("CHIMERA_EOA_POOL_PATH", str(pool_path)),
        ("CHIMERA_WORKER_KEYSTORE_DIR", str(_workers_keystore_dir())),
        ("CHIMERA_TREASURY_KEYSTORE", str(_treasury_keystore_path())),
    ]


def render_shadow_env(config: dict[str, Any], pool_path: Path) -> str:
    pairs = _env_pairs(config, pool_path)
    lines: list[str] = []
    lines.append("# --- PowerShell ---")
    for name, value in pairs:
        lines.append(f'$env:{name} = "{value}"')
    lines.append(f"# {_PASSWORD_COMMENT}")
    lines.append("")
    lines.append("# --- POSIX ---")
    for name, value in pairs:
        lines.append(f"export {name}={value}")
    lines.append(f"# {_PASSWORD_COMMENT}")
    return "\n".join(lines)


def write_dotenv(config: dict[str, Any], pool_path: Path, dotenv_path: Path) -> None:
    """Write NAME=value lines to a file OUTSIDE the repo (exit 2 otherwise)."""
    resolved = dotenv_path.expanduser().resolve()
    if _REPO_ROOT is not None:
        root = _REPO_ROOT.resolve()
        if resolved == root or root in resolved.parents:
            logger.error(
                "Refusing to write dotenv %s: it is inside the repository "
                "root %s. Pick a path outside the repo.",
                resolved,
                root,
            )
            raise SystemExit(EXIT_USAGE)
    lines = [f"{name}={value}" for name, value in _env_pairs(config, pool_path)]
    lines.append(f"# {_PASSWORD_COMMENT}")
    resolved.parent.mkdir(parents=True, exist_ok=True)
    resolved.write_text("\n".join(lines) + "\n", encoding="utf-8")
    logger.info("Dotenv written to %s", resolved)


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------
def cmd_plan(args: argparse.Namespace) -> int:
    config_path = _resolve_repo_path(args.config)
    pool_path = _resolve_repo_path(args.eoa_pool)
    config = load_testnet_config(config_path)
    funding = config.get("funding", {})
    lines = [
        "Chimera testnet practice plan (Base Sepolia — zero real funds)",
        "-" * 72,
        f"config:            {config_path}",
        f"eoa pool:          {pool_path}",
        f"chain_id:          {config['chain_id']}",
        f"rpc:               {config.get('rpc_url', '?')}",
        f"treasury keystore: {_treasury_keystore_path()}",
        f"worker keystores:  {_workers_keystore_dir()}",
        f"workers:           {config.get('workers', 3)}",
        f"treasury target:   {funding.get('treasury_target_eth', '?')} ETH",
        f"worker top-up:     {funding.get('worker_topup_eth', '?')} ETH",
        "-" * 72,
        "Sequence:",
        "  1. provision          (offline: treasury + worker keystores, pool sync)",
        "  2. faucet --wait      (paste the treasury address into a faucet)",
        "  3. fund --dry-run     then fund   (treasury fans out testnet ETH)",
        "  4. status             (all workers should read ok)",
        "  5. shadow-env         (env block to boot the engine in shadow)",
        "",
        "Faucets:",
    ]
    for faucet in config.get("faucets", []):
        lines.append(f"  - {faucet.get('name', '?')}: {faucet.get('url', '?')}")
    lines.append("")
    lines.append("Environment block (also via the shadow-env subcommand):")
    lines.append(render_shadow_env(config, pool_path))
    print("\n".join(lines))
    return EXIT_OK


def cmd_provision(args: argparse.Namespace) -> int:
    config_path = _resolve_repo_path(args.config)
    pool_path = _resolve_repo_path(args.eoa_pool)
    config = load_testnet_config(config_path)
    workers = int(config.get("workers", 3))
    root = Path(TESTNET_KEYSTORE_ROOT)

    treasury_argv = ["--role", "treasury", "--keystore-dir", str(root)]
    worker_argv = [
        "--eoa-pool",
        str(pool_path),
        "--keystore-dir",
        str(_workers_keystore_dir()),
        "--count",
        str(workers),
        "--label-prefix",
        "chimera-sepolia-{:02d}",
    ]
    if args.dry_run:
        treasury_argv.append("--dry-run")
        worker_argv.append("--dry-run")

    logger.info("Provisioning testnet treasury under %s", root)
    rc = provision_wallets.main(treasury_argv)
    if rc != EXIT_OK:
        return rc
    logger.info("Provisioning %d testnet workers, syncing %s", workers, pool_path)
    return provision_wallets.main(worker_argv)


def cmd_faucet(args: argparse.Namespace) -> int:
    config_path = _resolve_repo_path(args.config)
    config = load_testnet_config(config_path)
    funding = config.get("funding", {})
    target_eth = float(funding.get("treasury_target_eth", 0.1))

    treasury_path = _treasury_keystore_path()
    address = read_treasury_address(treasury_path)

    print(f"Treasury address (paste into a faucet): {address}")
    print(f"Target balance: {target_eth} ETH on {config.get('chain', '?')}")
    print("-" * 72)
    print(f"{'FAUCET':<32}URL")
    print("-" * 72)
    for faucet in config.get("faucets", []):
        print(f"{faucet.get('name', '?'):<32}{faucet.get('url', '?')}")
    print("-" * 72)

    if not args.wait:
        return EXIT_OK

    if Web3 is None:
        logger.error(_WEB3_MISSING_MSG)
        return EXIT_USAGE

    w3 = make_guarded_web3(config["rpc_url"])
    checksum = w3.to_checksum_address(address)
    target_wei = int(target_eth * WEI_PER_ETH)
    while True:
        balance = w3.eth.get_balance(checksum)
        logger.info(
            "Treasury %s balance: %.6f ETH (target %.6f)",
            checksum,
            balance / WEI_PER_ETH,
            target_eth,
        )
        if balance >= target_wei:
            logger.info("Treasury target reached.")
            return EXIT_OK
        time.sleep(FAUCET_POLL_SECONDS)


def cmd_fund(args: argparse.Namespace) -> int:
    # Offline guards FIRST — before any config-independent network attempt.
    pool_path = _resolve_repo_path(args.eoa_pool)
    assert_testnet_pool(pool_path)

    treasury_path = _treasury_keystore_path()
    assert_testnet_keystore(treasury_path)

    config_path = _resolve_repo_path(args.config)
    config = load_testnet_config(config_path)
    funding = config.get("funding", {})
    min_balance = float(funding.get("min_worker_balance_eth", 0.01))
    fund_amount = float(funding.get("worker_topup_eth", 0.02))
    rpc = config["rpc_url"]
    chain_id = config["chain_id"]

    if Web3 is None:
        logger.error(_WEB3_MISSING_MSG)
        return EXIT_USAGE

    # Chain guard: the very first RPC call, before password/decryption (§5.2).
    make_guarded_web3(rpc)

    password = read_password_from_env()
    treasury_key = decrypt_treasury_in_memory(treasury_path, password)

    # Re-assert the guard immediately before delegating to the send loop.
    make_guarded_web3(rpc)

    logger.info(
        "Delegating to fund_eoa.fund_wallets (pool=%s, min=%s ETH, top-up=%s "
        "ETH, dry_run=%s)",
        pool_path,
        min_balance,
        fund_amount,
        args.dry_run,
    )
    result = fund_eoa.fund_wallets(
        treasury_key=treasury_key,
        rpc=rpc,
        eoa_pool_path=str(pool_path),
        min_balance=min_balance,
        fund_amount=fund_amount,
        wait=True,
        dry_run=args.dry_run,
    )
    del treasury_key
    if result < 0:
        return EXIT_FAIL
    logger.info("fund complete: %d wallet(s) funded", result)
    return EXIT_OK


def cmd_status(args: argparse.Namespace) -> int:
    config_path = _resolve_repo_path(args.config)
    pool_path = _resolve_repo_path(args.eoa_pool)
    config = load_testnet_config(config_path)
    funding = config.get("funding", {})
    min_balance = (
        args.min_balance
        if args.min_balance is not None
        else float(funding.get("min_worker_balance_eth", 0.01))
    )
    rpc = args.rpc or config["rpc_url"]

    try:
        wallets = check_balances.load_wallets(str(pool_path))
    except (FileNotFoundError, ValueError, json.JSONDecodeError) as exc:
        logger.error("Failed to load EOA pool: %s", exc)
        return EXIT_FAIL

    online = not args.mock
    if online:
        if Web3 is None:
            logger.error(_WEB3_MISSING_MSG)
            return EXIT_USAGE
        # Chain guard before any balance call.
        make_guarded_web3(rpc)

    try:
        results = check_balances.collect_balances(
            wallets,
            rpc=rpc if online else None,
            tokens=[],
            min_balance=min_balance,
            mock=args.mock,
        )
    except (ConnectionError, RuntimeError) as exc:
        logger.error("Balance collection failed: %s", exc)
        return EXIT_FAIL

    if args.json:
        print(
            json.dumps(
                {
                    "chain": config.get("chain", "base-sepolia"),
                    "min_balance": min_balance,
                    "wallets": [r.to_dict() for r in results],
                },
                indent=2,
            )
        )
    else:
        check_balances.print_table(results, config.get("chain", "base-sepolia"), min_balance)

    low_wallets = [r for r in results if r.low and not r.excluded]
    if low_wallets:
        logger.warning("%d wallet(s) below minimum balance", len(low_wallets))
        return EXIT_FAIL
    return EXIT_OK


def cmd_shadow_env(args: argparse.Namespace) -> int:
    config_path = _resolve_repo_path(args.config)
    pool_path = _resolve_repo_path(args.eoa_pool)
    config = load_testnet_config(config_path)
    print(render_shadow_env(config, pool_path))
    if args.emit_dotenv:
        write_dotenv(config, pool_path, Path(args.emit_dotenv))
    return EXIT_OK


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def _common_parser() -> argparse.ArgumentParser:
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument(
        "--config",
        default=DEFAULT_TESTNET_CONFIG,
        help=f"Testnet constants JSON (default {DEFAULT_TESTNET_CONFIG}).",
    )
    common.add_argument(
        "--eoa-pool",
        default=DEFAULT_TESTNET_POOL,
        help=f"Testnet EOA pool path (default {DEFAULT_TESTNET_POOL}).",
    )
    common.add_argument(
        "--log-level",
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
        help="Logging verbosity.",
    )
    return common


def build_parser() -> argparse.ArgumentParser:
    common = _common_parser()
    parser = argparse.ArgumentParser(
        description=(
            "Base Sepolia practice harness: provision -> faucet -> fund -> "
            "status -> shadow-env, with zero real funds. All online "
            "subcommands are chain-id guarded to the configured testnet."
        ),
        parents=[common],
    )
    sub = parser.add_subparsers(dest="command")

    sub.add_parser("plan", parents=[common], help="Print the practice sequence (offline).")

    p_provision = sub.add_parser(
        "provision",
        parents=[common],
        help="Treasury + worker keystores under the testnet root; sync the sepolia pool.",
    )
    p_provision.add_argument(
        "--dry-run",
        action="store_true",
        help="Plan only; generates nothing (passed through to provision_wallets).",
    )

    p_faucet = sub.add_parser(
        "faucet",
        parents=[common],
        help="Print the treasury address + faucet table.",
    )
    p_faucet.add_argument(
        "--wait",
        action="store_true",
        help=f"Poll the treasury balance every {FAUCET_POLL_SECONDS}s until the target is reached.",
    )

    p_fund = sub.add_parser(
        "fund",
        parents=[common],
        help="Fan out testnet ETH from the treasury via fund_eoa.fund_wallets.",
    )
    p_fund.add_argument(
        "--dry-run",
        action="store_true",
        help="Compute intended funding without sending (passed through to fund_eoa).",
    )

    p_status = sub.add_parser(
        "status",
        parents=[common],
        help="Balance table for the sepolia pool via check_balances.",
    )
    p_status.add_argument("--rpc", default=None, help="Override the config rpc_url.")
    p_status.add_argument(
        "--min-balance",
        type=float,
        default=None,
        help="Minimum ETH balance (default: config funding.min_worker_balance_eth).",
    )
    p_status.add_argument("--json", action="store_true", help="Emit machine-readable JSON.")
    p_status.add_argument(
        "--mock",
        action="store_true",
        help="Offline mode: print pool addresses with 'mock' balances.",
    )

    p_shadow = sub.add_parser(
        "shadow-env",
        parents=[common],
        help="Emit the shadow-mode env block (PowerShell + POSIX). Offline.",
    )
    p_shadow.add_argument(
        "--emit-dotenv",
        default=None,
        metavar="PATH",
        help="Also write NAME=value lines to PATH (must be outside the repo).",
    )

    return parser


_COMMANDS = {
    "plan": cmd_plan,
    "provision": cmd_provision,
    "faucet": cmd_faucet,
    "fund": cmd_fund,
    "status": cmd_status,
    "shadow-env": cmd_shadow_env,
}


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    logger.setLevel(getattr(logging, args.log_level))
    command = args.command or "plan"
    return _COMMANDS[command](args)


if __name__ == "__main__":
    sys.exit(main())
