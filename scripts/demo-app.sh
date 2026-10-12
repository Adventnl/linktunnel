#!/usr/bin/env bash
# Runs the Tauri desktop app against two REAL agents (a controller and a headless server paired
# through a real signaling server) on a virtual X display, takes screenshots, and drives the GUI
# with xdotool: dashboard with live data, then a terminal on the server through the GUI.
#
# Needs: Xvfb, xdotool, ImageMagick (`import`), the webkit2gtk dev packages used to build the app.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug"
APP="$ROOT/apps/nexus-app/src-tauri/target/debug/nexus-app"
PORT="${APP_DEMO_PORT:-18473}"
APPDISP="${APP_DEMO_DISPLAY:-:77}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/nexus-app-demo.XXXXXX")"
OUT="${APP_DEMO_OUT:-$WORK}"; mkdir -p "$OUT"
PIDS=()
cleanup() { for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done; wait 2>/dev/null || true; }
trap cleanup EXIT
say() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }
fail() { printf '\033[1;31mFAILED: %s\033[0m\n' "$*"; exit 1; }
for tool in Xvfb xdotool import; do command -v "$tool" >/dev/null || { echo "$tool is required"; exit 1; }; done

say "Building (agent, CLI, signaling server, frontend, app)"
(cd "$ROOT" && cargo build -q -p nexus-signal -p nexus-agent -p nexus-cli)
(cd "$ROOT/apps/nexus-app" && { [ -d node_modules ] || npm ci --no-audit --no-fund; } && npx vite build >/dev/null)
cargo build -q --manifest-path "$ROOT/apps/nexus-app/src-tauri/Cargo.toml"

"$BIN/nexus-signal" --listen "127.0.0.1:$PORT" --audience "127.0.0.1:$PORT" >"$WORK/signal.log" 2>&1 &
PIDS+=($!)
SIGNAL_URL="ws://127.0.0.1:$PORT/v1/ws"
SRV="$WORK/server"; CTL="$WORK/controller"
env -u DISPLAY "$BIN/nexus-agent" --home "$SRV" --name "Home server" --file-store --signal-url "$SIGNAL_URL" >"$WORK/server.log" 2>&1 &
PIDS+=($!)
env -u DISPLAY "$BIN/nexus-agent" --home "$CTL" --name "MacBook" --file-store --signal-url "$SIGNAL_URL" --no-screen >"$WORK/controller.log" 2>&1 &
PIDS+=($!)
cli() { local home="$1"; shift; "$BIN/nexus" --home "$home" "$@"; }
for home in "$SRV" "$CTL"; do
  for _ in $(seq 1 100); do cli "$home" status 2>/dev/null | grep -q "Signaling .*connected (" && break; sleep 0.2; done
done
say "Pairing"
cli "$SRV" pair offer --assume-words-match >"$WORK/offer.out" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 50); do grep -q 'nexus pair join' "$WORK/offer.out" 2>/dev/null && break; sleep 0.1; done
CODE="$(sed -n 's/.*nexus pair join "\(.*\)".*/\1/p' "$WORK/offer.out" | head -n1)"
cli "$CTL" pair join "$CODE" --assume-words-match >/dev/null
sleep 2
cli "$SRV" trust unattended "MacBook" >/dev/null

say "Starting the app on $APPDISP"
Xvfb "$APPDISP" -screen 0 1280x800x24 -nolisten tcp >"$WORK/xvfb.log" 2>&1 &
PIDS+=($!)
sleep 1
DISPLAY="$APPDISP" NEXUS_HOME="$CTL" WEBKIT_DISABLE_COMPOSITING_MODE=1 WEBKIT_DISABLE_DMABUF_RENDERER=1 PATH="$BIN:$PATH" \
  "$APP" >"$WORK/app.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 60); do DISPLAY="$APPDISP" xdotool search --name "Nexus Remote" >/dev/null 2>&1 && break; sleep 0.5; done
WIN="$(DISPLAY="$APPDISP" xdotool search --name "Nexus Remote" | head -n1)" || fail "the app window never appeared (see $WORK/app.log)"
DISPLAY="$APPDISP" xdotool windowmove "$WIN" 0 0 windowsize "$WIN" 1280 800
sleep 4
DISPLAY="$APPDISP" import -window root "$OUT/app-dashboard.png"
echo "dashboard screenshot: $OUT/app-dashboard.png"
if [ "${APP_DEMO_INTERACT:-1}" = 1 ]; then
  # The "Terminal" button of the first card (layout is deterministic at this window size).
  DISPLAY="$APPDISP" xdotool mousemove "${APP_DEMO_TERMINAL_X:-263}" "${APP_DEMO_TERMINAL_Y:-186}" click 1
  sleep 4
  DISPLAY="$APPDISP" xdotool type --delay 40 "echo gui-$((6*7)) > $WORK/from-gui.txt"
  DISPLAY="$APPDISP" xdotool key Return
  sleep 2
  DISPLAY="$APPDISP" import -window root "$OUT/app-terminal.png"
  echo "terminal screenshot: $OUT/app-terminal.png"
  grep -q 'gui-42' "$WORK/from-gui.txt" 2>/dev/null && echo "the GUI terminal ran a command on the server: $(cat "$WORK/from-gui.txt")" || echo "(terminal command did not land; see screenshot)"
fi
echo "logs: $WORK"
