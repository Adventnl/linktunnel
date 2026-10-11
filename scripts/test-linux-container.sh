#!/usr/bin/env bash
# Runs the whole workspace test suite on Linux (the container's native arch: arm64 on Apple
# Silicon) so Unix-only code (sockets, permissions, keyring fallback) is exercised on a real Linux
# kernel instead of only being type-checked. Cargo caches live in named volumes.
set -euo pipefail
cd "$(dirname "$0")/.."
IMAGE="${NEXUS_LINUX_IMAGE:-rust:1.96-bookworm}"
docker run --rm \
  -v "$PWD":/work -w /work \
  -v nexus-cargo-registry:/usr/local/cargo/registry \
  -v nexus-cargo-git:/usr/local/cargo/git \
  -v nexus-target-linux:/target-linux \
  -e CARGO_TARGET_DIR=/target-linux \
  -e CARGO_TERM_COLOR=never \
  "$IMAGE" \
  bash -c 'rustup toolchain install "$(sed -n "s/^channel *= *\"\(.*\)\"/\1/p" rust-toolchain.toml)" --profile minimal -c rustfmt -c clippy >/dev/null 2>&1; uname -a; rustc --version; cargo test --workspace --no-fail-fast 2>&1'
