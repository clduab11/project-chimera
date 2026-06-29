#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")"

echo "=== Chimera Foundry Test Suite ==="
echo "Working directory: $(pwd)"

# Run forge tests
forge test --root contracts/ -vvv

echo "=== All Foundry tests passed ==="
