#!/usr/bin/env bash
# Builds, signs, notarizes and staples "Nexus Remote.pkg" (universal: Apple Silicon + Intel).
#
# Needs a Mac with Xcode command-line tools and, for distribution, an Apple Developer account:
#   APP_SIGN_ID    "Developer ID Application: Your Name (TEAMID)"
#   PKG_SIGN_ID    "Developer ID Installer: Your Name (TEAMID)"
#   NOTARY_PROFILE a `xcrun notarytool store-credentials` profile name
# Without them the package is built UNSIGNED, which is fine for your own machines
# (right-click → Open) but cannot be notarized.
#
# NOT RUN ON A MAC YET: see docs/STATUS.md. Expect to adjust paths on first use.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HERE="$ROOT/installers/macos"
OUT="$ROOT/target/macos"; APP="$OUT/Nexus Remote.app"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)"
rm -rf "$OUT"; mkdir -p "$APP/Contents/MacOS" "$OUT/root/Library/LaunchAgents" "$OUT/root/Library/LaunchDaemons" "$OUT/root/usr/local/bin"

for T in aarch64-apple-darwin x86_64-apple-darwin; do
  rustup target add "$T" >/dev/null
  (cd "$ROOT" && cargo build --locked --profile dist --target "$T" -p nexus-agent -p nexus-cli)
done
for B in nexus-agent nexus; do
  lipo -create -output "$APP/Contents/MacOS/$B" \
    "$ROOT/target/aarch64-apple-darwin/dist/$B" "$ROOT/target/x86_64-apple-darwin/dist/$B"
done
sed "s/@VERSION@/$VERSION/g" "$HERE/Info.plist" > "$APP/Contents/Info.plist"
mkdir -p "$OUT/root/Applications"; mv "$APP" "$OUT/root/Applications/"
APP="$OUT/root/Applications/Nexus Remote.app"
ln -sf "/Applications/Nexus Remote.app/Contents/MacOS/nexus" "$OUT/root/usr/local/bin/nexus"
install -m644 "$HERE/app.nexus-remote.agent.plist" "$OUT/root/Library/LaunchAgents/"
install -m644 "$HERE/app.nexus-remote.wake.plist" "$OUT/root/Library/LaunchDaemons/"

if [ -n "${APP_SIGN_ID:-}" ]; then
  for B in nexus nexus-agent; do
    codesign --force --options runtime --timestamp --entitlements "$HERE/entitlements.plist" \
      --sign "$APP_SIGN_ID" "$APP/Contents/MacOS/$B"
  done
  codesign --force --options runtime --timestamp --entitlements "$HERE/entitlements.plist" --sign "$APP_SIGN_ID" "$APP"
  codesign --verify --strict --verbose=2 "$APP"
fi

PKG="$OUT/Nexus-Remote-$VERSION.pkg"
pkgbuild --root "$OUT/root" --identifier app.nexus-remote.pkg --version "$VERSION" \
  --scripts "$HERE/scripts" ${PKG_SIGN_ID:+--sign "$PKG_SIGN_ID"} "$PKG"

if [ -n "${PKG_SIGN_ID:-}" ] && [ -n "${NOTARY_PROFILE:-}" ]; then
  xcrun notarytool submit "$PKG" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$PKG"
  spctl --assess --type install -vv "$PKG"
fi
echo "built $PKG"
