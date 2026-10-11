# Status ledger

**Legend**
✅ implemented **and verified here** by automated tests/commands run on the build machine
(macOS 26, Apple M4, arm64) ·
🟡 implemented and type-checked for other targets, **not executed** on them ·
🔲 not started ·
⛔ cannot be completed or verified from this environment (reason given)

The build machine is a single MacBook (no Windows PC, no Linux box, no second macOS device, no
GPU vendor variety, no real NAT zoo, Docker daemon not running). Anything that needs those is
marked accordingly instead of being claimed.

## Phase 1: Foundations and skeleton: ✅ complete

| Item | State | Evidence |
|---|---|---|
| Cargo workspace, pinned toolchain, lint policy, rustfmt/clippy config | ✅ | `cargo clippy --workspace --all-targets -- -D warnings` exits 0 |
| CI matrix (fmt, clippy, test on ubuntu/macos/windows, docs, cross type-check, cargo-deny, nightly cargo-audit + SBOM) | 🟡 | YAML validated locally; **not run** (no GitHub runner available here) |
| Supply-chain gates | ✅ | `cargo deny check` and `cargo audit` exit 0 (1296 advisories, 281 crates) |
| `nexus-core`: layered config (defaults → TOML → `NEXUS_*` env), strict validation | ✅ | 31 unit tests |
| `nexus-core`: paths (user/system scope), tracing init, `Redacted`, HDR latency histograms (p50/p95/p99), monotonic clock | ✅ | unit tests |
| `nexus-core`: i18n catalogs en + zh-CN with key/placeholder parity test | ✅ | unit test |
| `nexus-proto` v0: protobuf schema (common, session), version/capability negotiation with downgrade floors | ✅ | 55 tests incl. property tests |
| `nexus-proto`: length-delimited framing (pre-allocation size cap), serial-number arithmetic | ✅ | tests + "decoder is total" property test |
| `nexus-proto`: hot-path codecs: video header, audio packet, cursor update, input events (+ move/scroll coalescing) | ✅ | roundtrip + "never panics on arbitrary bytes" property tests |
| `nexus-crypto`: root-seed identity (Ed25519 + bound X25519), `DeviceId`, fingerprint, domain-separated signatures, strict verify | ✅ | 38 tests; **known-answer test cross-checked against an independent Python implementation** |
| `nexus-crypto`: secret stores: OS keychain, owner-only file (atomic create, perms/owner check, checksum), memory | ✅ macOS keychain, file store; 🟡 Windows Credential Manager, Linux Secret Service | real Keychain roundtrip test passed on this Mac (`--ignored`); Windows/Linux paths type-checked only |
| `nexus-cli`: `nexus id create/show/delete`, `nexus doctor`, `--lang zh-CN`, `--json` | ✅ | exercised end to end in a throwaway home |
| Cross-target type-check | ✅ type-check only | `cargo check --workspace` for x86_64-linux, aarch64-linux, x86_64-windows-msvc |

**Phase 1 DoD** ("CI green on win/mac/linux; keys generated and persisted; unit tests pass"):
keys generated and persisted ✅ (macOS Keychain + file store, verified); unit tests pass ✅
(124 tests); lint/audit gates ✅ locally; **"CI green on all three OSes" is unverified until the
workflows run on GitHub**: the workflows are written and the equivalent commands pass locally on
macOS and type-check for Linux/Windows.

## Phase 2: Networking and security spine: 🔲
## Phase 3: First pixels: 🔲
## Phase 4: Hardware acceleration and the latency war: 🔲
## Phase 5: Full control feature set: 🔲
## Phase 6: Windows deep control: 🔲 (runtime validation ⛔ no Windows machine)
## Phase 7: Servers: 🔲 (Linux runtime validation ⛔ no Linux machine)
## Phase 8: Presence, wake and the dashboard: 🔲
## Phase 9: Hardening and security review: 🔲
## Phase 10: Packaging, updates and polish: 🔲

## How to re-verify everything claimed above
```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check && cargo audit
cargo test -p nexus-crypto -- --ignored keyring        # touches your real OS keychain
cargo check --workspace --target x86_64-unknown-linux-gnu
cargo check --workspace --target x86_64-pc-windows-msvc --features nexus-cli/pure-blake3
```
