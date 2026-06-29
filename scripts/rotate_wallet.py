#!/usr/bin/env python3
"""
rotate_wallet.py
Project Chimera - EOA Rotation Alias (documentation-compatibility shim)

This is a THIN DELEGATOR to `scripts/rotate_eoa.py`. Operator docs and runbooks
historically reference `rotate_wallet.py`, but the implementation lives in
`rotate_eoa.py`. To keep BOTH names working, this module imports and invokes
rotate_eoa's `main()` entrypoint, passing through the process argv unchanged.

There is no logic here on purpose: all selection, cooldown, balance, and
state-update behavior is defined once in rotate_eoa.py (the single source of
truth). All CLI flags (--config, --chain, --min-eth, --cooldown-seconds,
--output-format, --dry-run, --log-level) are identical because argparse in
rotate_eoa parses sys.argv directly.

INVARIANT (AGENTS.md #5): stdlib only. rotate_eoa.py is already web3-free, so
this alias imports without web3.py and `--help` works (argparse help is rendered
by the delegated parser).

Usage (identical to rotate_eoa.py):
  python scripts/rotate_wallet.py --config config/eoa_pool.json --chain base
  python scripts/rotate_wallet.py --config config/eoa_pool.json --chain base --dry-run
"""

from __future__ import annotations

import sys
from pathlib import Path

# Ensure the sibling rotate_eoa module is importable regardless of the caller's
# working directory (e.g. invoked from repo root or from within scripts/).
_SCRIPT_DIR = Path(__file__).resolve().parent
if str(_SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPT_DIR))

from rotate_eoa import main as rotate_main  # noqa: E402  (after sys.path setup)


if __name__ == "__main__":
    # Pass through exactly: rotate_main() reads sys.argv via its own argparse,
    # and its integer return code becomes this process's exit code.
    sys.exit(rotate_main())
