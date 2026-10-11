//! Login: only a holder of the device key, for this server, once.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod common;

use common::*;
use nexus_crypto::DeviceIdentity;
use nexus_proto::v1::{
    PresenceInfo, PublishPresence, SignalClientMessage, SignalErrorCode,
    signal_client_message::Body as C, signal_server_message::Body as S,
};
use nexus_signal_client::{ClientConfig, ClientError};
use std::sync::Arc;

#[tokio::test]
async fn login_succeeds_and_reports_server_parameters() {
    let server = TestServer::start().await;
    let id = device(1);
    let (client, _events) = server.connect(&id).await;
    assert_eq!(client.device_id(), id.device_id());
    assert_eq!(client.params().heartbeat_interval.as_secs(), 5);
    assert_eq!(client.params().presence_ttl.as_secs(), 15);
}

#[tokio::test]
async fn nothing_works_before_login() {
    let server = TestServer::start().await;
    let mut raw = server.raw().await;
    raw.hello().await;
    raw.send(SignalClientMessage {
        id: 5,
        body: Some(C::PublishPresence(PublishPresence {
            info: Some(PresenceInfo::default()),
        })),
    })
    .await;
    match raw.recv().await.and_then(|m| m.body) {
        Some(S::Error(e)) => assert_eq!(e.code, SignalErrorCode::Unauthenticated as i32),
        other => panic!("expected an error, got {other:?}"),
    }
    assert!(
        raw.closed().await,
        "connection must be closed after an unauthenticated request"
    );
}

#[tokio::test]
async fn proof_made_for_another_server_is_rejected() {
    let server = TestServer::start().await;
    let id = device(1);
    let mut raw = server.raw().await;
    let (nonce, _real_audience) = raw.hello().await;
    // A valid signature, but over the name of some *other* server.
    raw.send(RawClient::auth_message(
        &id,
        "evil.example.com",
        &nonce,
        &[7; 16],
    ))
    .await;
    match raw.recv().await.and_then(|m| m.body) {
        Some(S::Error(e)) => assert_eq!(e.code, SignalErrorCode::Unauthenticated as i32),
        other => panic!("expected rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn a_recorded_login_cannot_be_replayed_on_a_new_connection() {
    let server = TestServer::start().await;
    let id = device(1);

    // Capture a legitimate login...
    let mut first = server.raw().await;
    let (nonce1, audience) = first.hello().await;
    let proof = RawClient::auth_message(&id, &audience, &nonce1, &[9; 16]);
    first.send(proof.clone()).await;
    assert!(matches!(
        first.recv().await.and_then(|m| m.body),
        Some(S::AuthOk(_))
    ));

    // ...and replay the exact bytes on another connection, where the server nonce differs.
    let mut second = server.raw().await;
    second.hello().await;
    second.send(proof).await;
    match second.recv().await.and_then(|m| m.body) {
        Some(S::Error(e)) => assert_eq!(e.code, SignalErrorCode::Unauthenticated as i32),
        other => panic!("replay was accepted: {other:?}"),
    }
}

#[tokio::test]
async fn a_signature_by_a_different_key_is_rejected() {
    let server = TestServer::start().await;
    let (victim, attacker) = (device(1), device(2));
    let mut raw = server.raw().await;
    let (nonce, audience) = raw.hello().await;
    // Claims the victim's identity but signs with the attacker's key.
    let mut msg = RawClient::auth_message(&attacker, &audience, &nonce, &[3; 16]);
    if let Some(C::Auth(a)) = msg.body.as_mut() {
        a.identity = victim.public().to_bytes().to_vec().into();
    }
    raw.send(msg).await;
    match raw.recv().await.and_then(|m| m.body) {
        Some(S::Error(e)) => assert_eq!(e.code, SignalErrorCode::Unauthenticated as i32),
        other => panic!("impersonation succeeded: {other:?}"),
    }
}

#[tokio::test]
async fn malformed_logins_are_rejected() {
    let server = TestServer::start().await;
    let id = device(1);
    for tweak in 0..4 {
        let mut raw = server.raw().await;
        let (nonce, audience) = raw.hello().await;
        let mut msg = RawClient::auth_message(&id, &audience, &nonce, &[5; 16]);
        if let Some(C::Auth(a)) = msg.body.as_mut() {
            match tweak {
                0 => a.identity = vec![1, 2, 3].into(),
                1 => a.client_nonce = vec![1; 3].into(),
                2 => a.signature = vec![0; 10].into(),
                _ => {
                    a.version = Some(nexus_proto::v1::ProtocolVersion {
                        major: 99,
                        minor: 0,
                    })
                }
            }
        }
        raw.send(msg).await;
        assert!(
            matches!(raw.recv().await.and_then(|m| m.body), Some(S::Error(_))),
            "variant {tweak} was accepted"
        );
    }
}

#[tokio::test]
async fn too_many_failed_logins_lock_out_the_source_address() {
    let server = TestServer::start().await;
    let id = device(1);
    // The default budget is 10 failures per minute per IP.
    for _ in 0..10 {
        let mut raw = server.raw().await;
        let (nonce, _) = raw.hello().await;
        raw.send(RawClient::auth_message(&id, "wrong", &nonce, &[1; 16]))
            .await;
        let _ = raw.recv().await;
    }
    // Even a *valid* login from this address is now refused.
    let mut raw = server.raw().await;
    raw.hello().await;
    match raw.recv().await.and_then(|m| m.body) {
        Some(S::Error(e)) => assert_eq!(e.code, SignalErrorCode::RateLimited as i32),
        other => panic!("expected lockout, got {other:?}"),
    }
}

#[tokio::test]
async fn client_refuses_a_server_claiming_a_different_name() {
    let server = TestServer::start_with(|c| c.audience = "someone-else.example".into()).await;
    let id = device(1);
    let cfg = ClientConfig::new(server.url());
    let err = nexus_signal_client::connect(&cfg, Arc::clone(&id) as Arc<DeviceIdentity>)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::AudienceMismatch { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn second_login_supersedes_the_first_without_marking_the_device_offline() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (a1, mut a1_events) = server.connect(&a).await;
    let (b_client, mut b_events) = server.connect(&b).await;
    pair(&server, &a1, &a, &b).await;
    b_client.subscribe_presence().await.unwrap();
    a1.publish_presence(presence("A", 1)).await.unwrap();
    let _ = next_event(&mut b_events).await; // PairingAdded
    let _ = next_event(&mut b_events).await; // presence

    let (a2, _a2_events) = server.connect(&a).await;
    assert_eq!(a1_events_next(&mut a1_events).await, "superseded");
    // The old connection's teardown must not flip the live device to offline.
    a2.heartbeat(1).await.ok();
    no_offline(&mut b_events).await;
}

async fn a1_events_next(events: &mut nexus_signal_client::EventStream) -> &'static str {
    use nexus_signal_client::ServerEvent as E;
    loop {
        match next_event(events).await {
            E::Superseded => return "superseded",
            E::Disconnected(_) => return "disconnected",
            _ => {}
        }
    }
}

async fn no_offline(events: &mut nexus_signal_client::EventStream) {
    use nexus_signal_client::ServerEvent as E;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(400);
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, events.next()).await {
        if let E::PresenceUpdate(p) = ev {
            assert!(
                p.online,
                "old connection's cleanup marked the device offline"
            );
        }
    }
}
