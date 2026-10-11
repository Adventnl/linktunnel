#!/usr/bin/env bash
# Runs the whole Phase 2 system as separate OS processes on one machine:
#   nexus-signal  (signaling/presence/pairing registry)
#   nexus-agent   x2 (two "devices" with separate identities and state)
#   nexus         (the CLI, talking to each agent over local IPC)
#
# Pairing here uses --assume-words-match because one script plays both humans; on real devices
# you compare the four words yourself (that human check is what defeats a man-in-the-middle).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug"
PORT="${NEXUS_DEMO_PORT:-18443}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/nexus-demo.XXXXXX")"
PIDS=()

cleanup() {
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

say() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }

say "Building"
(cd "$ROOT" && cargo build -q -p nexus-signal -p nexus-agent -p nexus-cli)

say "Starting the signaling server on 127.0.0.1:$PORT"
"$BIN/nexus-signal" --listen "127.0.0.1:$PORT" --audience "127.0.0.1:$PORT" >"$WORK/signal.log" 2>&1 &
PIDS+=($!)

A="$WORK/mac"; B="$WORK/pc"
for pair in "mac:MacBook" "pc:Windows PC"; do
  dir="${pair%%:*}"; name="${pair#*:}"
  say "Starting agent '$name'"
  "$BIN/nexus-agent" --home "$WORK/$dir" --name "$name" --file-store \
      --signal-url "ws://127.0.0.1:$PORT/v1/ws" >"$WORK/$dir.log" 2>&1 &
  PIDS+=($!)
done
sleep 2
cli() { local home="$1"; shift; "$BIN/nexus" --home "$home" "$@"; }

say "Agent status (MacBook)"
cli "$A" status

say "The PC shows a pairing code; the Mac enters it"
cli "$B" pair offer --assume-words-match >"$WORK/offer.out" 2>&1 &
OFFER_PID=$!
PIDS+=($OFFER_PID)
for _ in $(seq 1 50); do grep -q 'nexus pair join' "$WORK/offer.out" 2>/dev/null && break; sleep 0.1; done
CODE="$(sed -n 's/.*nexus pair join "\(.*\)".*/\1/p' "$WORK/offer.out" | head -n1)"
echo "code shown on the PC: $CODE"
cli "$A" pair join "$CODE" --assume-words-match
wait "$OFFER_PID" || true
echo "--- what the PC saw:"; cat "$WORK/offer.out"

say "Paired devices as the Mac sees them"
sleep 1
cli "$A" devices
say "...and as the PC sees them"
cli "$B" devices

say "Latency / throughput from the Mac to the PC over the encrypted QUIC session"
cli "$A" ping "Windows PC" --bulk-mb 64

say "Same output in Simplified Chinese"
cli "$A" --lang zh-CN devices

say "The PC's owner unpairs the Mac; the Mac loses it immediately"
cli "$B" unpair MacBook
sleep 1
cli "$A" devices
echo "(ping after unpair:)"; cli "$A" ping "Windows PC" || echo "refused, as expected"

say "Stopping"
cli "$A" stop; cli "$B" stop
sleep 1
