//! Attended access: the first sensitive request in a session asks the person at the target.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::*;
use nexus_capture::synthetic::{SyntheticBackend, SyntheticConfig};
use nexus_input::recording::RecordingInjector;
use nexus_input::{InputEvent, KeyState, Modifiers};
use nexus_media::controller::{ControllerOptions, ControllerSession, MediaEvent, Streams};
use nexus_media::target::{self, Consent, ConsentGate, TargetConfig, TargetStats};
use nexus_trust::grants;

struct Rig {
    controller: ControllerSession,
    streams: Streams,
    asked: Arc<AtomicU32>,
    recorded: nexus_input::recording::Recorded,
    target: tokio::task::JoinHandle<target::EndReason>,
}

async fn rig(answer: Consent, delay: Duration) -> Rig {
    rig_with(answer, delay, ControllerOptions::default()).await
}

async fn rig_with(answer: Consent, delay: Duration, options: ControllerOptions) -> Rig {
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
    let (backend, _probe) = SyntheticBackend::new(SyntheticConfig::default());
    let (injector, recorded) = RecordingInjector::new();
    let injector = std::sync::Mutex::new(Some(injector));
    let asked = Arc::new(AtomicU32::new(0));
    let gate: ConsentGate = {
        let asked = Arc::clone(&asked);
        Arc::new(move || {
            let asked = Arc::clone(&asked);
            Box::pin(async move {
                asked.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                answer
            })
        })
    };
    let config = TargetConfig {
        grants: [grants::SCREEN, grants::INPUT]
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>(),
        capture: Arc::new(backend),
        injector: Some(Arc::new(move || {
            let inj = injector
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| nexus_input::InputError::Failed("one injector".into()))?;
            Ok(Box::new(inj) as Box<dyn nexus_input::Injector>)
        })),
        max_fps: 30,
        max_inflight: 8,
        egress: None,
        files: None,
        clipboard: None,
        consent: Some(gate),
        power: None,
        wake_relay: false,
        terminal: None,
        portfwd: None,
        audit: None,
    };
    let target = tokio::spawn(target::serve(
        b_session,
        eb.control,
        config,
        Arc::new(TargetStats::default()),
    ));
    let (controller, streams) = ControllerSession::start(a_session, ea.control, options)
        .await
        .unwrap();
    std::mem::forget((a, b));
    Rig {
        controller,
        streams,
        asked,
        recorded,
        target,
    }
}

fn key_event() -> InputEvent {
    InputEvent::Key {
        page: 7,
        usage: 4,
        state: KeyState::Down,
        modifiers: Modifiers::default(),
    }
}

async fn first_frame_or_close(streams: &mut Streams) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), streams.events.recv()).await {
            Ok(Some(MediaEvent::Frame(_))) => return Ok(()),
            Ok(Some(MediaEvent::Closed(why))) => return Err(why),
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => return Err("event stream ended".into()),
        }
    }
    Err("timed out".into())
}

#[tokio::test]
async fn an_allowed_session_streams_and_asks_exactly_once() {
    let mut r = rig(Consent::Allowed, Duration::from_millis(50)).await;
    first_frame_or_close(&mut r.streams).await.unwrap();
    r.controller.send_input([key_event()]);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while r.recorded.events().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(r.recorded.events().len(), 1, "input flows once allowed");
    assert_eq!(
        r.asked.load(Ordering::SeqCst),
        1,
        "video + input share one answer"
    );
}

#[tokio::test]
async fn a_denied_session_shows_nothing_and_injects_nothing() {
    let mut r = rig(Consent::Denied, Duration::from_millis(10)).await;
    r.controller.send_input([key_event()]);
    let why = first_frame_or_close(&mut r.streams).await.unwrap_err();
    assert!(
        why.contains("not approved") || why.contains("ended"),
        "{why}"
    );
    assert!(r.recorded.events().is_empty());
    assert_eq!(
        r.target.await.unwrap(),
        target::EndReason::ConsentRefused(Consent::Denied)
    );
}

#[tokio::test]
async fn an_unanswered_prompt_times_out_and_says_so() {
    let mut r = rig(Consent::TimedOut, Duration::from_millis(10)).await;
    let why = first_frame_or_close(&mut r.streams).await.unwrap_err();
    assert!(why.contains("in time") || why.contains("ended"), "{why}");
    assert_eq!(
        r.target.await.unwrap(),
        target::EndReason::ConsentRefused(Consent::TimedOut)
    );
}

#[tokio::test]
async fn diagnostics_never_trigger_the_prompt() {
    let r = rig_with(
        Consent::Denied,
        Duration::from_millis(10),
        ControllerOptions {
            want_video: false,
            ..Default::default()
        },
    )
    .await;
    let mut stream = r
        .controller
        .session()
        .open_stream(nexus_net::StreamKind::Diagnostics)
        .await
        .unwrap();
    // Mode byte 0 = echo, then a payload that must come straight back.
    stream.send.write_all(&[0]).await.unwrap();
    stream.send.write_all(b"ping-ping").await.unwrap();
    let mut back = [0u8; 9];
    tokio::time::timeout(Duration::from_secs(5), stream.recv.read_exact(&mut back))
        .await
        .expect("echo in time")
        .unwrap();
    assert_eq!(&back, b"ping-ping");
    assert_eq!(
        r.asked.load(Ordering::SeqCst),
        0,
        "a ping is not a request for access"
    );
}
