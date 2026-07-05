"""Tests T-1..T-10 for scripts/provision_wallets.py (plan §7.1).

All tests are offline and write only under pytest's tmp_path (never into the
repo tree). Slow-scrypt runs are avoided by monkeypatching the module-level
SCRYPT_N constant to 2**14 test-side; the shipped default (262144) is asserted
before every patch so the weaker factor never ships.
"""

from __future__ import annotations

import copy
import hashlib
import json
import logging
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPTS_DIR = REPO_ROOT / "scripts"
SCRIPT_PATH = SCRIPTS_DIR / "provision_wallets.py"
POOL_TEMPLATE = REPO_ROOT / "config" / "eoa_pool.json"
PASSWORD_ENV = "CHIMERA_KEYSTORE_PASSWORD"
TEST_PASSWORD = "unit-test-password-123"
FAST_SCRYPT_N = 2**14
SHIPPED_SCRYPT_N = 262144  # 2**18 — the custody-grade default that must ship

sys.path.insert(0, str(SCRIPTS_DIR))
import provision_wallets as pw  # noqa: E402

try:
    from eth_account import Account as RealAccount
except ImportError:  # pragma: no cover - environment dependent
    RealAccount = None

HEX64_RE = re.compile(r"(?i)(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])")


# ---------------------------------------------------------------------------
# Helpers / fixtures
# ---------------------------------------------------------------------------
def run_main(argv: list[str]) -> int:
    """Call pw.main in-process, normalizing SystemExit to an exit code."""
    try:
        rc = pw.main(argv)
    except SystemExit as exc:
        rc = exc.code if isinstance(exc.code, int) else 1
    return rc


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def recursive_listing(root: Path) -> set[str]:
    return {str(p.relative_to(root)) for p in root.rglob("*")}


def make_import_blocker(tmp_path: Path) -> Path:
    blocker = tmp_path / "import_blocker"
    blocker.mkdir()
    for name in ("web3", "eth_account"):
        (blocker / f"{name}.py").write_text(
            'raise ImportError("blocked for test")\n', encoding="utf-8"
        )
    return blocker


@pytest.fixture
def pool_copy(tmp_path: Path) -> Path:
    dest = tmp_path / "eoa_pool.json"
    shutil.copyfile(POOL_TEMPLATE, dest)
    return dest


@pytest.fixture
def env_password(monkeypatch: pytest.MonkeyPatch) -> str:
    monkeypatch.setenv(PASSWORD_ENV, TEST_PASSWORD)
    return TEST_PASSWORD


@pytest.fixture
def fast_scrypt(monkeypatch: pytest.MonkeyPatch) -> int:
    # Prove the shipped default is 2**18 BEFORE weakening it test-side only.
    assert pw.SCRYPT_N == SHIPPED_SCRYPT_N
    assert (pw.SCRYPT_R, pw.SCRYPT_P) == (8, 1)
    monkeypatch.setattr(pw, "SCRYPT_N", FAST_SCRYPT_N)
    return FAST_SCRYPT_N


# ---------------------------------------------------------------------------
# T-1
# ---------------------------------------------------------------------------
def test_help_works_without_web3(tmp_path: Path) -> None:
    blocker = make_import_blocker(tmp_path)
    env = os.environ.copy()
    env["PYTHONPATH"] = str(blocker) + os.pathsep + env.get("PYTHONPATH", "")
    result = subprocess.run(
        [sys.executable, str(SCRIPT_PATH), "--help"],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(REPO_ROOT),
        timeout=60,
    )
    assert result.returncode == 0, result.stderr
    assert "usage" in result.stdout.lower()


# ---------------------------------------------------------------------------
# T-2
# ---------------------------------------------------------------------------
def test_imports_without_eth_account(tmp_path: Path, pool_copy: Path) -> None:
    blocker = make_import_blocker(tmp_path)
    keystore_dir = tmp_path / "ks"
    snippet = (
        "import sys\n"
        f"sys.path.insert(0, {str(SCRIPTS_DIR)!r})\n"
        f"sys.path.insert(0, {str(blocker)!r})\n"
        "import provision_wallets as pw\n"
        "assert pw.Account is None, 'blocker failed: Account is not None'\n"
        "try:\n"
        f"    rc = pw.main(['--eoa-pool', {str(pool_copy)!r},"
        f" '--keystore-dir', {str(keystore_dir)!r}, '--count', '1'])\n"
        "except SystemExit as exc:\n"
        "    rc = exc.code if isinstance(exc.code, int) else 1\n"
        "sys.exit(0 if rc == 2 else 20)\n"
    )
    result = subprocess.run(
        [sys.executable, "-c", snippet],
        capture_output=True,
        text=True,
        cwd=str(REPO_ROOT),
        timeout=60,
    )
    assert result.returncode == 0, result.stderr
    assert not keystore_dir.exists()


# ---------------------------------------------------------------------------
# T-3
# ---------------------------------------------------------------------------
def test_generation_requires_password(
    tmp_path: Path, pool_copy: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv(PASSWORD_ENV, raising=False)
    keystore_dir = tmp_path / "ks"
    before = pool_copy.read_bytes()
    rc = run_main(
        ["--eoa-pool", str(pool_copy), "--keystore-dir", str(keystore_dir), "--count", "2"]
    )
    assert rc == 2
    assert not keystore_dir.exists()
    assert pool_copy.read_bytes() == before


# ---------------------------------------------------------------------------
# T-4 — dry-run generates nothing, writes nothing, leaks nothing
# ---------------------------------------------------------------------------
def test_dry_run_writes_nothing_and_leaks_nothing(
    tmp_path: Path,
    pool_copy: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    caplog: pytest.LogCaptureFixture,
) -> None:
    monkeypatch.delenv(PASSWORD_ENV, raising=False)
    keystore_dir = tmp_path / "ks"
    pool_hash_before = sha256_file(pool_copy)
    listing_before = recursive_listing(tmp_path)

    class _Sentinel:
        @staticmethod
        def create(*args, **kwargs):
            raise AssertionError("key material touched in dry-run")

        @staticmethod
        def encrypt(*args, **kwargs):
            raise AssertionError("key material touched in dry-run")

    monkeypatch.setattr(pw, "Account", _Sentinel)

    with caplog.at_level(logging.DEBUG, logger="provision_wallets"):
        rc = run_main(
            [
                "--dry-run",
                "--count",
                "3",
                "--eoa-pool",
                str(pool_copy),
                "--keystore-dir",
                str(keystore_dir),
            ]
        )
    captured = capsys.readouterr()

    assert rc == 0
    assert not keystore_dir.exists()
    assert sha256_file(pool_copy) == pool_hash_before
    assert recursive_listing(tmp_path) == listing_before
    combined = captured.out + captured.err + caplog.text
    assert HEX64_RE.findall(combined) == []


# ---------------------------------------------------------------------------
# T-5 — encrypted at rest, correct V3 format, address-only pool
# ---------------------------------------------------------------------------
@pytest.mark.skipif(RealAccount is None, reason="eth_account not installed")
def test_real_run_no_plaintext_on_disk(
    tmp_path: Path,
    pool_copy: Path,
    env_password: str,
    fast_scrypt: int,
    capsys: pytest.CaptureFixture[str],
    caplog: pytest.LogCaptureFixture,
) -> None:
    # The shipped default work factor is proven again explicitly here.
    assert SHIPPED_SCRYPT_N == 2**18

    keystore_dir = tmp_path / "ks"
    with caplog.at_level(logging.DEBUG, logger="provision_wallets"):
        rc = run_main(
            ["--count", "3", "--eoa-pool", str(pool_copy), "--keystore-dir", str(keystore_dir)]
        )
    captured = capsys.readouterr()
    assert rc == 0

    keystore_files = sorted(keystore_dir.glob("*.json"))
    assert len(keystore_files) == 3

    pool = json.loads(pool_copy.read_text(encoding="utf-8"))
    pool_addrs = {w["address"].lower() for w in pool["wallets"]}

    key_variants: list[bytes] = []
    for ks_file in keystore_files:
        data = json.loads(ks_file.read_text(encoding="utf-8"))
        assert data["version"] == 3
        assert data["crypto"]["cipher"] == "aes-128-ctr"
        assert data["crypto"]["kdf"] == "scrypt"
        assert data["crypto"]["kdfparams"]["n"] == fast_scrypt
        assert data["crypto"]["kdfparams"]["r"] == 8
        assert data["crypto"]["kdfparams"]["p"] == 1

        if sys.platform != "win32":
            assert (ks_file.stat().st_mode & 0o777) == 0o600

        key = RealAccount.decrypt(data, env_password)
        derived = RealAccount.from_key(key).address
        assert derived.lower() in pool_addrs

        key_hex = key.hex()
        if key_hex.startswith("0x"):
            key_hex = key_hex[2:]
        for variant in (
            key_hex.lower(),
            key_hex.upper(),
            "0x" + key_hex.lower(),
            "0x" + key_hex.upper(),
        ):
            key_variants.append(variant.encode("ascii"))
        del key

    # Byte-scan every file under the tmp tree (keystores, pool, temp litter).
    for path in tmp_path.rglob("*"):
        if not path.is_file():
            continue
        blob = path.read_bytes()
        for variant in key_variants:
            assert variant not in blob, f"plaintext key material found in {path}"

    logged = (captured.out + captured.err + caplog.text).encode("ascii", "ignore")
    for variant in key_variants:
        assert variant not in logged, "plaintext key material found in logs/output"


# ---------------------------------------------------------------------------
# T-6 — idempotency
# ---------------------------------------------------------------------------
@pytest.mark.skipif(RealAccount is None, reason="eth_account not installed")
def test_idempotent_second_run(
    tmp_path: Path,
    pool_copy: Path,
    env_password: str,
    fast_scrypt: int,
    capsys: pytest.CaptureFixture[str],
) -> None:
    keystore_dir = tmp_path / "ks"
    argv = ["--count", "3", "--eoa-pool", str(pool_copy), "--keystore-dir", str(keystore_dir)]

    assert run_main(argv) == 0
    capsys.readouterr()
    pool_bytes_after_first = pool_copy.read_bytes()
    keystores_after_first = {
        p.name: sha256_file(p) for p in keystore_dir.glob("*.json")
    }
    assert len(keystores_after_first) == 3

    rc = run_main(argv + ["--json"])
    captured = capsys.readouterr()
    assert rc == 0
    report = json.loads(captured.out)
    assert report["created_count"] == 0
    assert report["created"] == []

    assert pool_copy.read_bytes() == pool_bytes_after_first
    keystores_after_second = {
        p.name: sha256_file(p) for p in keystore_dir.glob("*.json")
    }
    assert keystores_after_second == keystores_after_first


# ---------------------------------------------------------------------------
# T-7 — schema + rotation metadata preserved; FOREIGN warned, never touched
# ---------------------------------------------------------------------------
@pytest.mark.skipif(RealAccount is None, reason="eth_account not installed")
def test_preserves_schema_and_rotation_metadata(
    tmp_path: Path,
    pool_copy: Path,
    env_password: str,
    fast_scrypt: int,
    caplog: pytest.LogCaptureFixture,
) -> None:
    keystore_dir = tmp_path / "ks"
    argv_base = ["--eoa-pool", str(pool_copy), "--keystore-dir", str(keystore_dir)]

    # Provision one wallet so it becomes REAL, then give it rotation history.
    assert run_main(argv_base + ["--count", "1"]) == 0
    pool = json.loads(pool_copy.read_text(encoding="utf-8"))
    template_comment = pool["_comment"]
    real_entry = pool["wallets"][0]
    real_entry["use_count"] = 7
    real_entry["last_used"] = 1750000000

    excluded_addr = "0x" + "d" * 40
    foreign_addr = "0x" + "b" * 40
    excluded_entry = {
        "address": excluded_addr,
        "label": "excluded-01",
        "chain_balances": {"base": "0", "arbitrum": "0"},
        "last_used": 0,
        "use_count": 0,
        "excluded": True,
    }
    foreign_entry = {
        "address": foreign_addr,
        "label": "foreign-01",
        "chain_balances": {"base": "0", "arbitrum": "0"},
        "last_used": 1700000000,
        "use_count": 3,
        "excluded": False,
    }
    pool["wallets"].append(excluded_entry)
    pool["wallets"].append(foreign_entry)
    pool_copy.write_text(json.dumps(pool, indent=2) + "\n", encoding="utf-8")

    real_snapshot = copy.deepcopy(real_entry)
    excluded_snapshot = copy.deepcopy(excluded_entry)
    foreign_snapshot = copy.deepcopy(foreign_entry)

    with caplog.at_level(logging.WARNING, logger="provision_wallets"):
        rc = run_main(argv_base + ["--count", "4"])
    assert rc == 0

    after = json.loads(pool_copy.read_text(encoding="utf-8"))
    assert after["version"] == "2.0"
    assert after["_comment"] == template_comment

    by_addr = {w["address"]: w for w in after["wallets"]}
    assert by_addr[real_snapshot["address"]] == real_snapshot
    assert by_addr[excluded_addr] == excluded_snapshot
    assert by_addr[foreign_addr] == foreign_snapshot

    six_fields = {"address", "label", "chain_balances", "last_used", "use_count", "excluded"}
    for wallet in after["wallets"]:
        assert set(wallet.keys()) == six_fields

    assert foreign_addr in caplog.text
    assert any(
        record.levelno == logging.WARNING and foreign_addr in record.getMessage()
        for record in caplog.records
    )


# ---------------------------------------------------------------------------
# T-8 — repo-containment guard
# ---------------------------------------------------------------------------
def test_refuses_keystore_dir_inside_repo(pool_copy: Path) -> None:
    inside = REPO_ROOT / "keystores-test-should-never-exist"
    rc = run_main(
        ["--eoa-pool", str(pool_copy), "--keystore-dir", str(inside), "--count", "1"]
    )
    assert rc == 2
    assert not inside.exists()


# ---------------------------------------------------------------------------
# T-9 — --verify parity with SignerRegistry::validate_against_eoa_pool
# ---------------------------------------------------------------------------
@pytest.mark.skipif(RealAccount is None, reason="eth_account not installed")
def test_verify_parity_mirrors_signer_registry(
    tmp_path: Path,
    env_password: str,
    fast_scrypt: int,
    caplog: pytest.LogCaptureFixture,
) -> None:
    pool_path = tmp_path / "pool.json"
    pool = {
        "version": "2.0",
        "_comment": "test pool",
        "wallets": [
            {
                "address": "0x1111111111111111111111111111111111111111",
                "label": "w-01",
                "chain_balances": {"base": "0", "arbitrum": "0"},
                "last_used": 0,
                "use_count": 0,
                "excluded": False,
            },
            {
                "address": "0x2222222222222222222222222222222222222222",
                "label": "w-02",
                "chain_balances": {"base": "0", "arbitrum": "0"},
                "last_used": 0,
                "use_count": 0,
                "excluded": False,
            },
        ],
    }
    pool_path.write_text(json.dumps(pool, indent=2) + "\n", encoding="utf-8")
    keystore_dir = tmp_path / "ks"
    argv_base = ["--eoa-pool", str(pool_path), "--keystore-dir", str(keystore_dir)]

    assert run_main(argv_base + ["--count", "2"]) == 0
    assert len(list(keystore_dir.glob("*.json"))) == 2

    # Green case.
    assert run_main(argv_base + ["--verify"]) == 0

    # Missing keystore -> exit 1 naming the address (mod.rs:230-233 shape).
    victim = sorted(keystore_dir.glob("*.json"))[0]
    victim_addr = victim.name.rsplit("--", 1)[1].removesuffix(".json")
    hidden = victim.with_suffix(".hidden")
    victim.rename(hidden)
    caplog.clear()
    with caplog.at_level(logging.ERROR, logger="provision_wallets"):
        rc = run_main(argv_base + ["--verify"])
    assert rc == 1
    assert victim_addr.lower() in caplog.text.lower()
    assert "has no registered worker signer" in caplog.text
    hidden.rename(victim)

    # Excluded entry without keystore -> still 0.
    pool = json.loads(pool_path.read_text(encoding="utf-8"))
    pool["wallets"].append(
        {
            "address": "0x" + "d" * 40,
            "label": "excluded-01",
            "chain_balances": {"base": "0", "arbitrum": "0"},
            "last_used": 0,
            "use_count": 0,
            "excluded": True,
        }
    )
    pool_path.write_text(json.dumps(pool, indent=2) + "\n", encoding="utf-8")
    assert run_main(argv_base + ["--verify"]) == 0

    # Zero-address entry -> skipped, still 0.
    pool["wallets"].append(
        {
            "address": "0x0000000000000000000000000000000000000000",
            "label": "zero-01",
            "chain_balances": {"base": "0", "arbitrum": "0"},
            "last_used": 0,
            "use_count": 0,
            "excluded": False,
        }
    )
    pool_path.write_text(json.dumps(pool, indent=2) + "\n", encoding="utf-8")
    assert run_main(argv_base + ["--verify"]) == 0


# ---------------------------------------------------------------------------
# T-10 — treasury role never touches the pool
# ---------------------------------------------------------------------------
@pytest.mark.skipif(RealAccount is None, reason="eth_account not installed")
def test_treasury_role_never_touches_pool(
    tmp_path: Path,
    pool_copy: Path,
    env_password: str,
    fast_scrypt: int,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    treasury_path = tmp_path / "treasury-home" / "treasury.json"
    monkeypatch.setenv("CHIMERA_TREASURY_KEYSTORE", str(treasury_path))
    pool_hash_before = sha256_file(pool_copy)

    rc = run_main(["--role", "treasury", "--eoa-pool", str(pool_copy)])
    captured = capsys.readouterr()

    assert rc == 0
    assert sha256_file(pool_copy) == pool_hash_before
    created = [p for p in treasury_path.parent.rglob("*") if p.is_file()]
    assert created == [treasury_path]
    assert f"CHIMERA_TREASURY_KEYSTORE={treasury_path}" in captured.out

    data = json.loads(treasury_path.read_text(encoding="utf-8"))
    assert data["version"] == 3
    assert data["crypto"]["kdf"] == "scrypt"
