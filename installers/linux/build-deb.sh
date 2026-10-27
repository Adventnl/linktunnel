#!/usr/bin/env bash
# Builds .deb packages from a release build: `nexus-remote` (agent, CLI, systemd units) and
# `nexus-signal` (self-hosted signaling server).
#
#   installers/linux/build-deb.sh [--profile dist|release] [--arch amd64|arm64]
#
# Needs: cargo, dpkg-deb. Output: target/deb/*.deb
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROFILE=dist; ARCH="$(dpkg --print-architecture 2>/dev/null || echo amd64)"
while [ $# -gt 0 ]; do case "$1" in --profile) PROFILE="$2"; shift 2;; --arch) ARCH="$2"; shift 2;; *) echo "unknown option $1" >&2; exit 2;; esac; done
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)"
OUT="$ROOT/target/deb"; STAGE="$OUT/stage"
rm -rf "$STAGE"; mkdir -p "$STAGE" "$OUT"

echo "== building ($PROFILE)"
(cd "$ROOT" && cargo build --locked --profile "$PROFILE" -p nexus-agent -p nexus-cli -p nexus-signal -p nexus-update)
BIN="$ROOT/target/$([ "$PROFILE" = dev ] && echo debug || echo "$PROFILE")"

pkg() { # name, description, depends, recommends
  local dir="$STAGE/$1"; mkdir -p "$dir/DEBIAN"
  cat > "$dir/DEBIAN/control" <<CTRL
Package: $1
Version: $VERSION
Section: net
Priority: optional
Architecture: $ARCH
Maintainer: linktunnel contributors <noreply@example.invalid>
Depends: $3
Recommends: $4
Description: $2
CTRL
  echo "$dir"
}

# ---- nexus-remote ---------------------------------------------------------------------------
D="$(pkg nexus-remote "Secure remote control for your own devices (agent and CLI)" "libc6, libasound2t64 | libasound2" "xvfb, xauth")"
install -Dm755 "$BIN/nexus-agent" "$D/usr/bin/nexus-agent"
install -Dm755 "$BIN/nexus" "$D/usr/bin/nexus"
install -Dm644 "$ROOT/apps/nexus-app/src-tauri/icons/128x128@2x.png" "$D/usr/share/icons/hicolor/256x256/apps/linktunnel.png"
install -Dm755 "$BIN/nexus-release" "$D/usr/bin/nexus-release" 2>/dev/null || true
for u in nexus-agent.service nexus-agent-user.service nexus-wake.service; do
  install -Dm644 "$ROOT/installers/linux/systemd/$u" "$D/usr/lib/systemd/system/$u"
done
# The per-user unit belongs in the user unit directory.
mkdir -p "$D/usr/lib/systemd/user"; mv "$D/usr/lib/systemd/system/nexus-agent-user.service" "$D/usr/lib/systemd/user/nexus-agent-user.service"
install -Dm644 "$ROOT/installers/linux/systemd/nexus-agent.service.d/admin-terminal.conf.example" \
  "$D/usr/share/doc/nexus-remote/admin-terminal.conf.example"
install -Dm644 "$ROOT/installers/linux/sysusers.conf" "$D/usr/lib/sysusers.d/nexus.conf"
install -Dm644 "$ROOT/README.md" "$D/usr/share/doc/nexus-remote/README.md"
for f in USER_GUIDE SELF_HOSTING THREAT_MODEL LIMITATIONS; do
  [ -f "$ROOT/docs/$f.md" ] && install -Dm644 "$ROOT/docs/$f.md" "$D/usr/share/doc/nexus-remote/$f.md"
done
cat > "$D/DEBIAN/postinst" <<'SH'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
  systemd-sysusers /usr/lib/sysusers.d/nexus.conf 2>/dev/null || \
    adduser --system --group --home /var/lib/nexus --no-create-home --shell /usr/sbin/nologin nexus 2>/dev/null || true
  # Installed but not started: the service needs pairing first (see the user guide).
  systemctl daemon-reload >/dev/null 2>&1 || true
fi
SH
cat > "$D/DEBIAN/prerm" <<'SH'
#!/bin/sh
set -e
if [ "$1" = remove ] || [ "$1" = upgrade ]; then
  systemctl stop nexus-agent.service nexus-wake.service >/dev/null 2>&1 || true
fi
SH
chmod 755 "$D/DEBIAN/postinst" "$D/DEBIAN/prerm"

# ---- nexus-signal ---------------------------------------------------------------------------
S="$(pkg nexus-signal "Self-hosted signaling server for linktunnel" "libc6" "")"
install -Dm755 "$BIN/nexus-signal" "$S/usr/bin/nexus-signal"
install -Dm644 "$ROOT/installers/linux/systemd/nexus-signal.service" "$S/usr/lib/systemd/system/nexus-signal.service"
install -Dm644 "$ROOT/docs/SELF_HOSTING.md" "$S/usr/share/doc/nexus-signal/SELF_HOSTING.md"

for p in nexus-remote nexus-signal; do
  # Installed size, and reproducible ownership/timestamps.
  find "$STAGE/$p" -exec touch -h -d "@${SOURCE_DATE_EPOCH:-0}" {} +
  dpkg-deb --root-owner-group --build "$STAGE/$p" "$OUT/${p}_${VERSION}_${ARCH}.deb"
done
ls -la "$OUT"/*.deb
