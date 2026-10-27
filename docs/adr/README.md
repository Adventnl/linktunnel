# Architecture Decision Records

One file per decision that is costly to reverse. Format: context → decision → consequences →
alternatives. Status is one of **Proposed**, **Accepted**, **Superseded by NNNN**.
An ADR that depends on a measurement says so and links the result.

| # | Decision | Status |
|---|---|---|
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-workspace-and-code-standards.md) | Rust workspace, lint policy, error handling, unsafe policy | Accepted |
| [0003](0003-transport-quic-via-iroh.md) | Transport: QUIC via iroh (vs WebRTC, vs raw quinn) | Accepted; benchmark appendix pending |
| [0004](0004-identity-and-key-storage.md) | Device identity and key storage | Accepted |
| [0005](0005-pairing-and-trust-model.md) | Pairing, trust, revocation | Accepted |
| [0006](0006-session-security-and-consent.md) | Session security, consent, step-up authentication | Accepted |
| [0007](0007-signaling-presence-and-wake.md) | Signaling, presence, wake | Accepted |
| [0008](0008-wire-protocol-and-stream-mapping.md) | Wire protocol and QUIC stream mapping | Accepted |
| [0009](0009-media-pipeline-and-codecs.md) | Media pipeline, codecs, licensing | Accepted |
| [0010](0010-congestion-control-and-quality-ladder.md) | Congestion control and quality ladder | Proposed |
| [0011](0011-input-injection-and-privilege-separation.md) | Input injection and privilege separation | Accepted |
| [0012](0012-ui-shell-tauri-and-native-viewport.md) | UI shell: Tauri + native wgpu viewport | Accepted |
| [0013](0013-audit-log.md) | Audit log | Accepted |
| [0014](0014-updates-and-release-signing.md) | Updates and release signing | Accepted |
| [0015](0015-audio-streaming.md) | Audio streaming (Opus, datagrams, jitter buffer) | Accepted |
| [0016](0016-virtual-display-for-headless-servers.md) | Virtual display for headless Linux servers | Accepted |
| [0017](0017-lan-discovery-and-privacy.md) | LAN discovery (mDNS) without leaking presence | Accepted |
| [0018](0018-windows-service-and-secure-desktop.md) | Windows service and secure-desktop control | Accepted (unrun on Windows) |
| [0019](0019-desktop-app-over-agent-ipc.md) | Desktop app as a thin client of the agent | Accepted |

* [0020 — Session-bound WebAuthn/TOTP step-up](0020-session-stepup.md)
* [0021 — GPU presentation and optional hardware encoders](0021-gpu-presentation-and-encoders.md)
* [0022 — Physical-screen privacy leases](0022-physical-screen-privacy.md)
* [0023 — Standalone relay](0023-standalone-relay.md)

* [0024 — Windows control completion](0024-windows-control-completion.md) (source-only, untested)
* [0025 — Android Codex companion](0025-android-codex-companion.md) (APK/host checks passed; device acceptance pending)
