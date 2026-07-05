"""Tests T-11..T-15 for scripts/testnet_harness.py (plan §7.2).

All tests are offline (no network, no real RPC). Test artifacts are written
only under pytest's tmp_path — never into the repo tree.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPTS_DIR = REPO_ROOT / "scripts"
HARNESS_PATH = SCRIPTS_DIR / "testnet_harness.py"
PROVISION_PATH = SCRIPTS_DIR / "provision_wallets.py"
CONFIG_TEMPLATE = REPO_ROOT / "config" / "testnet.base-sepolia.json"
POOL_TEMPLATE = REPO_ROOT / "config" / "eoa_pool.sepolia.json"
PASSWORD_ENV = "CHIMERA_KEYSTORE_PASSWORD"

sys.path.insert(0, str(SCRIPTS_DIR))
import testnet_harness as th  # noqa: E402
import fund_eoa  # noqa: E402
import check_balances  # noqa: E402


# ---------------------------------------------------------------------------
# Helpers / fixtures
# ---------------------------------------------------------------------------
def run_main(argv: list[str]) -> int:
    """Call th.main in-process, normalizing SystemExit to an exit code."""
    try:
        rc = th.main(argv)
    except SystemExit as exc:
        rc = exc.code if isinstance(exc.code, int) else 1
    return rc


def make_import_blocker(tmp_path: Path) -> Path:
    blocker = tmp_path / "import_blocker"
    blocker.mkdir()
    for name in ("web3", "eth_account"):
        (blocker / f"{name}.py").write_text(
            'raise ImportError("blocked for test")\n', encoding="utf-8"
        )
    return blocker


@pytest.fixture
def tmp_config(tmp_path: Path) -> Path:
    dest = tmp_path / "testnet.base-sepolia.json"
    shutil.copyfile(CONFIG_TEMPLATE, dest)
    return dest


@pytest.fixture
def tmp_pool(tmp_path: Path) -> Path:
    dest = tmp_path / "eoa_pool.sepolia.json"
    shutil.copyfile(POOL_TEMPLATE, dest)
    return dest


@pytest.fixture
def tmp_testnet_root(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Redirect the harness's testnet keystore root into tmp_path so the
    keystore-subtree guard passes without touching the real home dir."""
    root = tmp_path / "testnet"
    root.mkdir()
    monkeypatch.setattr(th, "TESTNET_KEYSTORE_ROOT", root)
    monkeypatch.delenv("CHIMERA_TREASURY_KEYSTORE", raising=False)
    return root


class _CallRecorder:
    """Fake Web3 client that reports the WRONG chain id (mainnet 8453) and
    records every eth-level access so tests can prove call order."""

    def __init__(self) -> None:
        self.calls: list[str] = []
        recorder = self

        class _Eth:
            @property
            def chain_id(self) -> int:
                recorder.calls.append("chain_id")
                return 8453  # Base mainnet — must trip the guard

            def get_balance(self, *args, **kwargs):
                recorder.calls.append("get_balance")
                return 0

            def get_transaction_count(self, *args, **kwargs):
                recorder.calls.append("get_transaction_count")
                return 0

            @property
            def account(self):
                recorder.calls.append("account")
                raise AssertionError("account accessed after chain guard")

        self.eth = _Eth()

    def is_connected(self) -> bool:
        self.calls.append("is_connected")
        return True


# ---------------------------------------------------------------------------
# T-11 — chain guard blocks the wrong chain before any account/balance call
# ---------------------------------------------------------------------------
def test_chain_guard_blocks_wrong_chain(
    tmp_config: Path,
    tmp_pool: Path,
    tmp_testnet_root: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    recorder = _CallRecorder()
    monkeypatch.setattr(th, "_build_web3", lambda rpc: recorder)

    # Spies: the delegated workers must never be reached.
    def _fund_sentinel(**kwargs):
        raise AssertionError("fund_eoa.fund_wallets called despite chain guard")

    def _collect_sentinel(*args, **kwargs):
        raise AssertionError("collect_balances called despite chain guard")

    monkeypatch.setattr(fund_eoa, "fund_wallets", _fund_sentinel)
    monkeypatch.setattr(check_balances, "collect_balances", _collect_sentinel)
    monkeypatch.delenv(PASSWORD_ENV, raising=False)

    base = ["--config", str(tmp_config), "--eoa-pool", str(tmp_pool)]

    # fund: exit 3, nothing but the chain check touched the fake w3.
    rc = run_main(["fund", *base])
    assert rc == 3
    assert "chain_id" in recorder.calls
    forbidden = {"get_balance", "get_transaction_count", "account"}
    assert forbidden.isdisjoint(recorder.calls), recorder.calls
    # The chain check is the FIRST eth-level access.
    eth_calls = [c for c in recorder.calls if c != "is_connected"]
    assert eth_calls[0] == "chain_id"

    # status: same guard, same exit code, same call discipline.
    recorder.calls.clear()
    rc = run_main(["status", *base])
    assert rc == 3
    assert recorder.calls and recorder.calls[-1] == "chain_id"
    assert forbidden.isdisjoint(recorder.calls), recorder.calls


# ---------------------------------------------------------------------------
# T-12 — no live-flip path exists in either new script (static source scan)
# ---------------------------------------------------------------------------
def test_no_live_flip_path_exists() -> None:
    mode_re = re.compile(
        r"""(?ix)
        (execute_mode | CHIMERA_EXECUTE_MODE)      # a mode variable name
        ["']?\s*[:=,]\s*["']?                      # assignment / kv separator
        ([A-Za-z_]+)                               # the assigned value
        """
    )
    for script in (HARNESS_PATH, PROVISION_PATH):
        source = script.read_text(encoding="utf-8")
        for match in mode_re.finditer(source):
            assert match.group(2).lower() == "shadow", (
                f"{script.name}: mode variable assigned "
                f"{match.group(2)!r} (only 'shadow' is permitted)"
            )
        assert "toggle_shadow" not in source, f"{script.name} references toggle_shadow"
        assert "--set-live" not in source, f"{script.name} contains --set-live"


# ---------------------------------------------------------------------------
# T-13 — plan and shadow-env run offline (web3/eth_account import-blocked)
# ---------------------------------------------------------------------------
def test_plan_and_shadow_env_offline(tmp_path: Path) -> None:
    blocker = make_import_blocker(tmp_path)
    env = os.environ.copy()
    env["PYTHONPATH"] = str(blocker) + os.pathsep + env.get("PYTHONPATH", "")
    env.pop(PASSWORD_ENV, None)

    plan = subprocess.run(
        [sys.executable, str(HARNESS_PATH), "plan"],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(REPO_ROOT),
        timeout=60,
    )
    assert plan.returncode == 0, plan.stderr

    shadow = subprocess.run(
        [sys.executable, str(HARNESS_PATH), "shadow-env"],
        capture_output=True,
        text=True,
        env=env,
        cwd=str(REPO_ROOT),
        timeout=60,
    )
    assert shadow.returncode == 0, shadow.stderr
    out = shadow.stdout
    assert "CHIMERA_CHAIN_ID=84532" in out
    assert "CHIMERA_EXECUTE_MODE=shadow" in out
    assert "eoa_pool.sepolia.json" in out
    # The password variable must never be emitted with a value: any
    # "CHIMERA_KEYSTORE_PASSWORD=" followed by a non-space char is a leak.
    assert re.search(r"CHIMERA_KEYSTORE_PASSWORD=\S", out) is None
    # It IS mentioned, but only as an operator instruction (comment form).
    for line in out.splitlines():
        if "CHIMERA_KEYSTORE_PASSWORD" in line:
            assert line.lstrip().startswith("#"), f"non-comment password line: {line!r}"


# ---------------------------------------------------------------------------
# T-14 — fund refuses the mainnet pool before any network attempt
# ---------------------------------------------------------------------------
def test_fund_refuses_mainnet_pool(
    tmp_config: Path,
    tmp_testnet_root: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def _no_network(rpc):
        raise AssertionError("web3 construction attempted despite pool guard")

    monkeypatch.setattr(th, "_build_web3", _no_network)
    monkeypatch.delenv(PASSWORD_ENV, raising=False)

    rc = run_main(
        ["fund", "--config", str(tmp_config), "--eoa-pool", "config/eoa_pool.json"]
    )
    assert rc == 3

    # Same refusal for the absolute mainnet-pool path.
    rc = run_main(
        [
            "fund",
            "--config",
            str(tmp_config),
            "--eoa-pool",
            str(REPO_ROOT / "config" / "eoa_pool.json"),
        ]
    )
    assert rc == 3


# ---------------------------------------------------------------------------
# T-15 — fund delegates to fund_eoa.fund_wallets (reuse, never fork)
# ---------------------------------------------------------------------------
def test_fund_delegates_to_fund_eoa(
    tmp_config: Path,
    tmp_pool: Path,
    tmp_testnet_root: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    in_memory_key = "0x" + "11" * 32
    config = json.loads(tmp_config.read_text(encoding="utf-8"))

    calls: list[dict] = []

    def fake_fund_wallets(**kwargs):
        calls.append(kwargs)
        return 3

    monkeypatch.setattr(fund_eoa, "fund_wallets", fake_fund_wallets)
    monkeypatch.setattr(
        th, "decrypt_treasury_in_memory", lambda path, password: in_memory_key
    )
    monkeypatch.setattr(th, "make_guarded_web3", lambda rpc, chain_id: object())
    monkeypatch.setenv(PASSWORD_ENV, "unit-test-password-123")
    # cmd_fund gates on web3 availability before the (patched) guard.
    monkeypatch.setattr(th, "Web3", object())

    rc = run_main(
        ["fund", "--dry-run", "--config", str(tmp_config), "--eoa-pool", str(tmp_pool)]
    )
    assert rc == 0

    assert len(calls) == 1, "fund_wallets must be called exactly once"
    kwargs = calls[0]
    assert kwargs["treasury_key"] == in_memory_key
    assert kwargs["rpc"] == config["rpc_url"]
    assert Path(kwargs["eoa_pool_path"]).resolve() == tmp_pool.resolve()
    assert kwargs["min_balance"] == config["funding"]["min_worker_balance_eth"]
    assert kwargs["fund_amount"] == config["funding"]["worker_topup_eth"]
    assert kwargs["wait"] is True
    assert kwargs["dry_run"] is True

    # Reuse-not-fork: the harness contains no transaction-dispatch call of its
    # own — the send loop lives exclusively in fund_eoa.py.
    source = HARNESS_PATH.read_text(encoding="utf-8")
    assert "send_raw_transaction" not in source
    assert "sign_transaction" not in source
