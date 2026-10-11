# ADR-0008: Wire protocol and QUIC stream mapping

**Status:** Accepted (implemented in `nexus-proto`)

## Decision
* **Control plane = protobuf** (`prost`, compiled by pure-Rust `protox`, so no system `protoc`),
  length-delimited with a hard per-stream frame cap checked *before* buffering.
  Evolution rules: never renumber, new behaviour = new `oneof` variant + capability string.
* **Hot paths = compact little-endian binary** (no JSON, no varint-heavy parsing): video frame
  header (40 B), audio packet (12 B header), cursor update (28 B), input packets (TLV events with
  explicit lengths so unknown events are skippable). Every decoder is total and property-tested
  never to panic.
* **Keys are HID usages, not characters,** so the target's own layout interprets them;
  committed text (IME) is a separate `Text` event.

| Stream | QUIC mapping | Reliability |
|---|---|---|
| control (hello, RPC) | bidirectional | reliable, ordered |
| input | bidirectional, high priority | reliable, ordered; target coalesces moves, never reorders keys |
| video frame *n* | **its own unidirectional stream** | reliable within the frame; sender `reset`s a stream whose frame is stale |
| cursor position | datagram | latest wins (serial-number compare) |
| cursor shape | control stream | reliable, keyed by `shape_id` |
| audio | datagram | loss concealed by the Opus decoder |
| clipboard, files, terminal, port-forward | bidirectional, on demand | reliable |

Per-frame streams give loss recovery inside a frame for free, avoid head-of-line blocking
across frames, and make "drop stale" a one-line operation. If a frame misses its deadline the
receiver asks for **reference invalidation** (re-encode against the last good frame) instead of
a full keyframe.

## Alternatives considered
RTP-style datagram video with app-level FEC/NACK: lower worst-case latency under loss but a
large amount of new, unproven machinery. It remains a possible fast path for WAN once per-frame
streams are measured.
