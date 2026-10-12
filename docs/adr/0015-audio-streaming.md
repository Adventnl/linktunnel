# ADR-0015: Audio streaming

**Status:** Accepted (implemented; devices unverified on hardware)

## Context
The brief asks for system audio from the target with Opus at 2.5-10 ms frames. Audio is the one stream where *late is
worthless*: a packet that arrives after its slot is better dropped and concealed than delayed.

## Decision
* **Codec:** Opus 48 kHz stereo/mono via `libopus` (the `opus` crate, vendored build), `LowDelay` application, constrained VBR,
  in-band FEC on, expected-loss tuned from receiver reports. Frame sizes 2.5/5/10/20 ms (default 10 ms, 96 kbit/s).
* **Transport:** one packet = one **QUIC datagram** (12-byte header: tag `0x81`, codec, channels, flags, seq, frame size, 48 kHz
  timestamp). The high bit of the first byte distinguishes audio from cursor updates (`0x01`) on the shared datagram channel
  (`wire::audio::is_audio`). Loss is concealed, never retransmitted.
* **Receive side:** an *adaptive jitter buffer* (RFC 3550 interarrival jitter → target depth 1..16 frames, grow fast / shrink slowly,
  pull-driven by the playback clock) feeds the decoder; a lost frame is rebuilt from the *next* packet's FEC if present, else by
  Opus packet-loss concealment. A stalled stream re-buffers instead of replaying stale audio.
* **Control:** `StartAudio`/`AudioStarted`/`StopAudio`/`SetAudio` on the control stream; new **`audio` grant** (+ consent like video).
  The agent that views plays the sound itself (`DevicePlayback`, cpal); the target captures via cpal (`DeviceSource`): WASAPI loopback,
  CoreAudio loopback (macOS 14.6+), PulseAudio/PipeWire monitor on Linux. Microphone sharing is a separate, off-by-default switch.
* **Isolation:** codec/jitter/packetizer live in `nexus-audio` with no OS dependency; devices are behind the `device` feature.

## Consequences
* ✅ Tested without hardware: Opus round trip, loss/reorder/burst scenarios, mute discontinuity, hostile datagrams, QUIC end to end, fuzzing (libopus).
* 🟡 Real devices were never run (no sound hardware in the build container). No resampler: a device that cannot do 48 kHz f32/i16 is refused.
* DTX / silence suppression is deliberately off (it would need timestamp-gap handling in the jitter buffer).
* Audio and video are not lip-synced to each other yet (both carry sender clocks; the controller does not align them).

## Alternatives
* Raw PCM over the reliable stream: simple, but retransmission delay turns loss into latency spikes.
* WebRTC audio (SRTP + NetEQ): a better jitter buffer but pulls in the transport we rejected (ADR-0003).
* A pure-Rust Opus: young and unaudited on the fuzz-exposed path; libopus is the reference.
