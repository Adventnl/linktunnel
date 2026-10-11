//! Adaptive bitrate over a real QUIC session whose sender sits behind a slow, delayed uplink.
//!
//! The bottleneck is modelled in the sender (`EgressShaper`): kernel `tc netem` is not available
//! in the build sandbox and QUIC hole punching routes around a userspace UDP proxy, so the
//! shaper is the one impairment that is reliable everywhere. It still produces the signal the
//! receiver acts on: frames arrive later than they were sent, by a growing amount.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use nexus_capture::synthetic::{SyntheticBackend, SyntheticConfig};
use nexus_media::controller::{ControllerOptions, ControllerSession, MediaEvent};
use nexus_media::target::{self, EgressShaper, TargetConfig, TargetStats};
use nexus_trust::grants;

struct Outcome {
    goodput_bps: f64,
    p50_ms: f64,
    p95_ms: f64,
    keyframe_requests: u64,
}

/// Streams a noisy "video" screen for `secs`, measuring the final `window` seconds. The shaper
/// starts as `first`; at `change.0` seconds it is swapped for `change.1` (there is no live
/// reconfiguration, so the change is modelled by running two shapers back to back).
async fn run(adaptive: bool, rate_mbps: f64, delay_ms: u64, secs: f64, window: f64) -> Outcome {
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

    let (backend, _probe) = SyntheticBackend::new(SyntheticConfig {
        width: 1280,
        height: 720,
        fps: 60.0,
        animate: true,
        noise_percent: 5,
        ..Default::default()
    });
    let config = TargetConfig {
        grants: [grants::SCREEN]
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>(),
        capture: Arc::new(backend),
        injector: None,
        max_fps: 60,
        max_inflight: 12,
        files: None,
        egress: Some(EgressShaper {
            rate_bps: (rate_mbps * 1e6) as u64,
            delay: Duration::from_millis(delay_ms),
        }),
    };
    let tstats = Arc::new(TargetStats::default());
    let _target = tokio::spawn(target::serve(
        b_session,
        eb.control,
        config,
        Arc::clone(&tstats),
    ));
    let opts = ControllerOptions {
        adaptive,
        bitrate_bps: 6_000_000,
        ..Default::default()
    };
    let (controller, mut streams) = ControllerSession::start(a_session, ea.control, opts)
        .await
        .unwrap();

    let start = Instant::now();
    let mut window_started: Option<(Instant, u64)> = None;
    while start.elapsed().as_secs_f64() < secs {
        if let Ok(Some(MediaEvent::Closed(r))) =
            tokio::time::timeout(Duration::from_millis(20), streams.events.recv()).await
        {
            panic!("session closed: {r}");
        }
        if std::env::var_os("NEXUS_TRACE_RATE").is_some() && start.elapsed().as_millis() % 1000 < 25
        {
            println!(
                "t={:>4.1}s target {:>5.2} Mbit/s received {:>6.2} MB | encoder {} fps {}% {:.2} Mbit/s",
                start.elapsed().as_secs_f64(),
                f64::from(
                    controller
                        .stats()
                        .target_bitrate_bps
                        .load(Ordering::Relaxed)
                ) / 1e6,
                controller.stats().bytes_received.load(Ordering::Relaxed) as f64 / 1e6,
                tstats.fps_now.load(Ordering::Relaxed),
                tstats.scale_now.load(Ordering::Relaxed),
                f64::from(tstats.bitrate_now.load(Ordering::Relaxed)) / 1e6
            );
        }
        if window_started.is_none() && start.elapsed().as_secs_f64() >= secs - window {
            let _ = controller.stats().capture_to_receive.take_summary(); // reset the recorder
            window_started = Some((
                Instant::now(),
                controller.stats().bytes_received.load(Ordering::Relaxed),
            ));
        }
    }
    let (t0, bytes0) = window_started.unwrap();
    let bytes = controller.stats().bytes_received.load(Ordering::Relaxed) - bytes0;
    let summary = controller.stats().capture_to_receive.take_summary();
    let out = Outcome {
        goodput_bps: bytes as f64 * 8.0 / t0.elapsed().as_secs_f64(),
        p50_ms: summary.p50_us as f64 / 1000.0,
        p95_ms: summary.p95_us as f64 / 1000.0,
        keyframe_requests: controller.stats().keyframe_requests.load(Ordering::Relaxed),
    };
    controller.close().await;
    std::mem::forget(a);
    std::mem::forget(b);
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adaptation_keeps_latency_down_behind_a_slow_uplink() {
    // 4 Mbit/s uplink, 25 ms delay; the source wants 6 Mbit/s of incompressible video.
    let fixed = run(false, 4.0, 25, 16.0, 5.0).await;
    let adaptive = run(true, 4.0, 25, 16.0, 5.0).await;
    println!(
        "4 Mbit/s uplink: fixed 6 Mbit/s → {:.2} Mbit/s, p50 {:.0} ms, p95 {:.0} ms | adaptive → {:.2} Mbit/s, p50 {:.0} ms, p95 {:.0} ms (keyframe requests {} vs {})",
        fixed.goodput_bps / 1e6,
        fixed.p50_ms,
        fixed.p95_ms,
        adaptive.goodput_bps / 1e6,
        adaptive.p50_ms,
        adaptive.p95_ms,
        fixed.keyframe_requests,
        adaptive.keyframe_requests
    );
    assert!(
        adaptive.goodput_bps < 4.4e6,
        "cannot exceed the path: {:.2}",
        adaptive.goodput_bps / 1e6
    );
    assert!(
        adaptive.goodput_bps > 1.0e6,
        "must still use a meaningful share of the path: {:.2}",
        adaptive.goodput_bps / 1e6
    );
    assert!(
        adaptive.p95_ms < fixed.p95_ms * 0.6,
        "adaptation must cut tail latency: fixed {:.0} ms, adaptive {:.0} ms",
        fixed.p95_ms,
        adaptive.p95_ms
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fast_path_lets_the_rate_climb() {
    let fast = run(true, 40.0, 10, 20.0, 5.0).await;
    println!(
        "40 Mbit/s uplink: {:.2} Mbit/s, p95 {:.0} ms",
        fast.goodput_bps / 1e6,
        fast.p95_ms
    );
    assert!(
        fast.goodput_bps > 6.5e6,
        "the rate must grow past the 6 Mbit/s start when the path allows: {:.2}",
        fast.goodput_bps / 1e6
    );
}
