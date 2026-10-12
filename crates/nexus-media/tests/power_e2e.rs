//! Power requests and wake relays over a real QUIC session.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use nexus_capture::synthetic::{SyntheticBackend, SyntheticConfig};
use nexus_media::controller::{ControllerOptions, ControllerSession};
use nexus_media::target::{self, Consent, ConsentGate, TargetConfig, TargetStats};
use nexus_power::{PowerAction, RecordingPower};
use nexus_proto::v1::PowerActionKind;
use nexus_trust::grants;

async fn rig(
    grant_list: &[&str],
    power: Option<RecordingPower>,
    consent: Option<Consent>,
    wake_relay: bool,
) -> ControllerSession {
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
    let gate: Option<ConsentGate> = consent.map(|answer| {
        Arc::new(move || {
            Box::pin(async move { answer })
                as std::pin::Pin<Box<dyn std::future::Future<Output = Consent> + Send>>
        }) as ConsentGate
    });
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
        clipboard: None,
        consent: gate,
        power: power.map(|p| Arc::new(p) as Arc<dyn nexus_power::PowerBackend>),
        wake_relay,
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
    let (controller, streams) = ControllerSession::start(
        a_session,
        ea.control,
        ControllerOptions {
            want_video: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    std::mem::forget((streams, a, b));
    controller
}

#[tokio::test]
async fn a_granted_request_is_performed_after_the_reply() {
    let rec = RecordingPower::new();
    let c = rig(&[grants::POWER], Some(rec.clone()), None, false).await;
    c.power(PowerActionKind::Lock).await.unwrap();
    assert_eq!(rec.actions(), [PowerAction::Lock]);
    // Connection-dropping actions answer first and act a moment later.
    c.power(PowerActionKind::Restart).await.unwrap();
    for _ in 0..50 {
        if rec.actions().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(rec.actions(), [PowerAction::Lock, PowerAction::Restart]);
}

#[tokio::test]
async fn without_the_grant_or_a_backend_nothing_happens() {
    let rec = RecordingPower::new();
    let c = rig(&[grants::SCREEN], Some(rec.clone()), None, false).await;
    assert!(c.power(PowerActionKind::Shutdown).await.is_err());
    let c = rig(&[grants::POWER], None, None, false).await;
    assert!(c.power(PowerActionKind::Shutdown).await.is_err());
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(rec.actions().is_empty());
}

#[tokio::test]
async fn a_person_can_refuse_a_shutdown() {
    let rec = RecordingPower::new();
    let c = rig(
        &[grants::POWER],
        Some(rec.clone()),
        Some(Consent::Denied),
        false,
    )
    .await;
    assert!(c.power(PowerActionKind::Shutdown).await.is_err());
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(rec.actions().is_empty());
}

#[tokio::test]
async fn wake_relays_check_grant_config_and_input() {
    let c = rig(&[grants::SCREEN], None, None, true).await;
    assert!(
        c.wake_relay(vec!["00:1a:2b:3c:4d:5e".into()])
            .await
            .is_err(),
        "no wake grant"
    );
    let c = rig(&[grants::WAKE], None, None, false).await;
    assert!(
        c.wake_relay(vec!["00:1a:2b:3c:4d:5e".into()])
            .await
            .is_err(),
        "relay disabled"
    );
    let c = rig(&[grants::WAKE], None, None, true).await;
    assert!(c.wake_relay(vec![]).await.is_err(), "empty list");
    assert!(
        c.wake_relay(vec!["nonsense".into()]).await.is_err(),
        "bad MAC"
    );
    assert!(
        c.wake_relay((0..9).map(|i| format!("00:1a:2b:3c:4d:{i:02x}")).collect())
            .await
            .is_err(),
        "too many"
    );
}
