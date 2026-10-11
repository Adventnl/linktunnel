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

## Phase 2: Networking and security spine: ✅ complete on one machine; ⛔ real-NAT validation

| Item | State | Evidence |
|---|---|---|
| Noise pairing: `XXpsk0` + 6-word code + 4-word SAS; Noise `X` sealed envelopes; replay guard | ✅ | 78 `nexus-crypto` tests incl. MITM-with-known-code → different SAS; known-answer values from an independent Python implementation |
| `nexus-trust`: dual-signed pairing records, revocations, local trust store, I/O-free pairing state machine | ✅ | 38 tests incl. forged/transplanted signatures, policy ceilings, stale clocks, hand-edited trust file |
| `nexus-signal` server: key-possession login (audience + nonce bound), presence (TTL, sleep/offline states), pairing registry, sealed-envelope routing, rendezvous rooms, rate limits, `/metrics` | ✅ | 32 integration tests against a real WebSocket server (forged/replayed logins, zero-trust presence, revocation, envelopes, rate limits, full two-device pairing) |
| Stores: in-memory, SQLite, PostgreSQL (pairings); in-memory, Redis (presence) behind traits | ✅ | one conformance suite passing on all backends, **including real PostgreSQL 17 and Redis 8 containers**; the suite caught a genuine concurrent-insert race in the SQL `put`, now fixed |
| `nexus-net`: QUIC over iroh, endpoint keyed by the device identity, allow-list enforced before any app byte, live-session revocation, LAN/direct/relay path classification, hello negotiation, diagnostics | ✅ | 24 tests: unpaired and impostor peers rejected, revocation kills a live session, relay-only sessions through a real local `iroh-relay` with a pinned certificate, relay→direct migration |
| Transport benchmark & ADR-0003 | ✅ loopback only | docs/benchmarks/transport-loopback.md (iroh ≈ quinn ≫ webrtc-rs on bulk); NAT/WAN performance ⛔ unmeasured |
| `nexus-agent` daemon: signaling supervisor with reconnect + pairing re-sync, presence, pairing orchestration, session acceptance, persistent lockdown switch, unpair/revocation | ✅ | 7 whole-system tests: real agents + signaling server + QUIC, driven over IPC (restart persistence; signaling-server restart recovery) |
| `nexus-ipc`: Unix socket / named pipe, private token, peer-UID check | ✅ Unix; 🟡 Windows named pipe (type-checked) | 8 tests incl. wrong token, pre-auth requests, stale sockets, file modes |
| `nexus` CLI: `status`, `devices`, `pair offer/join`, `unpair`, `ping`, `lockdown`, `stop` (en + zh-CN) | ✅ | `scripts/demo-local.sh` runs signal + two agents + CLI as separate OS processes: both screens show identical words, presence goes online, `ping` over a real QUIC session on the LAN address |
| Linux | ✅ arm64 in a container | `scripts/test-linux-container.sh` runs the whole suite on Linux (found and fixed a race that macOS timing hid) |
| Docker Compose stack (signal + PostgreSQL + Redis + Caddy) | 🟡 | image builds and starts; see "Phase 2 compose check" below |
| Threat model v1 | ✅ | docs/THREAT_MODEL.md; `scripts/check-threat-model.sh` verifies every cited test exists |
| mDNS LAN discovery | 🔲 deferred to Phase 8 | peers find each other through signaling presence hints today |
| Horizontal scaling of `nexus-signal` (cross-node push routing, shared rendezvous) | 🔲 | stores can be external; routing is per-process, so run one active replica |
| Hole punching / symmetric NAT behaviour on real networks | ⛔ no NAT test bed | iroh's implementation is relied upon; local relay-to-direct migration is tested |
| Windows runtime (agent, IPC pipe, Credential Manager store) | ⛔ no Windows machine | type-checked for `x86_64-pc-windows-msvc` for the C-free crates; CI `windows-latest` will compile and test the rest |

**Phase 2 DoD** ("two machines pair, see each other's presence, open an authenticated E2E encrypted
data channel across NAT; echo + ping-latency demo; threat model v1"): pairing, presence and the
authenticated encrypted channel with echo/latency/throughput are demonstrated end to end between two
real agent processes on this MacBook (LAN path) and between two nodes through a real relay; the
literal "across NAT between two physical machines" step needs your second device and is described in
docs/USER_GUIDE.md. Threat model v1 is written.

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
