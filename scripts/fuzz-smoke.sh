#!/usr/bin/env bash
# Runs every fuzz target for a short, fixed time (default 30 s each) and fails on any crash.
# Needs: rustup nightly + `cargo install cargo-fuzz`. Usage: scripts/fuzz-smoke.sh [seconds] [target...]
set -euo pipefail
cd "$(dirname "$0")/../fuzz"
SECS="${1:-30}"; shift || true
TARGETS=("$@")
if [ ${#TARGETS[@]} -eq 0 ]; then mapfile -t TARGETS < <(cargo +nightly fuzz list); fi
status=0
for t in "${TARGETS[@]}"; do
  printf '\n\033[1;36m== fuzz %s (%ss)\033[0m\n' "$t" "$SECS"
  if ! cargo +nightly fuzz run "$t" -- -max_total_time="$SECS" -rss_limit_mb=2048 2>&1 | tail -n 6; then status=1; fi
  [ -d "artifacts/$t" ] && [ -n "$(ls -A "artifacts/$t" 2>/dev/null)" ] && { echo "CRASH ARTIFACTS in fuzz/artifacts/$t"; status=1; }
done
exit $status
