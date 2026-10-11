# Engineering conventions

These are enforced by tooling where possible (see `Cargo.toml` `[workspace.lints]`, `clippy.toml`,
`deny.toml`, CI).

## Rust
* **No `unwrap()` / `expect()` / `panic!` / `todo!` in non-test code.** Return typed errors.
  If a call is provably infallible, restructure so the type system shows it (e.g. build the
  value in a way that cannot fail) rather than adding an `#[allow]`. Tests may unwrap.
* **No `as` for narrowing integer casts.** Use `try_from` and handle the error; a length that
  does not fit is a protocol error, not a wrap-around.
* **`unsafe`**: denied by default; allowed only in the FFI modules listed in ADR-0002, each with a
  module-level safety contract and `// SAFETY:` comments on every block.
* **Errors:** `thiserror` enums in libraries, `anyhow` only in binaries (`main`, CLI commands).
* **Async:** never block inside `async` code (wrap blocking OS calls in `spawn_blocking`).
  Never hold a lock across an `.await` (`clippy::await_holding_lock` is denied).
* **Parsers are total:** every decoder returns `Result`, enforces size caps *before* allocating,
  and has a property test ("never panics on arbitrary input") plus a fuzz target.
* **Time:** latency maths uses the monotonic clock (`nexus_core::clock`); wall-clock time only
  for expiries and human-facing timestamps.
* **Secrets:** wrap in `Zeroizing`/`Redacted`; never format into logs; compare with constant-time
  equality.

## Cryptography
* Only vetted crates; **never invent primitives or protocols casually.** New cryptographic
  constructions need an ADR and a known-answer test produced by an independent implementation.
* Every signature has a purpose label (`SignContext`); every KDF has a unique context string.

## Logging
Structured `tracing`. Levels: `error` = needs a human, `warn` = degraded but handled, `info` =
lifecycle, `debug`/`trace` = diagnostics. Never log keys, tokens, clipboard or keystroke content.

## Localisation
Every user-facing string goes through `nexus_core::i18n` (Rust) or the frontend catalog (TS), in
**English and Simplified Chinese**. A parity test fails the build if a key or placeholder differs.
(clap's built-in `--help` text is English-only.)

## Commits and versions
Conventional Commits (`feat:`, `fix:`, `perf:`, `docs:`, `test:`, `chore:`, `refactor:`;
`!` or `BREAKING CHANGE:` for breaking changes). Semantic versioning for the workspace. Protocol
changes follow the evolution rules in `docs/PROTOCOL.md`.

## Cross-platform checking from one machine
```sh
rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc aarch64-unknown-linux-gnu
cargo check --workspace --target x86_64-unknown-linux-gnu
cargo check --workspace --target x86_64-pc-windows-msvc --features nexus-cli/pure-blake3
```
This type-checks platform-specific code. It does **not** run it: CI on real Windows/macOS/Linux
runners and the device lab are the authority.
