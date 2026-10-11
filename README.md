# Nexus Remote

A secure, real-time, cross-platform remote-control suite for devices **you own or are authorised
to administer**: screen, keyboard, mouse, clipboard, audio, files, terminal and power, between
Windows, macOS and Linux (desktops and headless servers), with live device presence and
Wake-on-LAN.

> **Project status:** under active construction. [docs/STATUS.md](docs/STATUS.md) is the
> honest ledger of what is implemented, what is *verified* (and on what), and what is not
> started. Nothing in this repository claims a capability that STATUS.md does not.

## Documents
| | |
|---|---|
| [Architecture](docs/ARCHITECTURE.md) | one-page overview, components, connection flow |
| [Decisions (ADRs)](docs/adr/README.md) | why each major choice was made |
| [Conventions](docs/CONVENTIONS.md) | lint policy, error handling, unsafe, logging, i18n |
| [Status](docs/STATUS.md) | per-phase ledger with verification evidence |

## Build and try it (macOS / Linux / Windows)
Requires the Rust toolchain pinned in `rust-toolchain.toml` (installed automatically by
`rustup`).

```sh
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Create and inspect this device's identity (stored in the OS keychain):
cargo run -p nexus-cli -- id create
cargo run -p nexus-cli -- id show
cargo run -p nexus-cli -- doctor
# Same, but in a throwaway directory using an owner-only key file:
cargo run -p nexus-cli -- --home /tmp/nexus-demo id create
# Chinese output:
cargo run -p nexus-cli -- --lang zh-CN id show
```

## Repository layout
```
crates/        Rust workspace members (see docs/ARCHITECTURE.md for the crate map)
apps/          UI applications (Tauri; Phase 8)
deploy/        Docker Compose, Helm, relay configuration (Phase 2+)
installers/    Per-OS packaging (Phase 10)
bench/         Latency / bandwidth harness (Phase 2+)
fuzz/          cargo-fuzz targets (Phase 9)
docs/          Architecture, ADRs, protocol, threat model, guides
```

## License
Not yet chosen; all crates are `publish = false`. Pick a license before distributing.
