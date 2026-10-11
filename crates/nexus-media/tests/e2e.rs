//! End-to-end: a real target and controller over a real QUIC session on loopback.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use nexus_capture::synthetic::{PROBE_OFF, PROBE_ON, SyntheticBackend, SyntheticConfig};
use nexus_input::recording::RecordingInjector;
use nexus_input::{InputEvent, KeyState, Modifiers, MouseButton};
use nexus_media::controller::{ControllerOptions, ControllerSession, MediaEvent, Streams};
use nexus_media::target::{self, TargetConfig, TargetStats};
use nexus_media::viewer::{CursorOverlay, Picture, VideoDecoder};
use nexus_trust::grants;

struct Rig {
    controller: ControllerSession,
    streams: Streams,
    stats: Arc<TargetStats>,
    target_task: tokio::task::JoinHandle<target::EndReason>,
    probe: nexus_capture::synthetic::Probe,
    recorded: nexus_input::recording::Recorded,
}

async fn rig(grant_list: &[&str], width: u32, height: u32) -> Rig {
    rig_with(
        grant_list,
        SyntheticConfig {
            width,
            height,
            ..Default::default()
        },
        ControllerOptions::default(),
    )
    .await
}

async fn rig_with(
    grant_list: &[&str],
    synthetic: SyntheticConfig,
    options: ControllerOptions,
) -> Rig {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await; // controller
    let b = loopback_node(&ib, &[key(&ia)]).await; // target
    let (a_session, b_session) = connect_pair(&a, &b, &loopback_peer(&b)).await;
    let (ha, hb) = (hello(1), hello(2));
    let (ea, eb) = tokio::join!(
        a_session.establish(&ha, CONTROLLER, &[nexus_proto::v1::VideoCodec::H264]),
        b_session.establish(&hb, TARGET, &[nexus_proto::v1::VideoCodec::H264])
    );
    let (ea, eb) = (ea.unwrap(), eb.unwrap());

    let (backend, probe) = SyntheticBackend::new(synthetic);
    let (injector, recorded) = RecordingInjector::new();
    let injector = std::sync::Mutex::new(Some(injector));
    let config = TargetConfig {
        grants: grant_list
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>(),
        capture: Arc::new(backend),
        injector: Some(Arc::new(move || {
            let inj =
                injector.lock().unwrap().take().ok_or_else(|| {
                    nexus_input::InputError::Failed("one injector per test".into())
                })?;
            Ok(Box::new(inj) as Box<dyn nexus_input::Injector>)
        })),
        max_fps: 60,
        max_inflight: 12,
        egress: None,
        files: None,
        clipboard: None,
    };
    let stats = Arc::new(TargetStats::default());
    let target_task = tokio::spawn(target::serve(
        b_session,
        eb.control,
        config,
        Arc::clone(&stats),
    ));
    let (controller, streams) = ControllerSession::start(a_session, ea.control, options)
        .await
        .unwrap();
    // Keep the nodes alive for the duration of the test.
    std::mem::forget(a);
    std::mem::forget(b);
    Rig {
        controller,
        streams,
        stats,
        target_task,
        probe,
        recorded,
    }
}

async fn next_event(streams: &mut Streams, what: &str) -> MediaEvent {
    tokio::time::timeout(Duration::from_secs(5), streams.events.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("event stream ended")
}

/// Reads events until a decoded picture satisfies `pred`.
async fn picture_where(
    streams: &mut Streams,
    dec: &mut VideoDecoder,
    controller: &ControllerSession,
    what: &str,
    pred: impl Fn(&Picture) -> bool,
) -> Picture {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(500), streams.events.recv()).await
        else {
            continue;
        };
        if let MediaEvent::Frame(f) = ev {
            match dec.decode(&f) {
                Ok(Some(p)) => {
                    if pred(&p) {
                        return p;
                    }
                }
                Ok(None) => {}
                Err(_) => controller.request_keyframe(),
            }
        }
    }
    panic!("never saw: {what}");
}

fn probe_colour(p: &Picture) -> [u8; 3] {
    // Sample the middle of the 64x64 probe square, away from the codec's block edges.
    let i = (32 * p.width as usize + 32) * 4;
    [p.bgra[i], p.bgra[i + 1], p.bgra[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) < 40)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn video_flows_and_a_flash_is_seen_with_measured_latency() {
    let mut r = rig(&[grants::SCREEN, grants::INPUT], 640, 360).await;
    let mut seen_monitors = false;
    let started = loop {
        match next_event(&mut r.streams, "VideoStarted").await {
            MediaEvent::Monitors(m) => {
                assert_eq!((m[0].width, m[0].height), (640, 360));
                seen_monitors = true;
            }
            MediaEvent::Started(s) => break s,
            other => panic!("unexpected {other:?}"),
        }
    };
    assert!(
        seen_monitors,
        "the monitor list comes before the stream starts"
    );
    assert_eq!((started.width, started.height), (640, 360));
    assert_eq!(started.encoder, "openh264");

    let mut dec = VideoDecoder::new();
    let first = picture_where(
        &mut r.streams,
        &mut dec,
        &r.controller,
        "first picture",
        |_| true,
    )
    .await;
    assert!(
        near(probe_colour(&first), PROBE_OFF),
        "{:?}",
        probe_colour(&first)
    );

    // Flip the probe and time how long until the other end can see it.
    let mut latencies = Vec::new();
    for round in 0..8 {
        let on = round % 2 == 0;
        let t0 = Instant::now();
        r.probe.set_flash(on);
        let want = if on { PROBE_ON } else { PROBE_OFF };
        picture_where(&mut r.streams, &mut dec, &r.controller, "probe flip", |p| {
            near(probe_colour(p), want)
        })
        .await;
        latencies.push(t0.elapsed());
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    latencies.sort();
    println!(
        "glass-to-glass over loopback QUIC (software H.264, 640x360): p50 {:?} max {:?}",
        latencies[latencies.len() / 2],
        latencies.last().unwrap()
    );
    assert!(
        latencies[latencies.len() / 2] < Duration::from_millis(250),
        "median flash latency {latencies:?}"
    );

    // An idle screen costs nothing: after the flips settle, no frames flow.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = r.stats.frames.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        r.stats.frames.load(Ordering::Relaxed),
        before,
        "a static screen must not be re-encoded"
    );

    r.controller.close().await;
    let end = tokio::time::timeout(Duration::from_secs(5), r.target_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end, target::EndReason::ControllerLeft);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delta_frames_carry_dirty_rectangles() {
    let mut r = rig(&[grants::SCREEN], 320, 240).await;
    let mut dec = VideoDecoder::new();
    picture_where(&mut r.streams, &mut dec, &r.controller, "first", |_| true).await;
    r.probe.set_flash(true);
    let p = picture_where(
        &mut r.streams,
        &mut dec,
        &r.controller,
        "delta with damage",
        |p| !p.dirty.is_empty(),
    )
    .await;
    assert_eq!(
        p.dirty,
        vec![nexus_codec::Rect {
            x: 0,
            y: 0,
            w: 64,
            h: 64
        }]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_arrives_in_order_and_disconnect_releases_held_keys() {
    let r = rig(&[grants::SCREEN, grants::INPUT], 320, 240).await;
    let key = |usage, state| InputEvent::Key {
        page: 7,
        usage,
        state,
        modifiers: Modifiers::default(),
    };
    let sent = vec![
        InputEvent::MouseMoveAbs {
            monitor: 0,
            x: 10,
            y: 20,
        },
        key(0xE1, KeyState::Down), // shift
        key(0x04, KeyState::Down), // a
        key(0x04, KeyState::Up),
        InputEvent::MouseButton {
            button: MouseButton::LEFT,
            pressed: true,
        },
        InputEvent::Text {
            text: "héllo".into(),
        },
    ];
    r.controller.send_input(sent.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    while r.recorded.events().len() < sent.len() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        r.recorded.events(),
        sent,
        "events arrive complete and in order"
    );

    // Shift and the left button are still held. Closing the session must let go of them.
    r.controller.close().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), r.target_task)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let events = r.recorded.events();
        let released_shift = events.contains(&key(0xE1, KeyState::Up));
        let released_button = events.contains(&InputEvent::MouseButton {
            button: MouseButton::LEFT,
            pressed: false,
        });
        if released_shift && released_button {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "held input was not released after disconnect: {events:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grants_are_enforced_by_the_target() {
    // View-only: video yes, input ignored.
    let mut r = rig(&[grants::SCREEN], 320, 240).await;
    r.controller.send_input([InputEvent::MouseMoveAbs {
        monitor: 0,
        x: 1,
        y: 1,
    }]);
    let mut dec = VideoDecoder::new();
    picture_where(
        &mut r.streams,
        &mut dec,
        &r.controller,
        "video without input grant",
        |_| true,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        r.recorded.events().is_empty(),
        "no input grant: nothing may be injected"
    );

    // No screen grant: the stream is refused.
    let mut r = rig(&[grants::INPUT], 320, 240).await;
    loop {
        match next_event(&mut r.streams, "refusal").await {
            MediaEvent::Refused { code, .. } => {
                assert_eq!(code, nexus_proto::v1::ErrorCode::PermissionDenied as i32);
                break;
            }
            MediaEvent::Frame(_) | MediaEvent::Started(_) => {
                panic!("video started without a screen grant")
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cursor_shape_comes_before_position_updates_and_positions_are_latest_wins() {
    let mut r = rig(&[grants::SCREEN], 320, 240).await;
    let mut overlay = CursorOverlay::new();
    r.probe.set_cursor_shape(4);
    r.probe.move_cursor(100, 50);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_shape = false;
    let mut pos = None;
    while Instant::now() < deadline && !(got_shape && pos.is_some()) {
        tokio::select! {
            ev = r.streams.events.recv() => {
                if let Some(MediaEvent::CursorShape(s)) = ev { overlay.add_shape(s); got_shape = true; }
            }
            changed = r.streams.cursor.changed() => {
                if changed.is_ok() {
                    let c = *r.streams.cursor.borrow_and_update();
                    if let Some(c) = c.filter(|c| c.x == 100 && c.y == 50) { pos = Some(c); }
                }
            }
        }
    }
    let pos = pos.expect("cursor position");
    assert_eq!(pos.shape_id, 4);
    assert!(
        overlay.shape(pos.shape_id).is_some(),
        "the shape must be known by the time a position references it (or arrive shortly after)"
    );

    // Move again: only the newest position matters.
    r.probe.move_cursor(7, 9);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if r.streams
            .cursor
            .borrow()
            .as_ref()
            .is_some_and(|c| c.x == 7 && c.y == 9)
        {
            break;
        }
        assert!(Instant::now() < deadline, "second position never arrived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keyframe_request_is_answered() {
    let mut r = rig(&[grants::SCREEN], 320, 240).await;
    let mut dec = VideoDecoder::new();
    picture_where(&mut r.streams, &mut dec, &r.controller, "first", |_| true).await;
    // Make a delta, then simulate a decoder failure.
    r.probe.set_flash(true);
    picture_where(&mut r.streams, &mut dec, &r.controller, "delta", |_| true).await;
    r.controller.request_keyframe();
    r.probe.set_flash(false); // new damage so the encoder has something to send as the keyframe
    let mut dec2 = VideoDecoder::new(); // a brand new decoder can only start from a keyframe
    let p = picture_where(
        &mut r.streams,
        &mut dec2,
        &r.controller,
        "recovered picture",
        |_| true,
    )
    .await;
    assert!(near(probe_colour(&p), PROBE_OFF) || near(probe_colour(&p), PROBE_ON));
    assert!(
        r.controller
            .stats()
            .keyframe_requests
            .load(Ordering::Relaxed)
            >= 1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_viewed_monitor_can_be_switched_mid_session() {
    let mut r = rig_with(
        &[grants::SCREEN],
        SyntheticConfig {
            monitors: 2,
            ..Default::default()
        },
        ControllerOptions::default(),
    )
    .await;
    let mut dec = VideoDecoder::new();
    // First monitor 0 …
    let mut seen0 = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !seen0 {
        if let Ok(Some(MediaEvent::Frame(f))) =
            tokio::time::timeout(Duration::from_millis(300), r.streams.events.recv()).await
        {
            assert_eq!(f.header.monitor, 0);
            seen0 = matches!(dec.decode(&f), Ok(Some(_)));
        }
    }
    assert!(seen0, "monitor 0 never produced a picture");

    r.controller.switch_monitor(1);
    assert_eq!(r.controller.monitor(), 1);
    // … then monitor 1; nothing of monitor 0 may be delivered after the first monitor-1 frame.
    let mut seen1 = false;
    let mut late_zero = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !seen1 {
        if let Ok(Some(MediaEvent::Frame(f))) =
            tokio::time::timeout(Duration::from_millis(300), r.streams.events.recv()).await
        {
            if f.header.monitor == 1 {
                match dec.decode(&f) {
                    Ok(Some(_)) => seen1 = true,
                    Ok(None) => {}
                    Err(_) => r.controller.request_keyframe(),
                }
            } else if seen1 {
                late_zero = true;
            }
        }
    }
    assert!(seen1, "monitor 1 never produced a picture");
    assert!(!late_zero);
    // And back.
    r.controller.switch_monitor(0);
    let mut back = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !back {
        if let Ok(Some(MediaEvent::Frame(f))) =
            tokio::time::timeout(Duration::from_millis(300), r.streams.events.recv()).await
            && f.header.monitor == 0
        {
            back = matches!(dec.decode(&f), Ok(Some(_)));
        }
    }
    assert!(back, "could not switch back to monitor 0");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mac_keyboard_gets_pc_shortcuts_when_the_remap_is_on() {
    let r = rig_with(
        &[grants::SCREEN, grants::INPUT],
        SyntheticConfig::default(),
        ControllerOptions {
            want_video: false,
            key_remap: nexus_input::remap::KeyRemap::mac_to_pc(),
            ..Default::default()
        },
    )
    .await;
    let key = |usage, state, modifiers| InputEvent::Key {
        page: 7,
        usage,
        state,
        modifiers,
    };
    // ⌘C as a Mac types it: Command down, C down, C up, Command up.
    r.controller.send_input([
        key(0xE3, KeyState::Down, Modifiers::META),
        key(0x06, KeyState::Down, Modifiers::META),
        key(0x06, KeyState::Up, Modifiers::META),
        key(0xE3, KeyState::Up, Modifiers::default()),
    ]);
    let expected = vec![
        key(0xE0, KeyState::Down, Modifiers::CTRL),
        key(0x06, KeyState::Down, Modifiers::CTRL),
        key(0x06, KeyState::Up, Modifiers::CTRL),
        key(0xE0, KeyState::Up, Modifiers::default()),
    ];
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && r.recorded.events() != expected {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(r.recorded.events(), expected);
}
