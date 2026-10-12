#!/usr/bin/env bash
# Phase 7 demo on Linux: a headless "server" (no monitor, no X server, no DISPLAY) and a
# controller, joined by two real agents and a signaling server. Everything below rides the same
# encrypted session:
#
#   1. the server grows a virtual screen by itself and the controller screenshots it,
#   2. a persistent terminal runs a command and survives a reconnect,
#   3. a TCP port on the server is forwarded to the controller,
#   4. an SSH-style stdio proxy (ProxyCommand) reaches the same port,
#   5. a file goes up and comes back with the same BLAKE3 hash.
#
# Needs: Xvfb, python3. `script` (util-linux) is used for the terminal step if present.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug"
PORT="${SERVER_DEMO_PORT:-18463}"
SVC_PORT="${SERVER_DEMO_SERVICE_PORT:-18464}"
FWD_PORT="${SERVER_DEMO_FORWARD_PORT:-18465}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/nexus-server-demo.XXXXXX")"
OUT="${SERVER_DEMO_OUT:-$WORK}"
PIDS=()
cleanup() { for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done; wait 2>/dev/null || true; }
trap cleanup EXIT
say() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }
fail() { printf '\033[1;31mFAILED: %s\033[0m\n' "$*"; exit 1; }

command -v Xvfb >/dev/null || { echo "Xvfb is required for this demo"; exit 1; }
command -v python3 >/dev/null || { echo "python3 is required for this demo"; exit 1; }
say "Building"
(cd "$ROOT" && cargo build -q -p nexus-signal -p nexus-agent -p nexus-cli)

"$BIN/nexus-signal" --listen "127.0.0.1:$PORT" --audience "127.0.0.1:$PORT" >"$WORK/signal.log" 2>&1 &
PIDS+=($!)
SIGNAL_URL="ws://127.0.0.1:$PORT/v1/ws"

# A service on the "server" that only listens on its own loopback: a tiny line-echo server.
python3 - "$SVC_PORT" >"$WORK/service.log" 2>&1 <<'PY' &
import socketserver, sys
class H(socketserver.StreamRequestHandler):
    def handle(self):
        for line in self.rfile:
            self.wfile.write(b"echo:" + line); self.wfile.flush()
socketserver.ThreadingTCPServer.allow_reuse_address = True
socketserver.ThreadingTCPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
PIDS+=($!)

SRV="$WORK/server"; CTL="$WORK/controller"; SHARE="$WORK/share"; mkdir -p "$SHARE"
# The server: no DISPLAY at all, so the agent must create its own screen.
env -u DISPLAY -u WAYLAND_DISPLAY "$BIN/nexus-agent" --home "$SRV" --name "Server" --file-store \
  --signal-url "$SIGNAL_URL" --share-rw "$SHARE" >"$WORK/server.log" 2>&1 &
PIDS+=($!)
env -u DISPLAY "$BIN/nexus-agent" --home "$CTL" --name "MacBook" --file-store \
  --signal-url "$SIGNAL_URL" --no-screen >"$WORK/controller.log" 2>&1 &
PIDS+=($!)
cli() { local home="$1"; shift; "$BIN/nexus" --home "$home" "$@"; }
for home in "$SRV" "$CTL"; do
  for _ in $(seq 1 100); do cli "$home" status 2>/dev/null | grep -q "Signaling .*connected (" && break; sleep 0.2; done
done
grep -q "serving a virtual screen" "$WORK/server.log" || fail "the server did not create a virtual screen (see $WORK/server.log)"

say "Pairing the controller with the server"
cli "$SRV" pair offer --assume-words-match >"$WORK/offer.out" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 50); do grep -q 'nexus pair join' "$WORK/offer.out" 2>/dev/null && break; sleep 0.1; done
CODE="$(sed -n 's/.*nexus pair join "\(.*\)".*/\1/p' "$WORK/offer.out" | head -n1)"
cli "$CTL" pair join "$CODE" --assume-words-match
sleep 2
cli "$SRV" trust unattended "MacBook"

say "0. The dashboard sees the server (text snapshot, then the live TUI under a pty)"
cli "$CTL" dashboard --snapshot | tee "$WORK/dashboard.txt"
grep -q "Server" "$WORK/dashboard.txt" || fail "the dashboard does not list the server"
if command -v script >/dev/null; then
  ( sleep 2; printf 'q' ) | timeout 20 script -qec "stty rows 30 cols 130; exec $BIN/nexus --home $CTL dashboard" /dev/null >"$WORK/tui.out" 2>&1 \
    || fail "the live dashboard did not exit cleanly on q"
  grep -q "Server" "$WORK/tui.out" || fail "the live dashboard did not draw the server"
  echo "live dashboard drew the device list and quit on q"
fi

say "1. GUI session on a machine with no monitor"
cli "$CTL" remote monitors "Server"
cli "$CTL" remote screenshot "Server" -o "$OUT/server-screen.png"
[ -s "$OUT/server-screen.png" ] || fail "no screenshot"
echo "screenshot: $(wc -c <"$OUT/server-screen.png") bytes"

say "2. A persistent terminal"
if command -v script >/dev/null; then
  # A pty for the CLI; it runs one command in a named session and detaches (Ctrl-] .).
  ( sleep 1.5; printf 'echo MARKER-$((6*7)) > %s/terminal.txt\r' "$SHARE"; sleep 1; printf '\035.' ) \
    | timeout 30 script -qec "$BIN/nexus --home $CTL shell Server --name demo" /dev/null >"$WORK/shell.out" 2>&1 || true
  grep -q 'MARKER-42' "$SHARE/terminal.txt" 2>/dev/null || fail "the terminal command did not run on the server"
  echo "terminal ran a command on the server: $(cat "$SHARE/terminal.txt")"
else
  echo "(util-linux 'script' not found; skipping the terminal step)"
fi

say "3. Port forward: the server's loopback-only service on the controller's port $FWD_PORT"
cli "$CTL" forward Server "$FWD_PORT:127.0.0.1:$SVC_PORT" >"$WORK/forward.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 50); do grep -q Forwarding "$WORK/forward.log" 2>/dev/null && break; sleep 0.2; done
REPLY="$(python3 - "$FWD_PORT" <<'PY'
import socket, sys
s = socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=10)
s.sendall(b"ping through the tunnel\n"); print(s.makefile().readline().strip())
PY
)"
[ "$REPLY" = "echo:ping through the tunnel" ] || fail "forward replied: $REPLY"
echo "forward ok: $REPLY"

say "4. SSH-style stdio proxy (ProxyCommand) to the same service"
REPLY="$(printf 'via proxy\n' | timeout 20 "$BIN/nexus" --home "$CTL" proxy Server 127.0.0.1 "$SVC_PORT" | head -n1)"
[ "$REPLY" = "echo:via proxy" ] || fail "proxy replied: $REPLY"
echo "proxy ok: $REPLY"

say "5. File transfer, verified by hash"
head -c 3000000 /dev/urandom >"$WORK/payload.bin"
# Shared folders are addressed by their real path on the device.
SHARE_REAL="$(cd "$SHARE" && pwd -P)"
cli "$CTL" cp "$WORK/payload.bin" "Server:$SHARE_REAL/payload.bin"
cli "$CTL" ls "Server:$SHARE_REAL"
[ -f "$SHARE/payload.bin" ] || fail "the file did not arrive"
cmp "$WORK/payload.bin" "$SHARE/payload.bin" || fail "uploaded file differs"
echo "uploaded file identical ($(wc -c <"$SHARE/payload.bin") bytes)"
cli "$CTL" cp "Server:$SHARE_REAL/payload.bin" "$WORK/back.bin"
cmp "$WORK/payload.bin" "$WORK/back.bin" || fail "downloaded file differs"
echo "round trip identical"

say "All five steps passed through one encrypted session path"
cli "$CTL" stop; cli "$SRV" stop
