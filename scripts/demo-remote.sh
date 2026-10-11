#!/usr/bin/env bash
# Phase 3 demo on Linux: view and control a real (virtual) X desktop through two real agents.
#
#   Xvfb :N            the "Windows PC"'s screen (a plain virtual X server)
#   nexus-agent (pc)   watches that screen, injects input with XTest
#   nexus-agent (mac)  the controller's agent
#   nexus remote ...   the CLI viewer: screenshot, key, click, type
#
# Needs: Xvfb. xterm/xeyes are used if installed to make the screen interesting.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug"
PORT="${REMOTE_DEMO_PORT:-18453}"
DISP="${REMOTE_DEMO_DISPLAY:-:93}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/nexus-remote-demo.XXXXXX")"
OUT="${REMOTE_DEMO_OUT:-$WORK}"
PIDS=()
cleanup() { for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done; wait 2>/dev/null || true; }
trap cleanup EXIT
say() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }

command -v Xvfb >/dev/null || { echo "Xvfb is required for this demo"; exit 1; }
say "Building"
(cd "$ROOT" && cargo build -q -p nexus-signal -p nexus-agent -p nexus-cli)

say "Starting a virtual desktop on $DISP"
Xvfb "$DISP" -screen 0 1024x640x24 -nolisten tcp >"$WORK/xvfb.log" 2>&1 &
PIDS+=($!)
sleep 1
for app in xclock xeyes xterm; do
  if command -v "$app" >/dev/null; then LC_ALL=C.UTF-8 DISPLAY="$DISP" "$app" >/dev/null 2>&1 & PIDS+=($!); fi
done

"$BIN/nexus-signal" --listen "127.0.0.1:$PORT" --audience "127.0.0.1:$PORT" >"$WORK/signal.log" 2>&1 &
PIDS+=($!)
SIGNAL_URL="ws://127.0.0.1:$PORT/v1/ws"

A="$WORK/mac"; B="$WORK/pc"
DISPLAY="$DISP" "$BIN/nexus-agent" --home "$B" --name "Windows PC" --file-store --signal-url "$SIGNAL_URL" >"$WORK/pc.log" 2>&1 &
PIDS+=($!)
env -u DISPLAY "$BIN/nexus-agent" --home "$A" --name "MacBook" --file-store --signal-url "$SIGNAL_URL" --no-screen >"$WORK/mac.log" 2>&1 &
PIDS+=($!)
cli() { local home="$1"; shift; "$BIN/nexus" --home "$home" "$@"; }
for home in "$A" "$B"; do
  for _ in $(seq 1 100); do cli "$home" status 2>/dev/null | grep -q "Signaling .*connected (" && break; sleep 0.2; done
done

say "Pairing (the PC lets the Mac see and control it)"
cli "$B" pair offer --assume-words-match >"$WORK/offer.out" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 50); do grep -q 'nexus pair join' "$WORK/offer.out" 2>/dev/null && break; sleep 0.1; done
CODE="$(sed -n 's/.*nexus pair join "\(.*\)".*/\1/p' "$WORK/offer.out" | head -n1)"
cli "$A" pair join "$CODE" --assume-words-match
sleep 2

say "The demo PC has nobody at it, so it pre-authorises the Mac (otherwise it would ask first)"
cli "$B" trust unattended "MacBook"

say "Monitors on the PC, as the Mac sees them"
cli "$A" remote monitors "Windows PC"

say "Screenshot of the PC's screen"
cli "$A" remote screenshot "Windows PC" -o "$OUT/before.png" --cursor

say "Click at (300,200) and type text on the PC"
cli "$A" remote click "Windows PC" 300 200
cli "$A" remote type "Windows PC" "hello from the Mac — héllo ✓"
cli "$A" remote key "Windows PC" "ctrl+a"
sleep 1
cli "$A" remote screenshot "Windows PC" -o "$OUT/after.png" --cursor
echo; echo "Screenshots written to $OUT (before.png, after.png)"
ls -la "$OUT"/*.png

if command -v ffmpeg >/dev/null; then
  say "Live viewer window (on a second virtual display), driving the PC with the mouse"
  VDISP="${REMOTE_DEMO_VIEWER_DISPLAY:-:94}"
  Xvfb "$VDISP" -screen 0 1280x800x24 -nolisten tcp >"$WORK/xvfb2.log" 2>&1 &
  PIDS+=($!)
  sleep 1
  DISPLAY="$VDISP" cli "$A" remote view "Windows PC" >"$WORK/view.log" 2>&1 &
  PIDS+=($!)
  sleep 4
  ffmpeg -loglevel error -f x11grab -video_size 1280x800 -i "$VDISP" -frames:v 1 -y "$OUT/viewer-window.png"
  echo "viewer window captured to $OUT/viewer-window.png"
fi
cli "$A" stop; cli "$B" stop
