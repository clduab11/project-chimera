#!/usr/bin/env bash
# run_forge.sh — clean entrypoint to be executed from WSL
set -euo pipefail

export PATH="$HOME/.foundry/bin:$PATH"

cd "$(dirname "$0")"

echo "=== CLEAN ==="
rm -rf out cache 2>/dev/null || true

echo "=== BUILD ==="
forge build --root .

echo "=== TEST ==="
forge test --root . 2>&1

echo "=== DONE ==="
