//! Physical-screen privacy lifecycle over real encrypted sessions, with a recording OS backend.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use common::*;
use nexus_capture::synthetic::{SyntheticBackend, SyntheticConfig};
use nexus_media::{
    controller::{ControllerOptions, ControllerSession},
    target::{self, Consent, ConsentGate, TargetConfig, TargetStats},
};
use nexus_privacy::{PrivacyBackend, PrivacyError, PrivacyLease};
use nexus_trust::grants;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Recording {
    enabled: Arc<AtomicUsize>,
    restored: Arc<AtomicUsize>,
}
struct Lease(Arc<AtomicUsize>);
impl PrivacyLease for Lease {
    fn renew(&self) -> Result<(), PrivacyError> {
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl PrivacyBackend for Recording {
    fn enable(&self, block: bool) -> Result<Box<dyn PrivacyLease>, PrivacyError> {
        assert!(block);
        self.enabled.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Lease(self.restored.clone())))
    }
}
async fn rig(
    granted: bool,
    consent: Consent,
) -> (
    ControllerSession,
    nexus_media::controller::Streams,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<nexus_net::Node>,
    Arc<nexus_net::Node>,
) {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let b = loopback_node(&ib, &[key(&ia)]).await;
    let (sa, sb) = connect_pair(&a, &b, &loopback_peer(&b)).await;
    let (ha, hb) = (hello(1), hello(2));
    let (ea, eb) = tokio::join!(
        sa.establish(&ha, CONTROLLER, &[nexus_proto::v1::VideoCodec::H264]),
        sb.establish(&hb, TARGET, &[nexus_proto::v1::VideoCodec::H264])
    );
    let (capture, _) = SyntheticBackend::new(SyntheticConfig::default());
    let enabled = Arc::new(AtomicUsize::new(0));
    let restored = Arc::new(AtomicUsize::new(0));
    let gate: ConsentGate = Arc::new(move || Box::pin(async move { consent }));
    let config = TargetConfig {
        encoder: None,
        grants: if granted {
            [grants::PRIVACY.to_owned()].into()
        } else {
            Default::default()
        },
        capture: Arc::new(capture),
        injector: None,
        max_fps: 30,
        max_inflight: 8,
        egress: None,
        files: None,
        clipboard: None,
        consent: Some(gate),
        power: None,
        portfwd: None,
        audit: None,
        terminal: None,
        wake_relay: false,
        audio: None,
        privacy: Some(Arc::new(Recording {
            enabled: enabled.clone(),
            restored: restored.clone(),
        })),
    };
    tokio::spawn(target::serve(
        sb,
        eb.unwrap().control,
        config,
        Arc::new(TargetStats::default()),
    ));
    let (controller, streams) = ControllerSession::start(
        sa,
        ea.unwrap().control,
        ControllerOptions {
            want_video: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (controller, streams, enabled, restored, a, b)
}
#[tokio::test]
async fn privacy_is_granted_confirmed_and_restored_on_disable_and_disconnect() {
    let (c, _streams, enabled, restored, _a, _b) = rig(true, Consent::Allowed).await;
    let state = c.privacy(true, true).await.unwrap();
    assert!(state.enabled && state.local_input_blocked);
    assert_eq!(enabled.load(Ordering::SeqCst), 1);
    c.privacy(true, true).await.unwrap();
    assert_eq!(enabled.load(Ordering::SeqCst), 1, "idempotent enable");
    c.privacy(false, false).await.unwrap();
    assert_eq!(restored.load(Ordering::SeqCst), 1);
    c.privacy(true, true).await.unwrap();
    c.close().await;
    for _ in 0..50 {
        if restored.load(Ordering::SeqCst) == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(restored.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn no_grant_or_consent_cannot_hide_a_screen() {
    for (granted, consent) in [(false, Consent::Allowed), (true, Consent::Denied)] {
        let (c, _streams, enabled, _restored, _a, _b) = rig(granted, consent).await;
        assert!(c.privacy(true, true).await.is_err());
        assert_eq!(enabled.load(Ordering::SeqCst), 0);
        c.close().await;
    }
}
