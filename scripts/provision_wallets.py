#!/usr/bin/env python3
"""provision_wallets.py — Project Chimera worker/treasury keystore provisioning.

Generates worker/treasury keypairs in memory, encrypts them as Web3 Secret
Storage V3 keystores (scrypt n=2^18, r=8, p=1, aes-128-ctr) written OUTSIDE
the repository, and syncs public addresses into config/eoa_pool.json while
preserving schema v2.0, `_comment`, and all rotation metadata.

INVARIANT (AGENTS.md #5): imports without web3/eth_account; --help and
--dry-run run stdlib-only. Private keys exist only in memory between
Account.create() and Account.encrypt(); they are never written to disk in
plaintext, never logged, never printed. The password comes ONLY from the
CHIMERA_KEYSTORE_PASSWORD environment variable — there is no password CLI
flag and no key-export/decrypt-and-print path anywhere in this module.

Usage:
  python scripts/provision_wallets.py --dry-run       # plan only, generates nothing
  python scripts/provision_wallets.py                 # provision workers to --count
  python scripts/provision_wallets.py --role treasury # single treasury keystore
  python scripts/provision_wallets.py --verify        # SignerRegistry parity check
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import re
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# Optional dependency. Never required for import, --help, or --dry-run
# (Invariant #5).
try:
    from eth_account import Account  # ships with web3>=6 (requirements.txt)
except ImportError:  # pragma: no cover - environment dependent
    Account = None  # type: ignore[assignment]

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("provision_wallets")

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
DEFAULT_POOL_PATH = "config/eoa_pool.json"
DEFAULT_COUNT = 10  # == pacing clean_eoa_pool_size default
DEFAULT_LABEL_FMT = "chimera-eoa-{:02d}"
DEFAULT_HOME_DIR = Path.home() / ".chimera" / "keystores"
# Runbook §2.1 / alloy-compatible scrypt parameters. generate_keystore reads
# these module-level names at call time (do NOT bake into def-time defaults).
SCRYPT_N, SCRYPT_R, SCRYPT_P = 2**18, 8, 1
EXIT_OK, EXIT_FAIL, EXIT_USAGE = 0, 1, 2

PASSWORD_ENV = "CHIMERA_KEYSTORE_PASSWORD"
WORKER_DIR_ENV = "CHIMERA_WORKER_KEYSTORE_DIR"
TREASURY_ENV = "CHIMERA_TREASURY_KEYSTORE"
ZERO_ADDRESS = "0x0000000000000000000000000000000000000000"

_PACING_WORKER_DIR_RE = re.compile(
    r'^\s*worker_keystore_dir:\s*"?([^"#\r\n]*?)"?\s*(?:#.*)?$'
)


# ---------------------------------------------------------------------------
# Data models
# ---------------------------------------------------------------------------
@dataclass
class PoolEntry:  # mirror of the six-field v2.0 wallet object (G6)
    address: str
    label: str
    chain_balances: dict[str, str] = field(
        default_factory=lambda: {"base": "0", "arbitrum": "0"}
    )
    last_used: int = 0
    use_count: int = 0
    excluded: bool = False

    def to_dict(self) -> dict[str, Any]:
        return {
            "address": self.address,
            "label": self.label,
            "chain_balances": self.chain_balances,
            "last_used": self.last_used,
            "use_count": self.use_count,
            "excluded": self.excluded,
        }


@dataclass
class ProvisionPlan:
    keep: list[str]  # addresses with matching keystores (REAL)
    replace: list[tuple[str, str]]  # (placeholder_address, label) to regenerate
    append: list[str]  # new labels to add to reach --count
    foreign: list[str]  # history-bearing entries w/o keystore (warn only)
    excluded: list[str]  # untouched by contract


# ---------------------------------------------------------------------------
# Path resolution and containment guard
# ---------------------------------------------------------------------------
def find_repo_root(start: Path) -> Path | None:
    """Walk up from `start` to the first directory containing `.git`."""
    current = start if start.is_dir() else start.parent
    for candidate in [current, *current.parents]:
        if (candidate / ".git").exists():
            return candidate
    return None


def _pacing_worker_keystore_dir(repo_root: Path | None) -> str:
    """Read worker_keystore_dir from config/pacing.yaml with a stdlib
    line-regex (no YAML dependency). Returns "" when absent/empty."""
    if repo_root is None:
        return ""
    pacing = repo_root / "config" / "pacing.yaml"
    if not pacing.exists():
        return ""
    try:
        for line in pacing.read_text(encoding="utf-8").splitlines():
            m = _PACING_WORKER_DIR_RE.match(line)
            if m:
                return m.group(1).strip()
    except OSError as exc:  # pragma: no cover - defensive
        logger.debug("Could not read %s: %s", pacing, exc)
    return ""


def resolve_keystore_dir(cli_value: str | None, role: str) -> Path:
    """Resolve the keystore target.

    Worker role (returns a directory), first non-empty wins:
      --keystore-dir > $CHIMERA_WORKER_KEYSTORE_DIR > worker_keystore_dir in
      config/pacing.yaml (stdlib line-regex) > ~/.chimera/keystores/workers.

    Treasury role (returns a FILE path):
      --keystore-dir/treasury.json > $CHIMERA_TREASURY_KEYSTORE >
      ~/.chimera/keystores/treasury.json.
    """
    if role == "treasury":
        if cli_value:
            return Path(cli_value).expanduser() / "treasury.json"
        env_val = os.environ.get(TREASURY_ENV, "").strip()
        if env_val:
            return Path(env_val).expanduser()
        return DEFAULT_HOME_DIR / "treasury.json"

    if cli_value:
        return Path(cli_value).expanduser()
    env_val = os.environ.get(WORKER_DIR_ENV, "").strip()
    if env_val:
        return Path(env_val).expanduser()
    repo_root = find_repo_root(Path(__file__).resolve())
    pacing_val = _pacing_worker_keystore_dir(repo_root)
    if pacing_val:
        return Path(pacing_val).expanduser()
    return DEFAULT_HOME_DIR / "workers"


def assert_outside_repo(keystore_dir: Path, repo_root: Path | None) -> None:
    """SystemExit(EXIT_USAGE) if `keystore_dir` is inside the repository.
    Not overridable — keys must never enter the repo (plan §3.1(2))."""
    if repo_root is None:
        return
    resolved = keystore_dir.expanduser().resolve()
    root = repo_root.resolve()
    inside = resolved == root or root in resolved.parents
    if inside:
        logger.error(
            "Refusing keystore target %s: it is inside the repository root %s. "
            "Keystores must live outside the repo (e.g. %s).",
            resolved,
            root,
            DEFAULT_HOME_DIR,
        )
        raise SystemExit(EXIT_USAGE)


# ---------------------------------------------------------------------------
# Password (env-only, by design — never argv, never logged)
# ---------------------------------------------------------------------------
def read_password_from_env() -> str:
    password = os.environ.get(PASSWORD_ENV, "")
    if not password:
        logger.error(
            "%s is not set (or empty). Export the keystore password via that "
            "environment variable; there is deliberately no CLI flag for it.",
            PASSWORD_ENV,
        )
        raise SystemExit(EXIT_USAGE)
    if len(password) < 12:
        logger.warning(
            "%s is shorter than 12 characters; consider a stronger passphrase.",
            PASSWORD_ENV,
        )
    return password


# ---------------------------------------------------------------------------
# Pool round-trip (raw dict — deliberately NOT rotate_eoa's lossy dataclasses)
# ---------------------------------------------------------------------------
def load_pool_raw(pool_path: Path) -> dict[str, Any]:
    """Raw json.load; preserves version/_comment/unknown keys (G7)."""
    if not pool_path.exists():
        logger.error("EOA pool not found at %s", pool_path)
        raise SystemExit(EXIT_FAIL)
    with open(pool_path, "r", encoding="utf-8") as f:
        data = json.load(f)
    wallets = data.get("wallets")
    if not isinstance(wallets, list):
        logger.error("EOA pool %s: 'wallets' must be a list", pool_path)
        raise SystemExit(EXIT_FAIL)
    logger.info("Loaded %d wallets from %s", len(wallets), pool_path)
    return data


def save_pool_atomic(pool: dict[str, Any], pool_path: Path) -> None:
    """Same-dir temp file + os.replace; indent=2 + trailing newline to match
    the committed config/eoa_pool.json formatting."""
    payload = json.dumps(pool, indent=2) + "\n"
    fd = tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=str(pool_path.parent),
        prefix=pool_path.name + ".",
        suffix=".tmp",
        delete=False,
    )
    try:
        with fd:
            fd.write(payload)
        os.replace(fd.name, pool_path)
    except BaseException:
        try:
            os.unlink(fd.name)
        except OSError:
            pass
        raise
    logger.info("Pool written atomically to %s", pool_path)


# ---------------------------------------------------------------------------
# Keystore inventory and wallet classification
# ---------------------------------------------------------------------------
def _normalize_address(value: str) -> str:
    value = value.strip().lower()
    if not value.startswith("0x"):
        value = "0x" + value
    return value


def inventory_keystores(keystore_dir: Path) -> dict[str, Path]:
    """lowercased 0x-address -> file, from each V3 JSON's plaintext "address"
    field. No decryption, no password (plan §3.1(3))."""
    inventory: dict[str, Path] = {}
    if not keystore_dir.is_dir():
        return inventory
    for path in sorted(keystore_dir.glob("*.json")):
        try:
            with open(path, "r", encoding="utf-8") as f:
                data = json.load(f)
            addr = data.get("address")
            if isinstance(addr, str) and addr:
                inventory[_normalize_address(addr)] = path
        except (OSError, json.JSONDecodeError, UnicodeDecodeError):
            logger.warning("Skipping unreadable keystore file %s", path)
    return inventory


def classify_entry(entry: dict[str, Any], have_keystore: bool) -> str:
    """Returns "REAL" | "PLACEHOLDER" | "FOREIGN" | "EXCLUDED" (plan §3.1(5))."""
    if bool(entry.get("excluded", False)):
        return "EXCLUDED"
    if have_keystore:
        return "REAL"
    if int(entry.get("use_count", 0)) == 0 and int(entry.get("last_used", 0)) == 0:
        return "PLACEHOLDER"
    return "FOREIGN"


def build_plan(
    pool: dict[str, Any],
    inventory: dict[str, Path],
    count: int,
    label_fmt: str = DEFAULT_LABEL_FMT,
) -> ProvisionPlan:
    wallets = pool.get("wallets", [])
    keep: list[str] = []
    placeholders: list[tuple[str, str]] = []
    foreign: list[str] = []
    excluded: list[str] = []
    non_excluded_count = 0

    for entry in wallets:
        address = str(entry.get("address", ""))
        have_keystore = _normalize_address(address) in inventory
        kind = classify_entry(entry, have_keystore)
        if kind != "EXCLUDED":
            non_excluded_count += 1
        if kind == "REAL":
            keep.append(address)
        elif kind == "PLACEHOLDER":
            placeholders.append((address, str(entry.get("label", ""))))
        elif kind == "FOREIGN":
            foreign.append(address)
        else:
            excluded.append(address)

    needed = max(0, count - len(keep))
    replace = placeholders[:needed]

    append: list[str] = []
    if non_excluded_count < count:
        next_index = len(wallets) + 1
        for _ in range(count - non_excluded_count):
            if "{" in label_fmt:
                append.append(label_fmt.format(next_index))
            else:
                append.append(f"{label_fmt}-{next_index:02d}")
            next_index += 1

    return ProvisionPlan(
        keep=keep,
        replace=replace,
        append=append,
        foreign=foreign,
        excluded=excluded,
    )


# ---------------------------------------------------------------------------
# Keystore generation (the ONLY scope where private key bytes exist)
# ---------------------------------------------------------------------------
def _write_json_atomic_0600(target: Path, payload: dict[str, Any]) -> None:
    """Same-dir temp created via os.open with 0o600 (best-effort on Windows),
    then os.replace onto the final name."""
    tmp_path = target.parent / f".{target.name}.tmp-{os.getpid()}"
    flags = os.O_CREAT | os.O_EXCL | os.O_WRONLY
    fd = os.open(str(tmp_path), flags, 0o600)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(json.dumps(payload, indent=2) + "\n")
        os.replace(tmp_path, target)
    except BaseException:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass
        raise


def generate_keystore(keystore_dir: Path, label: str, password: str) -> str:
    """Account.create() -> Account.encrypt(kdf="scrypt") -> atomic 0o600 write
    as "<label>--<0xAddress>.json". Returns the checksummed address; the
    private key never leaves this function's scope and is deleted immediately
    after encryption.

    NOTE: SCRYPT_N/SCRYPT_R/SCRYPT_P are read from module globals at call
    time. With eth_account/eth_keyfile, kdf="scrypt" + iterations=N maps to
    scrypt n=N with r=8, p=1 (library defaults matching SCRYPT_R/SCRYPT_P).
    """
    if Account is None:  # pragma: no cover - guarded by callers
        logger.error("eth_account is required to generate keystores.")
        raise SystemExit(EXIT_USAGE)
    acct = Account.create()  # OS CSPRNG, in memory only
    address: str = acct.address
    keyfile = Account.encrypt(acct.key, password, kdf="scrypt", iterations=SCRYPT_N)
    del acct  # drop key material immediately after encryption
    target = keystore_dir / f"{label}--{address}.json"
    _write_json_atomic_0600(target, keyfile)
    logger.info("Keystore written: %s (label=%s, address=%s)", target, label, address)
    return address


# ---------------------------------------------------------------------------
# Plan application and pool rewrite
# ---------------------------------------------------------------------------
def apply_plan(
    pool: dict[str, Any], created: list[tuple[str, str]]
) -> dict[str, Any]:
    """(label, address) pairs -> replace placeholder rows in place (preserving
    each row's label and zeroed metadata) / append new six-field rows."""
    wallets = pool.get("wallets", [])
    labels_present = {str(w.get("label", "")): w for w in wallets}
    for label, address in created:
        entry = labels_present.get(label)
        if entry is not None:
            entry["address"] = address
        else:
            wallets.append(PoolEntry(address=address, label=label).to_dict())
    return pool


# ---------------------------------------------------------------------------
# Treasury role
# ---------------------------------------------------------------------------
def provision_treasury(keystore_path: Path, password: str) -> str:
    """Single treasury keystore at `keystore_path`. Pool never read/written."""
    if keystore_path.exists():
        logger.info("Treasury keystore already exists at %s; not overwriting.", keystore_path)
        try:
            with open(keystore_path, "r", encoding="utf-8") as f:
                return _normalize_address(json.load(f).get("address", ""))
        except (OSError, json.JSONDecodeError):
            logger.error("Existing treasury keystore at %s is unreadable.", keystore_path)
            raise SystemExit(EXIT_FAIL)
    if Account is None:  # pragma: no cover - guarded by callers
        logger.error("eth_account is required to generate the treasury keystore.")
        raise SystemExit(EXIT_USAGE)
    keystore_path.parent.mkdir(parents=True, exist_ok=True)
    acct = Account.create()
    address: str = acct.address
    keyfile = Account.encrypt(acct.key, password, kdf="scrypt", iterations=SCRYPT_N)
    del acct
    _write_json_atomic_0600(keystore_path, keyfile)
    logger.info("Treasury keystore written: %s (address=%s)", keystore_path, address)
    return address


# ---------------------------------------------------------------------------
# --verify: python mirror of SignerRegistry::validate_against_eoa_pool (G3)
# ---------------------------------------------------------------------------
def verify_parity(pool_path: Path, keystore_dir: Path, password: str) -> int:
    """Skip excluded + zero-address; every other pool address must have a
    decrypting keystore whose derived address matches; extra keystores ->
    warning, exit stays 0. Message shape mirrors signer_registry/mod.rs:230-233.
    Derived signers are discarded — nothing is exported or printed."""
    if Account is None:  # pragma: no cover - guarded by callers
        logger.error("eth_account is required for --verify.")
        return EXIT_USAGE

    derived: dict[str, Path] = {}
    if keystore_dir.is_dir():
        for path in sorted(keystore_dir.glob("*.json")):
            try:
                with open(path, "r", encoding="utf-8") as f:
                    raw = json.load(f)
                key = Account.decrypt(raw, password)
                address = Account.from_key(key).address
                del key
                derived[_normalize_address(address)] = path
            except (OSError, json.JSONDecodeError, ValueError) as exc:
                logger.warning("Unreadable keystore %s: %s", path, exc)

    pool = load_pool_raw(pool_path)
    pool_addrs: set[str] = set()
    for entry in pool.get("wallets", []):
        if bool(entry.get("excluded", False)):
            continue
        addr_str = str(entry.get("address", ""))
        norm = _normalize_address(addr_str)
        if norm == ZERO_ADDRESS:
            continue
        pool_addrs.add(norm)
        if norm not in derived:
            logger.error(
                "Live mode: EOA pool entry %s has no registered worker signer in %s",
                addr_str,
                pool_path,
            )
            return EXIT_FAIL

    for norm, path in derived.items():
        if norm not in pool_addrs:
            logger.warning(
                "Extra keystore not referenced by the pool: %s (address=%s)",
                path,
                norm,
            )

    logger.info(
        "Verify OK: all %d non-excluded pool addresses map to decrypting keystores.",
        len(pool_addrs),
    )
    return EXIT_OK


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------
def render_report(
    plan: ProvisionPlan,
    created: list[tuple[str, str]],
    keystore_dir: Path,
    as_json: bool,
    dry_run: bool = False,
) -> str:
    if as_json:
        return json.dumps(
            {
                "dry_run": dry_run,
                "keystore_dir": str(keystore_dir),
                "kept": plan.keep,
                "created": [
                    {"label": label, "address": address} for label, address in created
                ],
                "planned_replace": [
                    {"placeholder": addr, "label": label} for addr, label in plan.replace
                ],
                "planned_append": plan.append,
                "foreign": plan.foreign,
                "excluded": plan.excluded,
                "created_count": len(created),
            },
            indent=2,
        )

    lines: list[str] = []
    title = "Provisioning plan (dry-run)" if dry_run else "Provisioning result"
    lines.append(title)
    lines.append("-" * 72)
    lines.append(f"{'LABEL':<24}{'ADDRESS':<44}STATUS")
    lines.append("-" * 72)
    for addr in plan.keep:
        lines.append(f"{'':<24}{addr:<44}kept")
    for label, address in created:
        lines.append(f"{label:<24}{address:<44}created")
    if dry_run:
        for addr, label in plan.replace:
            lines.append(f"{label:<24}{addr:<44}would replace")
        for label in plan.append:
            lines.append(f"{label:<24}{'(new)':<44}would append")
    for addr in plan.foreign:
        lines.append(f"{'':<24}{addr:<44}foreign (untouched)")
    for addr in plan.excluded:
        lines.append(f"{'':<24}{addr:<44}excluded (untouched)")
    lines.append("-" * 72)
    lines.append(f"created: {len(created)}")
    lines.append("")
    lines.append("Environment for SignerRegistry:")
    lines.append(f"  {WORKER_DIR_ENV}={keystore_dir}")
    lines.append("")
    lines.append(
        "Reminder: config/pacing.yaml is untouched (Invariant #1); configure "
        "paths via CHIMERA_* env overrides."
    )
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Provision encrypted Web3 Secret Storage V3 keystores (outside the "
            "repo) and sync public addresses into the EOA pool. The keystore "
            "password is read ONLY from the CHIMERA_KEYSTORE_PASSWORD "
            "environment variable."
        )
    )
    parser.add_argument(
        "--eoa-pool",
        default=DEFAULT_POOL_PATH,
        help=f"Path to eoa_pool.json (default {DEFAULT_POOL_PATH}).",
    )
    parser.add_argument(
        "--keystore-dir",
        default=None,
        help=(
            "Keystore directory (worker role) or parent directory of "
            "treasury.json (treasury role). Must be OUTSIDE the repository."
        ),
    )
    parser.add_argument(
        "--count",
        type=int,
        default=DEFAULT_COUNT,
        help=f"Target number of worker wallets (default {DEFAULT_COUNT}).",
    )
    parser.add_argument(
        "--label-prefix",
        default=DEFAULT_LABEL_FMT,
        help=f"Label format for appended rows (default '{DEFAULT_LABEL_FMT}').",
    )
    parser.add_argument(
        "--role",
        default="worker",
        choices=["worker", "treasury"],
        help="worker: pool-synced keystores; treasury: single keystore, pool untouched.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Plan only. Generates NO key material, writes nothing, needs no password.",
    )
    parser.add_argument(
        "--verify",
        action="store_true",
        help="Parity check mirroring SignerRegistry::validate_against_eoa_pool.",
    )
    parser.add_argument("--json", action="store_true", help="Emit machine-readable JSON.")
    parser.add_argument(
        "--log-level",
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
        help="Logging verbosity.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    logger.setLevel(getattr(logging, args.log_level))

    repo_root = find_repo_root(Path(__file__).resolve())

    # ----- treasury role: guard + password + single keystore; pool untouched
    if args.role == "treasury":
        treasury_path = resolve_keystore_dir(args.keystore_dir, "treasury")
        assert_outside_repo(treasury_path.parent, repo_root)
        if args.dry_run:
            print(f"Dry-run: would write treasury keystore at {treasury_path}")
            print(f"{TREASURY_ENV}={treasury_path}")
            return EXIT_OK
        if Account is None:
            logger.error(
                "eth_account is not installed; cannot generate keystores. "
                "Install project requirements (pip install -r requirements.txt)."
            )
            return EXIT_USAGE
        password = read_password_from_env()
        address = provision_treasury(treasury_path, password)
        logger.info("Treasury address: %s", address)
        print(f"{TREASURY_ENV}={treasury_path}")
        return EXIT_OK

    # ----- worker role
    keystore_dir = resolve_keystore_dir(args.keystore_dir, "worker")
    assert_outside_repo(keystore_dir, repo_root)
    pool_path = Path(args.eoa_pool)

    if args.verify:
        if Account is None:
            logger.error(
                "--verify requires eth_account to decrypt keystores. "
                "Install project requirements (pip install -r requirements.txt)."
            )
            return EXIT_USAGE
        password = read_password_from_env()
        return verify_parity(pool_path, keystore_dir, password)

    pool = load_pool_raw(pool_path)
    inventory = inventory_keystores(keystore_dir)
    plan = build_plan(pool, inventory, args.count, args.label_prefix)

    for addr in plan.foreign:
        logger.warning(
            "Pool entry %s has rotation history but no keystore in %s; "
            "leaving it untouched (operator may hold that key elsewhere).",
            addr,
            keystore_dir,
        )

    if args.dry_run:
        # Short-circuit BEFORE the password read and BEFORE any Account call:
        # zero key material is generated in dry-run (plan §3.2, T-4).
        print(render_report(plan, [], keystore_dir, args.json, dry_run=True))
        return EXIT_OK

    if Account is None:
        logger.error(
            "eth_account is not installed; cannot generate keystores. "
            "Install project requirements (pip install -r requirements.txt), "
            "or use --dry-run to preview the plan without dependencies."
        )
        return EXIT_USAGE

    password = read_password_from_env()

    created: list[tuple[str, str]] = []
    if plan.replace or plan.append:
        keystore_dir.mkdir(parents=True, exist_ok=True)
        # Keystores first, pool second (crash ordering, plan §3.1(7c)).
        for _placeholder_addr, label in plan.replace:
            created.append((label, generate_keystore(keystore_dir, label, password)))
        for label in plan.append:
            created.append((label, generate_keystore(keystore_dir, label, password)))
        pool = apply_plan(pool, created)
        save_pool_atomic(pool, pool_path)
    else:
        logger.info("Nothing to do: pool already at target (0 wallets to create).")

    print(render_report(plan, created, keystore_dir, args.json))
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
