//! The whole product path on a real (virtual) desktop: Xvfb screen → XDamage capture → H.264 →
//! QUIC → decode on the controller, and mouse input back into the X server.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use nexus_capture::x11::X11Backend;
use nexus_input::x11::X11Injector;
use nexus_input::{InputEvent, MouseButton};
use nexus_media::controller::{ControllerOptions, ControllerSession, MediaEvent};
use nexus_media::target::{self, TargetConfig, TargetStats};
use nexus_media::viewer::{CursorOverlay, Picture, VideoDecoder};
use nexus_testkit::{Painter, Xvfb};
use nexus_trust::grants;

fn rgb(p: &Picture, x: usize, y: usize) -> [u8; 3] {
    let i = (y * p.width as usize + x) * 4;
    [p.bgra[i + 2], p.bgra[i + 1], p.bgra[i]]
}

fn close(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) < 40)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_real_x_desktop_is_viewed_and_controlled_over_quic() {
    let Some(xvfb) = Xvfb::start(800, 600) else {
        return;
    };
    let painter = Painter::connect(xvfb.display());
    painter.fill(0, 0, 800, 600, 0x2040c0); // blue desktop

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

    let display = xvfb.display().to_owned();
    let config = TargetConfig {
        grants: [grants::SCREEN, grants::INPUT]
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>(),
        capture: Arc::new(X11Backend::connect(Some(&display)).unwrap()),
        injector: Some(Arc::new(move || {
            Ok(Box::new(X11Injector::connect(Some(&display))?) as Box<dyn nexus_input::Injector>)
        })),
        max_fps: 60,
        max_inflight: 12,
        egress: None,
        files: None,
        clipboard: None,
        consent: None,
        power: None,
        wake_relay: false,
        terminal: None,
    };
    let stats = Arc::new(TargetStats::default());
    let target_task = tokio::spawn(target::serve(
        b_session,
        eb.control,
        config,
        Arc::clone(&stats),
    ));
    let (controller, mut streams) =
        ControllerSession::start(a_session, ea.control, ControllerOptions::default())
            .await
            .unwrap();

    let mut dec = VideoDecoder::new();
    let mut overlay = CursorOverlay::new();
    let mut started = None;

    async fn until(
        streams: &mut nexus_media::controller::Streams,
        controller: &ControllerSession,
        dec: &mut VideoDecoder,
        overlay: &mut CursorOverlay,
        started: &mut Option<nexus_proto::v1::VideoStarted>,
        what: &str,
        pred: impl Fn(&Picture) -> bool,
    ) -> Picture {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            let Ok(Some(ev)) =
                tokio::time::timeout(Duration::from_millis(300), streams.events.recv()).await
            else {
                continue;
            };
            match ev {
                MediaEvent::Started(s) => *started = Some(s),
                MediaEvent::CursorShape(c) => overlay.add_shape(c),
                MediaEvent::Frame(f) => match dec.decode(&f) {
                    Ok(Some(p)) if pred(&p) => return p,
                    Ok(_) => {}
                    Err(_) => controller.request_keyframe(),
                },
                _ => {}
            }
        }
        panic!("timed out waiting for: {what}");
    }

    let first = until(
        &mut streams,
        &controller,
        &mut dec,
        &mut overlay,
        &mut started,
        "blue desktop",
        |p| close(rgb(p, 400, 300), [0x20, 0x40, 0xc0]),
    )
    .await;
    assert_eq!((first.width, first.height), (800, 600));
    assert_eq!(started.as_ref().unwrap().width, 800);

    // Something changes on the target: a red window appears.
    painter.fill(300, 200, 200, 100, 0xd02020);
    let t0 = Instant::now();
    let p = until(
        &mut streams,
        &controller,
        &mut dec,
        &mut overlay,
        &mut started,
        "red rectangle",
        |p| close(rgb(p, 400, 250), [0xd0, 0x20, 0x20]),
    )
    .await;
    println!(
        "X11 draw → decoded on controller: {:?} (software H.264, 800x600, debug build)",
        t0.elapsed()
    );
    assert!(
        close(rgb(&p, 50, 50), [0x20, 0x40, 0xc0]),
        "the rest of the desktop is unchanged"
    );
    assert!(
        !p.dirty.is_empty(),
        "XDamage rectangles reach the controller"
    );

    // Control: move the mouse from the controller and watch the real X pointer follow.
    controller.send_input([InputEvent::MouseMoveAbs {
        monitor: 0,
        x: 250,
        y: 150,
    }]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while painter.pointer() != (250, 150) {
        assert!(
            Instant::now() < deadline,
            "pointer never reached (250,150); at {:?}",
            painter.pointer()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The controller sees the pointer too, through the cursor channel.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if streams
            .cursor
            .borrow()
            .as_ref()
            .is_some_and(|c| c.x == 250 && c.y == 150)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cursor channel never reported (250,150): {:?}",
            streams.cursor.borrow()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    controller.send_input([
        InputEvent::MouseButton {
            button: MouseButton::LEFT,
            pressed: true,
        },
        InputEvent::MouseButton {
            button: MouseButton::LEFT,
            pressed: false,
        },
    ]);

    controller.close().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), target_task)
        .await
        .unwrap();
}
