# ADR-0003: Transport: QUIC via iroh (vs WebRTC, vs raw quinn)

**Status:** Accepted. The loopback benchmark appendix is added when `bench/transport` lands
(Phase 2); the decision does not depend on its outcome, see "What the benchmark can and cannot
tell us".

## Context
The brief asks us to benchmark WebRTC (`webrtc-rs`) against QUIC (`quinn`) and pick. Three
candidates were considered:

| | A. WebRTC (`webrtc` 0.21) | B. QUIC (`quinn`) + our own ICE/STUN/TURN | C. QUIC via `iroh` 1.x |
|---|---|---|---|
| NAT traversal | ICE, standard, interoperable with coturn | **we write it** (candidate gathering, checks, nomination, relay, path migration) | built in: QUIC address discovery, hole punching, path selection hooks |
| Relay | TURN (UDP/TCP/TLS), coturn | **we write it** | HTTPS relay, self-hostable `iroh-relay`, access-control hook |
| Peer identity | DTLS fingerprint via signaling (MITM-able by signaling → needs extra app-layer auth) | TLS cert pinned to Ed25519 | endpoint id **is** an Ed25519 public key, authenticated by TLS 1.3 raw public keys |
| Reliable streams | SCTP data channels (HOL blocking, throughput limits) | native | native, multiplexed |
| Unreliable data | data channel (unordered, 0 retransmits) or RTP | datagrams (RFC 9221) | datagrams |
| Path migration | ICE restart | we write it | native multipath |
| Maturity / risk | async layer over a new sans-IO core; 0.x | quinn is mature, but our NAT code would be new and cannot be validated against real NATs in CI | 1.x, widely deployed; fewer unknowns on the hardest part |
| Browser controller | yes | no (needs WebTransport gateway) | no (same) |

## Decision
Use **C: QUIC via iroh** for native↔native sessions.

* NAT traversal and relaying are the parts that cannot be validated without a real network
  zoo; adopting a hardened implementation is the lower-risk choice ("boring, proven technology
  on the path that decides whether the product works at all").
* Identity unification: the iroh endpoint secret key is our device's Ed25519 key, so there is
  exactly one answer to "who is this peer" at every layer. Allow-listing happens in the
  accept path before any application parsing.
* Traffic mapping: reliable bidirectional streams for control/input/files/terminal;
  per-frame unidirectional streams (reset when stale) for video; datagrams for cursor and
  audio ([ADR-0008](0008-wire-protocol-and-stream-mapping.md)).
* HTTPS relaying works where UDP is blocked, which a pure-UDP design cannot do.
* The brief's "STUN + coturn" is replaced by iroh's QUIC address discovery + `iroh-relay`;
  `nexus-relay` packages a configured, access-controlled relay for self-hosting.

All use goes through a small `nexus_net::Transport` boundary, so a different backend (raw
quinn, WebRTC for a browser client) can be added without touching session logic.

## What the benchmark can and cannot tell us
`bench/transport` measures loopback echo latency (small messages ≈ input events), bulk
throughput, and datagram loss/latency for iroh, raw quinn and `webrtc-rs`. Loopback measures
stack overhead, **not** NAT behaviour, WAN loss or relay performance, so it can only *veto* a
candidate that is unacceptably slow; it cannot prove NAT traversal works. Real-network results
need the device lab checklist (docs/QA_MATRIX.md).

## Consequences
* Dependency on the n0 ecosystem (iroh, noq). Mitigation: the `Transport` boundary; all crates
  pinned and vetted by `cargo-deny`.
* No browser client in v1 (stretch goal; would add WebTransport or a WebRTC adapter).
* Default relay set must be configured to our own relays: nothing is routed through third-party
  infrastructure unless explicitly enabled.
