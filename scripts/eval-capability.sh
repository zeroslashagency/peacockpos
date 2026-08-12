#!/usr/bin/env bash
# Shim → team/evals/capability.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
exec bash "$ROOT/team/evals/capability.sh" "$@"
