#!/bin/bash
#
# Run the complete parity harness: Python self-tests, Rust tests, and the diff.
#

set -e

echo "════════════════════════════════════════════════════════════════"
echo "  Peacock Parity Harness — Complete Validation"
echo "════════════════════════════════════════════════════════════════"
echo

# 1. Python oracle self-tests
echo "→ Running Python oracle self-tests..."
python3 scripts/parity_reference.py --test
echo "✓ Python self-tests passed"
echo

# 2. Rust peacock-core tests
echo "→ Running peacock-core unit tests..."
cargo test -p peacock-core --quiet
echo "✓ peacock-core tests passed"
echo

# 3. Parity check
echo "→ Running parity harness..."
cargo run -p peacock-parity --quiet
echo

echo "════════════════════════════════════════════════════════════════"
echo "  ✓ All checks passed"
echo "════════════════════════════════════════════════════════════════"
