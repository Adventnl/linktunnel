#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
//! End-to-end audio pipeline tests without a network: source → Opus → datagrams → (loss,
//! reordering) → jitter buffer → decoder → PCM.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use nexus_audio::{
    AudioSource, FrameSize, OpusDecoder, OpusEncoder, Player, SineSource, StreamConfig,
    spawn_stream,
};

fn cfg() -> StreamConfig {
    StreamConfig::default()
}

/// Frequency estimate by zero crossings of channel 0.
fn estimate_hz(pcm: &[f32], channels: usize) -> f32 {
    let mono: Vec<f32> = pcm.iter().step_by(channels).copied().collect();
    let crossings = mono
        .windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count();
    crossings as f32 * 48_000.0 / mono.len() as f32
}

fn rms(pcm: &[f32]) -> f32 {
    (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt()
}

fn tone_frames(n: usize) -> Vec<Vec<f32>> {
    let c = cfg();
    let mut src = SineSource::new(c.channels, 440.0);
    (0..n)
        .map(|_| {
            let mut f = vec![0.0; c.frame_len()];
            src.read(&mut f).unwrap();
            f
        })
        .collect()
}

#[test]
fn opus_roundtrip_preserves_the_tone() {
    let c = cfg();
    let mut enc = OpusEncoder::new(&c).unwrap();
    let mut dec = OpusDecoder::new(&c).unwrap();
    let mut decoded = Vec::new();
    for f in tone_frames(100) {
        let packet = enc.encode(&f).unwrap().to_vec();
        assert!(packet.len() < 400, "{} bytes for 10 ms", packet.len());
        let mut out = vec![0.0; c.frame_len()];
        dec.decode(&packet, &mut out).unwrap();
        decoded.extend(out);
    }
    let tail = &decoded[decoded.len() / 2..];
    let hz = estimate_hz(tail, 2);
    assert!((hz - 440.0).abs() < 15.0, "decoded tone at {hz} Hz");
    assert!(rms(tail) > 0.25, "rms {}", rms(tail));
}

#[test]
fn wrong_sized_frames_and_bad_configs_are_errors_not_panics() {
    let c = cfg();
    let mut enc = OpusEncoder::new(&c).unwrap();
    assert!(enc.encode(&[0.0; 10]).is_err());
    let bad = StreamConfig { channels: 3, ..c };
    assert!(OpusEncoder::new(&bad).is_err());
    assert!(
        OpusDecoder::new(&StreamConfig {
            bitrate_bps: 1,
            ..c
        })
        .is_err()
    );
    let mut dec = OpusDecoder::new(&c).unwrap();
    assert!(dec.decode(&[1, 2, 3], &mut [0.0; 4]).is_err());
}

#[test]
fn every_frame_size_roundtrips() {
    for frame in [
        FrameSize::Ms2_5,
        FrameSize::Ms5,
        FrameSize::Ms10,
        FrameSize::Ms20,
    ] {
        let c = StreamConfig { frame, ..cfg() };
        let mut enc = OpusEncoder::new(&c).unwrap();
        let mut dec = OpusDecoder::new(&c).unwrap();
        let mut src = SineSource::new(2, 300.0);
        for _ in 0..30 {
            let mut f = vec![0.0; c.frame_len()];
            src.read(&mut f).unwrap();
            let p = enc.encode(&f).unwrap().to_vec();
            let mut out = vec![0.0; c.frame_len()];
            dec.decode(&p, &mut out).unwrap();
        }
    }
}

/// Runs the whole chain, dropping and shuffling datagrams as `net` dictates, and returns what the
/// player produced plus its counters.
fn run_chain(
    frames: usize,
    net: impl Fn(usize, Bytes) -> Vec<(usize, Bytes)>,
) -> (Vec<f32>, nexus_audio::PlayerStats) {
    let c = cfg();
    let collected: Arc<Mutex<Vec<Bytes>>> = Arc::default();
    let sink = Arc::clone(&collected);
    let mut handle = spawn_stream(
        Box::new(SineSource::new(2, 440.0).limit_frames(frames as u64)),
        c,
        Box::new(move |d| {
            sink.lock().unwrap().push(d);
            true
        }),
    )
    .unwrap();
    while !handle.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    handle.stop();
    let packets = collected.lock().unwrap().clone();
    assert_eq!(packets.len(), frames);

    // Deliver with the network model: (slot, datagram) pairs; arrival time = slot * 10 ms.
    let mut deliveries: Vec<(usize, Bytes)> = packets
        .into_iter()
        .enumerate()
        .flat_map(|(i, p)| net(i, p))
        .collect();
    deliveries.sort_by_key(|(slot, _)| *slot);
    let mut player = Player::new(c).unwrap();
    let mut pcm = Vec::new();
    let mut it = deliveries.into_iter().peekable();
    for slot in 0..frames + 12 {
        while it.peek().is_some_and(|(s, _)| *s <= slot) {
            let (s, d) = it.next().unwrap();
            player.push(&d, s as u64 * 10_000);
        }
        let mut out = vec![0.0; c.frame_len()];
        player.fill(&mut out);
        pcm.extend(out);
    }
    (pcm, player.stats())
}

#[test]
fn clean_network_plays_everything_with_no_concealment() {
    let (pcm, st) = run_chain(100, |i, p| vec![(i, p)]);
    assert_eq!(st.concealed + st.recovered, 0, "{st:?}");
    assert_eq!(st.decoded, 100);
    let hz = estimate_hz(&pcm[pcm.len() / 3..pcm.len() / 3 * 2], 2);
    assert!((hz - 440.0).abs() < 15.0, "{hz}");
}

#[test]
fn reordering_within_the_buffer_is_invisible() {
    // Swap every pair of packets.
    let (_, st) = run_chain(100, |i, p| {
        vec![(if i % 2 == 0 { i + 1 } else { i - 1 }, p)]
    });
    assert_eq!(st.concealed + st.recovered, 0, "{st:?}");
    // At most the very first packet can be lost to start-up (the buffer cannot know it exists).
    assert!(st.decoded >= 99, "{st:?}");
}

#[test]
fn isolated_loss_is_recovered_from_fec_and_the_tone_survives() {
    // Drop every 10th packet.
    let (pcm, st) = run_chain(200, |i, p| if i % 10 == 5 { vec![] } else { vec![(i, p)] });
    assert_eq!(st.recovered + st.concealed, 20, "{st:?}");
    assert!(
        st.recovered >= 15,
        "FEC should rescue isolated losses: {st:?}"
    );
    let mid = &pcm[pcm.len() / 3..pcm.len() / 3 * 2];
    let hz = estimate_hz(mid, 2);
    assert!((hz - 440.0).abs() < 25.0, "{hz}");
    assert!(rms(mid) > 0.2);
}

#[test]
fn burst_loss_is_concealed() {
    let (_, st) = run_chain(100, |i, p| {
        if (40..44).contains(&i) {
            vec![]
        } else {
            vec![(i, p)]
        }
    });
    assert!(st.concealed >= 3, "{st:?}");
}

#[test]
fn garbage_and_foreign_streams_are_rejected_without_panicking() {
    let mut p = Player::new(cfg()).unwrap();
    p.push(&[], 0);
    p.push(&[0x81], 0);
    p.push(&[0xFF; 100], 0);
    // A mono stream pushed at a stereo player.
    let mono = StreamConfig {
        channels: 1,
        ..cfg()
    };
    let mut pk = nexus_audio::Packetizer::new(&mono);
    p.push(&pk.packetize(&[1, 2, 3]).unwrap(), 0);
    assert_eq!(p.stats().rejected, 4);
    let mut out = vec![1.0; 480 * 2 * 3];
    p.fill(&mut out);
    assert!(out.iter().all(|s| *s == 0.0));
}

#[test]
fn fill_handles_any_callback_size() {
    let c = cfg();
    let mut enc = OpusEncoder::new(&c).unwrap();
    let mut pk = nexus_audio::Packetizer::new(&c);
    let mut p = Player::new(c).unwrap();
    for (i, f) in tone_frames(30).into_iter().enumerate() {
        let d = pk.packetize(enc.encode(&f).unwrap()).unwrap();
        p.push(&d, i as u64 * 10_000);
    }
    for size in [1usize, 7, 480, 1000, 4096, 3] {
        let mut out = vec![0.0; size * 2];
        p.fill(&mut out);
    }
}

#[test]
fn muting_marks_a_discontinuity_and_resumes() {
    let c = cfg();
    let collected: Arc<Mutex<Vec<Bytes>>> = Arc::default();
    let sink = Arc::clone(&collected);
    let handle = spawn_stream(
        Box::new(SineSource::new(2, 440.0).realtime()),
        c,
        Box::new(move |d| {
            sink.lock().unwrap().push(d);
            true
        }),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    handle.set_muted(true);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let during = collected.lock().unwrap().len();
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(
        collected.lock().unwrap().len(),
        during,
        "muted stream must be silent"
    );
    handle.set_muted(false);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let packets = collected.lock().unwrap().clone();
    drop(handle);
    let flagged = packets
        .iter()
        .filter(|p| nexus_proto::wire::audio::decode_packet(p).unwrap().0.flags & 1 == 1)
        .count();
    assert_eq!(flagged, 1);
    assert!(packets.len() > during);
}
