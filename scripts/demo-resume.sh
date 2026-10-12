#!/usr/bin/env bash
# Session resume after a dropped link, with two real agents and ATTENDED access:
#
#   1. the controller opens a persistent terminal; a person at the target approves (once);
#   2. the controller's agent is killed with SIGKILL (no goodbye: a crash, a Wi-Fi drop);
#   3. the agent comes back and the controller re-attaches to the same terminal: the target must
#      NOT ask again (the link dropped moments after an approval), and the shell is the same one;
#   4. a deliberate disconnect must NOT leave a resume window: the next connection asks again.
#
# Needs: python3, `script` (util-linux).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug"
PORT="${RESUME_DEMO_PORT:-18483}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/nexus-resume-demo.XXXXXX")"
PIDS=()
cleanup() { for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done; wait 2>/dev/null || true; }
trap cleanup EXIT
say() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }
fail() { printf '\033[1;31mFAILED: %s\033[0m\n' "$*"; exit 1; }
command -v script >/dev/null || { echo "util-linux 'script' is required"; exit 1; }

(cd "$ROOT" && cargo build -q -p nexus-signal -p nexus-agent -p nexus-cli)
"$BIN/nexus-signal" --listen "127.0.0.1:$PORT" --audience "127.0.0.1:$PORT" >"$WORK/signal.log" 2>&1 &
PIDS+=($!)
URL="ws://127.0.0.1:$PORT/v1/ws"
SRV="$WORK/server"; CTL="$WORK/controller"
cli() { local home="$1"; shift; "$BIN/nexus" --home "$home" "$@"; }
start_agent() { # name home extra...
  local name="$1" home="$2"; shift 2
  env -u DISPLAY "$BIN/nexus-agent" --home "$home" --name "$name" --file-store --signal-url "$URL" "$@" >>"$WORK/$name.log" 2>&1 &
  echo $!
}
wait_up() { for _ in $(seq 1 100); do cli "$1" status 2>/dev/null | grep -q "Signaling .*connected (" && return 0; sleep 0.2; done; return 1; }

SRV_PID=$(start_agent Server "$SRV" --no-screen); PIDS+=("$SRV_PID")
CTL_PID=$(start_agent Controller "$CTL" --no-screen); PIDS+=("$CTL_PID")
wait_up "$SRV" && wait_up "$CTL" || fail "agents did not come up"

say "Pairing (the server stays ATTENDED: a person must approve)"
cli "$SRV" pair offer --assume-words-match >"$WORK/offer.out" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 50); do grep -q 'nexus pair join' "$WORK/offer.out" 2>/dev/null && break; sleep 0.1; done
CODE="$(sed -n 's/.*nexus pair join "\(.*\)".*/\1/p' "$WORK/offer.out" | head -n1)"
cli "$CTL" pair join "$CODE" --assume-words-match >/dev/null
sleep 2

# The "person at the server": answers y to every prompt (the audit log tells us how many there were).
( while true; do echo y; sleep 0.3; done ) | cli "$SRV" consent >"$WORK/consent.out" 2>&1 &
PIDS+=($!)
sleep 1

say "1. A persistent terminal on the server (approved once)"
SHELL_IN="$WORK/shell.in"; mkfifo "$SHELL_IN"
( sleep 2.5; printf 'echo FIRST-$$ > %s/first.txt\r' "$WORK"; sleep 4 ) >"$SHELL_IN" &
timeout 40 script -qec "stty rows 24 cols 80; exec $BIN/nexus --home $CTL shell Server --name resume" /dev/null <"$SHELL_IN" >"$WORK/shell1.out" 2>&1 &
SHELL_PID=$!
PIDS+=($SHELL_PID)
for _ in $(seq 1 60); do [ -s "$WORK/first.txt" ] && break; sleep 0.25; done
[ -s "$WORK/first.txt" ] || fail "the first terminal command did not run"
FIRST="$(cat "$WORK/first.txt")"; echo "first command ran in shell: $FIRST"
REQ1=$(cli "$SRV" audit tail 2>/dev/null | grep -c 'consent.requested' || true)
echo "approval prompts so far: $REQ1"
[ "$REQ1" = 1 ] || fail "expected exactly one approval prompt, saw $REQ1"

say "2. The controller's agent is killed (SIGKILL): the link drops with no goodbye"
kill -9 "$CTL_PID"; wait "$CTL_PID" 2>/dev/null || true
kill "$SHELL_PID" 2>/dev/null || true
sleep 1

say "3. It comes back; the same terminal is re-attached WITHOUT a new prompt"
CTL_PID=$(start_agent Controller "$CTL" --no-screen); PIDS+=("$CTL_PID")
wait_up "$CTL" || fail "the controller agent did not restart"
for _ in $(seq 1 50); do cli "$CTL" devices 2>/dev/null | grep -q "Server.*online" && break; sleep 0.3; done
SHELL_IN2="$WORK/shell2.in"; mkfifo "$SHELL_IN2"
( sleep 2.5; printf 'echo SECOND-$$ > %s/second.txt\r' "$WORK"; sleep 4 ) >"$SHELL_IN2" &
timeout 40 script -qec "stty rows 24 cols 80; exec $BIN/nexus --home $CTL shell Server --name resume" /dev/null <"$SHELL_IN2" >"$WORK/shell2.out" 2>&1 &
PIDS+=($!)
for _ in $(seq 1 60); do [ -s "$WORK/second.txt" ] && break; sleep 0.25; done
[ -s "$WORK/second.txt" ] || fail "the terminal did not come back"
echo "second command ran: $(cat "$WORK/second.txt")"
AUDIT="$(cli "$SRV" audit tail 2>/dev/null)"
REQ2=$(grep -c 'consent.requested' <<<"$AUDIT" || true)
RES=$(grep -c 'consent.resumed' <<<"$AUDIT" || true)
echo "approval prompts: $REQ2, resumed without asking: $RES"
[ "$REQ2" = 1 ] || fail "the resumed session asked again ($REQ2 prompts)"
[ "$RES" -ge 1 ] || fail "the audit log does not show the resume"

say "4. A deliberate disconnect leaves no resume window"
cli "$CTL" stop >/dev/null 2>&1 || true
sleep 2
CTL_PID=$(start_agent Controller "$CTL" --no-screen); PIDS+=("$CTL_PID")
wait_up "$CTL" || fail "restart failed"
for _ in $(seq 1 50); do cli "$CTL" devices 2>/dev/null | grep -q "Server.*online" && break; sleep 0.3; done
cli "$CTL" power Server lock --yes >/dev/null 2>&1 || true   # needs approval (the lock itself may fail here; we only count prompts)
sleep 1
REQ3=$(cli "$SRV" audit tail 2>/dev/null | grep -c 'consent.requested' || true)
echo "approval prompts after a clean stop and a new connection: $REQ3"
[ "$REQ3" -ge 2 ] || fail "a clean disconnect must not allow a silent resume"

say "Resume works for a dropped link and only for a dropped link"
