# Transport benchmark (ADR-0003): loopback

**What this measures:** the CPU/stack overhead of each candidate carrying the three traffic
classes the product uses. **What it cannot measure:** NAT traversal, packet loss, jitter, relay
performance, or real-network congestion behaviour. Loopback can *veto* a stack that is
unacceptably slow; it cannot prove one fit. Real-network validation is the device-lab checklist.

## Method
`bench/transport` (standalone workspace; `cargo run --release -- all`). Same machine, same process,
`127.0.0.1`, release build. Each candidate runs:

| Class | Product use | Measurement |
|---|---|---|
| small reliable echo (64 B) | input events, control RPC | round-trip time, 2000 samples |
| small unreliable echo (100 B, paced 2 ms) | cursor position, audio | round-trip time + loss, 2000 samples |
| bulk reliable upload (64 MiB) | files, keyframe bursts | throughput |

* **iroh 1.3** is measured *through the real `nexus-net` stack*: identity-pinned TLS, allow-list
  hook, session handshake: i.e. what the product actually pays.
* **quinn 0.11** is a bare QUIC echo (self-signed certificate, verification skipped) and is the
  floor for any QUIC-based design.
* **webrtc-rs 0.21** uses SCTP data channels (reliable-ordered for the first class,
  unordered/0-retransmit for the second, 16 KiB messages for bulk) over DTLS over ICE-UDP.

Environment: MacBook Air, Apple M4 (10 cores), macOS 26.6, Rust 1.96.0, 2026-10-12.

## Results (two consecutive runs)

Run 1

| transport | reliable p50 | p95 | p99 | unreliable p50 | p95 | loss | bulk |
|---|---|---|---|---|---|---|---|
| iroh 1.3 (via nexus-net) | 0.036 ms | 0.089 ms | 0.114 ms | 0.084 ms | 0.151 ms | 0.0 % | 2864 Mbit/s |
| quinn 0.11 (raw) | 0.025 ms | 0.033 ms | 0.040 ms | 0.146 ms | 0.289 ms | 0.0 % | 2952 Mbit/s |
| webrtc-rs 0.21 (data channels) | 0.047 ms | 0.121 ms | 0.159 ms | 0.296 ms | 0.544 ms | 0.0 % | 420 Mbit/s |

Run 2

| transport | reliable p50 | p95 | p99 | unreliable p50 | p95 | loss | bulk |
|---|---|---|---|---|---|---|---|
| iroh 1.3 (via nexus-net) | 0.035 ms | 0.045 ms | 0.052 ms | 0.203 ms | 0.303 ms | 0.0 % | 2606 Mbit/s |
| quinn 0.11 (raw) | 0.026 ms | 0.036 ms | 0.043 ms | 0.171 ms | 0.255 ms | 0.0 % | 2788 Mbit/s |
| webrtc-rs 0.21 (data channels) | 0.048 ms | 0.081 ms | 0.105 ms | 0.293 ms | 0.534 ms | 0.0 % | 422 Mbit/s |

## Reading the numbers
* All three are far below the latency budget (a 60 fps frame is 16,700 µs; these are tens of µs):
  **no candidate is vetoed on latency.** Differences of tens of microseconds are irrelevant next to
  encode (ms), network RTT (ms) and display refresh.
* iroh costs **~10 µs** (reliable round trip) and **~5-10 %** bulk throughput over raw quinn. That is
  the price of identity pinning, allow-listing and multipath path management.
* webrtc-rs is **~1.4-1.9× slower on small messages and ~6.5× slower on bulk** than the QUIC stacks
  (SCTP + DTLS overhead). At 420 Mbit/s it still exceeds any realistic remote-desktop stream
  (≤ 100 Mbit/s), so *throughput alone* does not rule it out, but it leaves no headroom for 4K
  keyframe bursts, file transfers sharing the pipe, or low-power targets.
* Datagram latency is noisy at this scale (differences between runs exceed differences between
  stacks); do not read a ranking into it.

## Decision input
Performance does not decide between the candidates. The decision rests on the non-performance
criteria in ADR-0003: proven NAT traversal and relay fallback (including over HTTPS), identity =
Ed25519 key, native stream/datagram model for the media mapping, and unified path migration.
