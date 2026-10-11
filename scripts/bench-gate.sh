#!/usr/bin/env bash
# Latency regression gate: benchmarks the BASE revision and the current tree on this machine and
# fails if any gated p95 regressed by more than 10 %.
#
#   scripts/bench-gate.sh [BASE_REV]        # default: origin/main
#
# Both builds run on the same machine, back to back, so runner-to-runner variance cancels out.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BASE_REV="${1:-origin/main}"
WORK="$(mktemp -d)"
trap 'git -C "$ROOT" worktree remove --force "$WORK/base" 2>/dev/null || true; rm -rf "$WORK"' EXIT

echo "== benchmarking base ($BASE_REV)"
git -C "$ROOT" worktree add --detach "$WORK/base" "$BASE_REV" >/dev/null
if [[ -d "$WORK/base/bench/latency" ]]; then
  (cd "$WORK/base" && cargo run --release -q -p nexus-bench -- all --iterations 150 --out "$WORK/base.json" >/dev/null)
else
  echo "base revision has no benchmark harness yet; nothing to compare against"
  exit 0
fi
echo "== benchmarking head"
(cd "$ROOT" && cargo run --release -q -p nexus-bench -- all --iterations 150 --out "$WORK/head.json" >/dev/null)
python3 "$ROOT/bench/compare.py" "$WORK/base.json" "$WORK/head.json"
