//! Structured operations against the jitter buffer: it must never panic and, within one continuous
//! stream, never hand out the same packet twice. (After the buffer re-synchronises, from a sender's
//! discontinuity flag or from running dry, an old duplicate may legitimately play once more: a
//! ten-millisecond repeat, not a safety issue.)
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use nexus_audio::{JitterBuffer, Pull};
use nexus_proto::wire::audio::{AudioCodec, AudioHeader};

#[derive(Debug, Arbitrary)]
enum Op {
    Push { seq: u16, ts: u32, flags: u8, advance_us: u16 },
    Pull,
}

fuzz_target!(|ops: Vec<Op>| {
    let mut j = JitterBuffer::new(10_000);
    let mut now = 0u64;
    let mut played = std::collections::HashSet::new();
    let mut underruns = 0;
    for op in ops {
        match op {
            Op::Push { seq, ts, flags, advance_us } => {
                now += u64::from(advance_us);
                let h = AudioHeader { codec: AudioCodec::Opus, channels: 2, flags, seq, frame_samples: 480, timestamp: ts };
                // The payload is the (seq, ts) pair, so a replay of the same packet is detectable
                // unless the discontinuity flag legitimately restarted the stream.
                let mut payload = seq.to_le_bytes().to_vec();
                payload.extend_from_slice(&ts.to_le_bytes());
                if flags & 1 == 1 {
                    played.clear();
                }
                j.push(&h, bytes::Bytes::from(payload), now);
            }
            Op::Pull => {
                let pulled = j.pull();
                if j.stats().underruns != underruns {
                    underruns = j.stats().underruns;
                    played.clear();
                }
                if let Pull::Packet(p) = pulled {
                    assert!(played.insert(p.to_vec()), "a packet was played twice");
                }
            }
        }
        assert!(j.buffered() <= 1 << 16);
    }
});
