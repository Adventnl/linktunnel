//! Clipboard sync across a real QUIC session, with the grant check in the path.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use nexus_capture::synthetic::{SyntheticBackend, SyntheticConfig};
use nexus_clipboard::{
    ClipContent, ClipboardBackend, ClipboardPolicy, ClipboardSetup, MemoryClipboard,
};
use nexus_media::controller::{ControllerOptions, ControllerSession};
use nexus_media::target::{self, TargetConfig, TargetStats};
use nexus_trust::grants;

fn setup(mem: &MemoryClipboard) -> ClipboardSetup {
    let mem = mem.clone();
    ClipboardSetup {
        factory: Arc::new(move || Ok(Box::new(mem.clone()) as Box<dyn ClipboardBackend>)),
        policy: ClipboardPolicy {
            poll: Duration::from_millis(20),
            ..Default::default()
        },
    }
}

async fn rig(
    grant_list: &[&str],
    controller_clip: &MemoryClipboard,
    target_clip: &MemoryClipboard,
) {
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
    let config = TargetConfig {
        encoder: None,
        privacy: None,
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
        clipboard: Some(setup(target_clip)),
        consent: None,
        power: None,
        wake_relay: false,
        audio: None,
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
    let options = ControllerOptions {
        clipboard: Some(setup(controller_clip)),
        want_video: false,
        ..Default::default()
    };
    let (controller, streams) = ControllerSession::start(a_session, ea.control, options)
        .await
        .unwrap();
    std::mem::forget((controller, streams, a, b));
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..300 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn copies_cross_the_session_in_both_directions() {
    let (c, t) = (MemoryClipboard::new(), MemoryClipboard::new());
    rig(&[grants::CLIPBOARD], &c, &t).await;
    tokio::time::sleep(Duration::from_millis(300)).await; // both sides snapshot first
    c.set(ClipContent::text("from the MacBook"));
    until("target to get it", || {
        t.get() == ClipContent::text("from the MacBook")
    })
    .await;
    t.set(ClipContent::text("from the PC — 你好"));
    until("controller to get it", || {
        c.get() == ClipContent::text("from the PC — 你好")
    })
    .await;
}

#[tokio::test]
async fn without_the_clipboard_grant_nothing_crosses() {
    let (c, t) = (MemoryClipboard::new(), MemoryClipboard::new());
    rig(&[grants::SCREEN], &c, &t).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    c.set(ClipContent::text("secret"));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(t.get().is_empty());
    assert_eq!(t.sync_writes(), 0);
}
