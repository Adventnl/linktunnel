//! Hostile audio datagrams straight into the player: header parsing, the jitter buffer and libopus.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_audio::{FrameSize, Player, StreamConfig};

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let cfg = StreamConfig {
        channels: 1 + (sel & 1),
        frame: [FrameSize::Ms2_5, FrameSize::Ms5, FrameSize::Ms10, FrameSize::Ms20][usize::from(sel >> 1) & 3],
        bitrate_bps: 64_000,
    };
    let Ok(mut player) = Player::new(cfg) else { return };
    // Datagrams are separated by a length byte so the fuzzer can build several packets.
    let mut rest = rest;
    let mut now = 0u64;
    while let Some((&len, tail)) = rest.split_first() {
        let n = usize::from(len).min(tail.len());
        let (pkt, after) = tail.split_at(n);
        now += 10_000;
        player.push(pkt, now);
        let mut out = vec![0.0f32; cfg.frame_len()];
        player.fill(&mut out);
        assert!(out.iter().all(|s| s.is_finite()));
        rest = after;
    }
});
