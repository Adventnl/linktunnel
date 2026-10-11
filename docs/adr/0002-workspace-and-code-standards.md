# ADR-0002: Rust workspace, lint policy, error handling, unsafe policy

**Status:** Accepted

## Context
One codebase must run as a system service, a desktop app and a server component on Windows,
macOS and Linux, and parse hostile input from the network.

## Decision
* **Cargo workspace**, edition 2024, MSRV = the pinned toolchain in `rust-toolchain.toml`.
* **Lints are code:** `[workspace.lints]` denies `unwrap`, `expect`, `panic`, `todo`,
  `dbg!`, undocumented `unsafe`; warns on lossy casts and missing docs. CI runs clippy with
  `-D warnings`. Tests may unwrap (`clippy.toml`).
* **`unsafe`:** denied workspace-wide and additionally `#![forbid(unsafe_code)]` in every crate
  that has no reason for it. FFI lives in a few isolated modules (`capture`, `input`, `power`,
  `codec` hardware backends, `session-helper`) that opt in with `#[allow(unsafe_code)]`, a
  module-level safety contract, and a test or a documented reason they cannot be tested.
* **Errors:** library crates define a `thiserror` enum; `anyhow` appears only in binaries.
  Errors crossing a trust boundary carry codes, never internal detail.
* **Secrets in logs:** secrets are wrapped in `Redacted<T>` or never formatted; logging is
  structured (`tracing`).
* **Async:** `tokio`. Capture, encode and input hot paths run on dedicated, elevated-priority OS
  threads and talk to async code through bounded channels.
* **Supply chain:** `cargo-deny` (licenses, bans incl. OpenSSL, sources, advisories) and
  `cargo-audit` in CI; SBOM generated per release.

## Consequences
Some ergonomic friction (checked casts instead of `as`); in return the panic surface of
network-facing code is auditable by grep.
