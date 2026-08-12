#!/usr/bin/env bash
# Shim → team/evals/regression.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
exec bash "$ROOT/team/evals/regression.sh" "$@"
