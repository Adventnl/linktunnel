//! System audio over a real QUIC session: tone source → Opus → datagrams → jitter buffer → PCM.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss
)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use nexus_audio::{SineSource, StreamConfig};
use nexus_capture::synthetic::{SyntheticBackend, SyntheticConfig};
use nexus_media::controller::{ControllerOptions, ControllerSession};
use nexus_media::target::{self, AudioSourceFactory, TargetConfig, TargetStats};
use nexus_proto::v1::{SetAudio, StartAudio};
use nexus_trust::grants;

async fn rig(grant_list: &[&str], audio: bool, video: bool) -> ControllerSession {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let b = loopback_node(&ib, &[key(&ia)]).await;
    let (a_session, b_session) = connect_pair(&a, &b, &loopback_peer(&b)).await;
    let (ha, hb) = (hello(1), hello(2));
    let (ea, eb) = tokio::join!(
        a_session.establish(&ha, CONTROLLER, &[nexus_proto::v1::VideoCodec::H264]),
        b_session.establish(&hb, TARGET, &[nexus_proto::v1::VideoCodec::H264])
    );
    let (ea, eb) = (ea.unwrap(), eb.unwrap());
    let (backend, _p) = SyntheticBackend::new(SyntheticConfig::default());
    let factory: AudioSourceFactory = Arc::new(|cfg: &StreamConfig| {
        Ok(Box::new(SineSource::new(cfg.channels, 440.0).realtime())
            as Box<dyn nexus_audio::AudioSource>)
    });
    let config = TargetConfig {
        grants: grant_list
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>(),
        capture: Arc::new(backend),
        injector: None,
        max_fps: 30,
        max_inflight: 8,
        egress: None,
        files: None,
        clipboard: None,
        consent: None,
        power: None,
        wake_relay: false,
        audio: audio.then_some(factory),
        terminal: None,
        portfwd: None,
        audit: None,
    };
    tokio::spawn(target::serve(
        b_session,
        eb.control,
        config,
        Arc::new(TargetStats::default()),
    ));
    let (controller, streams) = ControllerSession::start(
        a_session,
        ea.control,
        ControllerOptions {
            want_video: video,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    std::mem::forget((streams, a, b));
    controller
}

fn hz(pcm: &[f32]) -> f32 {
    let mono: Vec<f32> = pcm.iter().step_by(2).copied().collect();
    mono.windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count() as f32
        * 48_000.0
        / mono.len() as f32
}

#[tokio::test(flavor = "multi_thread")]
async fn tone_arrives_and_is_audible() {
    let c = rig(&[grants::AUDIO], true, false).await;
    let (player, started) = c
        .start_audio(StartAudio {
            channels: 2,
            frame_ms: 10,
            bitrate_bps: 0,
        })
        .await
        .unwrap();
    assert_eq!(started.channels, 2);
    assert_eq!(started.frame_ms, 10);
    assert_eq!(started.source, "tone");
    // Play out ~1.5 s at real-time pace, like a device callback would.
    let mut pcm = Vec::new();
    for _ in 0..150 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut out = vec![0.0; 960];
        player.lock().unwrap().fill(&mut out);
        pcm.extend(out);
    }
    let st = player.lock().unwrap().stats();
    assert!(st.decoded > 60, "{st:?}");
    // Skip the start-up and measure the steady state.
    let tail = &pcm[pcm.len() / 2..];
    let f = hz(tail);
    assert!((f - 440.0).abs() < 25.0, "heard {f} Hz ({st:?})");
    c.stop_audio();
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_needs_the_grant_and_a_source() {
    let c = rig(&[grants::SCREEN], true, false).await;
    assert!(
        c.start_audio(StartAudio::default()).await.is_err(),
        "no grant"
    );
    let c = rig(&[grants::AUDIO], false, false).await;
    assert!(
        c.start_audio(StartAudio::default()).await.is_err(),
        "no source"
    );
    let c = rig(&[grants::AUDIO], true, false).await;
    assert!(
        c.start_audio(StartAudio {
            channels: 7,
            ..Default::default()
        })
        .await
        .is_err(),
        "bad channel count"
    );
    assert!(
        c.start_audio(StartAudio {
            frame_ms: 13,
            ..Default::default()
        })
        .await
        .is_err(),
        "bad frame size"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn muting_silences_the_stream_and_audio_coexists_with_video() {
    let c = rig(&[grants::AUDIO, grants::SCREEN], true, true).await;
    let (player, _) = c.start_audio(StartAudio::default()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    c.set_audio(SetAudio {
        mute: 1,
        bitrate_bps: 0,
        loss_percent: u32::MAX,
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = player.lock().unwrap().jitter_stats().received;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = player.lock().unwrap().jitter_stats().received;
    assert_eq!(before, after, "no packets while muted");
    c.set_audio(SetAudio {
        mute: 2,
        bitrate_bps: 64_000,
        loss_percent: 10,
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(player.lock().unwrap().jitter_stats().received > after);
}
