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
