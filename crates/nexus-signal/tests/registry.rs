//! Pairing registry, presence visibility, revocation and sealed envelopes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod common;

use std::time::Duration;

use common::*;
use nexus_crypto::noise::envelope::{self, CounterSource, Envelope};
use nexus_crypto::noise::replay::ReplayGuard;
use nexus_proto::v1::{GoingOfflineReason, PresenceState, SignalErrorCode, SignedPairingRecord};
use nexus_signal_client::ServerEvent as E;
use nexus_trust::{grants, record, revocation};

fn code(e: &nexus_signal_client::ClientError) -> SignalErrorCode {
    e.server_code()
        .unwrap_or_else(|| panic!("not a server error: {e:?}"))
}

fn state(p: &nexus_proto::v1::DevicePresence) -> PresenceState {
    PresenceState::try_from(p.info.as_ref().unwrap().state).unwrap()
}

// ---- pairing registration -------------------------------------------------------------------

#[tokio::test]
async fn a_valid_dual_signed_record_is_accepted_and_pushed_to_the_peer() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, _ea) = server.connect(&a).await;
    let (cb, mut eb) = server.connect(&b).await;

    let signed = pair(&server, &ca, &a, &b).await;
    assert_eq!(next_event(&mut eb).await, E::PairingAdded(signed.clone()));
    assert_eq!(ca.list_pairings().await.unwrap(), vec![signed.clone()]);
    assert_eq!(cb.list_pairings().await.unwrap(), vec![signed]);
}

#[tokio::test]
async fn a_record_with_only_one_signature_is_rejected() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, _e) = server.connect(&a).await;
    let g = grants::all();
    let bytes =
        record::build_record(a.public(), "A", b.public(), "B", &g, &g, server.now()).unwrap();
    let sa = record::sign_record(&a, &bytes);
    // A claims B agreed, signing B's slot with its own signature.
    let forged = record::assemble(&bytes, a.public(), &sa, &sa).unwrap();
    let err = ca.register_pairing(forged).await.unwrap_err();
    assert_eq!(code(&err), SignalErrorCode::InvalidRequest);
    assert!(ca.list_pairings().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_third_party_cannot_register_a_pairing_between_two_other_devices() {
    let server = TestServer::start().await;
    let (a, b, mallory) = (device(1), device(2), device(3));
    let (cm, _e) = server.connect(&mallory).await;
    // A genuine, fully signed A–B record that Mallory somehow obtained.
    let genuine = signed_pairing(&a, &b, server.now());
    let err = cm.register_pairing(genuine).await.unwrap_err();
    assert_eq!(
        code(&err),
        SignalErrorCode::InvalidRequest,
        "caller must be a party"
    );
}

#[tokio::test]
async fn expired_and_future_records_are_rejected() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, _e) = server.connect(&a).await;
    for at in [
        server.now() - record::MAX_REGISTRATION_AGE_MS - 1,
        server.now() + record::MAX_FUTURE_SKEW_MS + 1,
    ] {
        let err = ca
            .register_pairing(signed_pairing(&a, &b, at))
            .await
            .unwrap_err();
        assert_eq!(
            code(&err),
            SignalErrorCode::InvalidRequest,
            "created_at={at}"
        );
    }
}

#[tokio::test]
async fn garbage_records_are_rejected() {
    let server = TestServer::start().await;
    let (ca, _e) = server.connect(&device(1)).await;
    let err = ca
        .register_pairing(SignedPairingRecord {
            record: vec![1, 2, 3].into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(&err), SignalErrorCode::InvalidRequest);
}

// ---- presence visibility --------------------------------------------------------------------

#[tokio::test]
async fn paired_devices_see_each_others_presence() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, _ea) = server.connect(&a).await;
    let (cb, mut eb) = server.connect(&b).await;
    pair(&server, &ca, &a, &b).await;
    let _ = next_event(&mut eb).await; // PairingAdded

    ca.publish_presence(presence("MacBook", 1)).await.unwrap();
    assert!(
        cb.subscribe_presence()
            .await
            .unwrap()
            .iter()
            .any(|d| d.info.as_ref().unwrap().name == "MacBook")
    );

    ca.publish_presence(nexus_proto::v1::PresenceInfo {
        state: PresenceState::Locked as i32,
        ..presence("MacBook", 2)
    })
    .await
    .unwrap();
    match next_event(&mut eb).await {
        E::PresenceUpdate(p) => {
            assert!(p.online);
            assert_eq!(state(&p), PresenceState::Locked);
            assert_eq!(p.device_id.as_ref(), a.device_id().as_bytes());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn unpaired_devices_learn_nothing_about_a_device() {
    let server = TestServer::start().await;
    let (a, b, stranger) = (device(1), device(2), device(3));
    let (ca, _ea) = server.connect(&a).await;
    let (cb, _eb) = server.connect(&b).await;
    let (cs, mut es) = server.connect(&stranger).await;
    pair(&server, &ca, &a, &b).await;
    ca.publish_presence(presence("MacBook", 1)).await.unwrap();

    assert!(
        cs.subscribe_presence().await.unwrap().is_empty(),
        "snapshot must exclude unpaired devices"
    );
    ca.publish_presence(presence("MacBook", 2)).await.unwrap();
    no_event(&mut es).await;
    // And B, who *is* paired, still sees it.
    assert_eq!(cb.subscribe_presence().await.unwrap().len(), 1);
}

#[tokio::test]
async fn abrupt_disconnect_marks_the_device_offline_immediately() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, ea) = server.connect(&a).await;
    let (cb, mut eb) = server.connect(&b).await;
    pair(&server, &ca, &a, &b).await;
    let _ = next_event(&mut eb).await;
    ca.publish_presence(presence("A", 1)).await.unwrap();
    cb.subscribe_presence().await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(200), eb.next()).await; // drain any update

    ca.close().await;
    drop(ea);
    loop {
        if let E::PresenceUpdate(p) = next_event(&mut eb).await
            && !p.online
        {
            assert_eq!(state(&p), PresenceState::Offline);
            break;
        }
    }
}

#[tokio::test]
async fn going_to_sleep_is_reported_as_sleeping_not_offline() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, _ea) = server.connect(&a).await;
    let (cb, mut eb) = server.connect(&b).await;
    pair(&server, &ca, &a, &b).await;
    let _ = next_event(&mut eb).await;
    cb.subscribe_presence().await.unwrap();
    ca.publish_presence(presence("A", 1)).await.unwrap();
    let _ = next_event(&mut eb).await;

    ca.going_offline(GoingOfflineReason::Sleep).await.unwrap();
    match next_event(&mut eb).await {
        E::PresenceUpdate(p) => {
            assert!(!p.online);
            assert_eq!(state(&p), PresenceState::Sleeping);
        }
        other => panic!("unexpected {other:?}"),
    }
    // The socket then drops (the machine suspends); the sleeping state must survive that.
    ca.close().await;
    no_event(&mut eb).await;
    let snapshot = cb.subscribe_presence().await.unwrap();
    assert_eq!(state(&snapshot[0]), PresenceState::Sleeping);
}

#[tokio::test]
async fn silent_devices_expire_after_the_presence_ttl() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, _ea) = server.connect(&a).await;
    let (cb, mut eb) = server.connect(&b).await;
    pair(&server, &ca, &a, &b).await;
    let _ = next_event(&mut eb).await;
    cb.subscribe_presence().await.unwrap();
    ca.publish_presence(presence("A", 1)).await.unwrap();
    let _ = next_event(&mut eb).await;

    server.advance(14_000);
    server.hub.reap_presence().await;
    no_event(&mut eb).await; // still within the 15 s TTL
    server.advance(2_000);
    server.hub.reap_presence().await;
    match next_event(&mut eb).await {
        E::PresenceUpdate(p) => assert!(!p.online && state(&p) == PresenceState::Offline),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn heartbeats_keep_a_device_online_and_stale_updates_are_ignored() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, _ea) = server.connect(&a).await;
    let (cb, mut eb) = server.connect(&b).await;
    pair(&server, &ca, &a, &b).await;
    let _ = next_event(&mut eb).await;
    cb.subscribe_presence().await.unwrap();
    ca.publish_presence(presence("new", 5)).await.unwrap();
    let _ = next_event(&mut eb).await;

    for _ in 0..3 {
        server.advance(10_000);
        ca.heartbeat(0).await.unwrap();
        server.hub.reap_presence().await;
    }
    no_event(&mut eb).await;
    // A reordered older update must not overwrite newer state.
    ca.publish_presence(presence("old", 3)).await.unwrap();
    no_event(&mut eb).await;
    assert_eq!(
        cb.subscribe_presence().await.unwrap()[0]
            .info
            .as_ref()
            .unwrap()
            .name,
        "new"
    );
}

#[tokio::test]
async fn oversized_or_invalid_presence_is_rejected() {
    let server = TestServer::start().await;
    let (ca, _e) = server.connect(&device(1)).await;
    let mut p = presence("x", 1);
    p.name = "n".repeat(65);
    assert_eq!(
        code(&ca.publish_presence(p).await.unwrap_err()),
        SignalErrorCode::InvalidRequest
    );
    let mut p = presence("x", 1);
    p.wake_macs = vec![vec![1, 2, 3].into()];
    assert_eq!(
        code(&ca.publish_presence(p).await.unwrap_err()),
        SignalErrorCode::InvalidRequest
    );
    let mut p = presence("x", 1);
    p.state = PresenceState::Sleeping as i32; // clients cannot self-declare "sleeping"
    assert_eq!(
        code(&ca.publish_presence(p).await.unwrap_err()),
        SignalErrorCode::InvalidRequest
    );
    let mut p = presence("x", 1);
    p.ip_hints = (0..9).map(|i| format!("10.0.0.{i}")).collect();
    assert_eq!(
        code(&ca.publish_presence(p).await.unwrap_err()),
        SignalErrorCode::InvalidRequest
    );
}

// ---- revocation -----------------------------------------------------------------------------

#[tokio::test]
async fn revocation_ends_visibility_and_messaging_in_both_directions() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, mut ea) = server.connect(&a).await;
    let (cb, mut eb) = server.connect(&b).await;
    let signed = pair(&server, &ca, &a, &b).await;
    let _ = next_event(&mut eb).await;
    cb.subscribe_presence().await.unwrap();
    ca.publish_presence(presence("A", 1)).await.unwrap();
    let _ = next_event(&mut eb).await;

    // B revokes.
    let rev = revocation::revoke(&b, a.public(), server.now());
    cb.revoke_pairing(rev.clone()).await.unwrap();
    match next_event(&mut ea).await {
        E::PairingRevoked { peer, signed } => {
            assert_eq!(peer, b.device_id());
            assert_eq!(signed, rev);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(ca.list_pairings().await.unwrap().is_empty());

    // Presence no longer flows...
    ca.publish_presence(presence("A", 2)).await.unwrap();
    no_event(&mut eb).await;
    assert!(cb.subscribe_presence().await.unwrap().is_empty());
    // ...nor envelopes, either way.
    assert_eq!(
        code(
            &ca.send_envelope(b.device_id(), vec![1; 100])
                .await
                .unwrap_err()
        ),
        SignalErrorCode::NotPaired
    );
    assert_eq!(
        code(
            &cb.send_envelope(a.device_id(), vec![1; 100])
                .await
                .unwrap_err()
        ),
        SignalErrorCode::NotPaired
    );
    // Re-registering the old record does not bring it back.
    assert_eq!(
        code(&ca.register_pairing(signed).await.unwrap_err()),
        SignalErrorCode::Conflict
    );
}

#[tokio::test]
async fn only_the_revoker_can_submit_its_revocation() {
    let server = TestServer::start().await;
    let (a, b, mallory) = (device(1), device(2), device(3));
    let (ca, _ea) = server.connect(&a).await;
    let (cm, _em) = server.connect(&mallory).await;
    pair(&server, &ca, &a, &b).await;

    // Mallory submits a genuine B→A revocation she obtained, or a self-made one.
    let genuine = revocation::revoke(&b, a.public(), server.now());
    assert_eq!(
        code(&cm.revoke_pairing(genuine).await.unwrap_err()),
        SignalErrorCode::InvalidRequest
    );
    let bogus = revocation::revoke(&mallory, a.public(), server.now());
    assert_eq!(
        code(&cm.revoke_pairing(bogus).await.unwrap_err()),
        SignalErrorCode::NotFound
    );
    assert_eq!(
        ca.list_pairings().await.unwrap().len(),
        1,
        "pairing untouched"
    );
}

// ---- sealed envelopes -----------------------------------------------------------------------

#[tokio::test]
async fn a_sealed_envelope_travels_end_to_end_and_the_server_cannot_read_it() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, _ea) = server.connect(&a).await;
    let (_cb, mut eb) = server.connect(&b).await;
    pair(&server, &ca, &a, &b).await;
    let _ = next_event(&mut eb).await;

    let counters = CounterSource::new();
    let msg = Envelope {
        kind: 1,
        counter: counters.next(),
        unix_ms: server.now(),
        body: b"wake 00:11:22:33:44:55".to_vec(),
    };
    let sealed = envelope::seal(&a, b.public(), &msg).unwrap();
    ca.send_envelope(b.device_id(), sealed.clone())
        .await
        .unwrap();

    match next_event(&mut eb).await {
        E::Envelope {
            from, sealed: got, ..
        } => {
            assert_eq!(from, a.device_id());
            assert_eq!(got, sealed, "the server relays bytes unchanged");
            let mut guard = ReplayGuard::default();
            let peers = [a.public().clone()];
            let (peer, opened) =
                envelope::open_from_peers(&b, &peers, &mut guard, &got, server.now()).unwrap();
            assert_eq!(peer, a.public());
            assert_eq!(opened.body, b"wake 00:11:22:33:44:55");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn envelopes_to_unpaired_or_unknown_devices_are_indistinguishable() {
    let server = TestServer::start().await;
    let (a, b, stranger) = (device(1), device(2), device(3));
    let (ca, _ea) = server.connect(&a).await;
    let (_cs, _es) = server.connect(&stranger).await; // exists and is online, but unpaired
    let _ = b;
    let unpaired = ca
        .send_envelope(stranger.device_id(), vec![1; 50])
        .await
        .unwrap_err();
    let unknown = ca
        .send_envelope(device(99).device_id(), vec![1; 50])
        .await
        .unwrap_err();
    assert_eq!(code(&unpaired), SignalErrorCode::NotPaired);
    assert_eq!(code(&unknown), SignalErrorCode::NotPaired);
}

#[tokio::test]
async fn offline_recipient_oversize_and_rate_limits() {
    let server = TestServer::start().await;
    let (a, b) = (device(1), device(2));
    let (ca, _ea) = server.connect(&a).await;
    let (cb, eb) = server.connect(&b).await;
    pair(&server, &ca, &a, &b).await;

    assert_eq!(
        code(
            &ca.send_envelope(b.device_id(), vec![0; 20_000])
                .await
                .unwrap_err()
        ),
        SignalErrorCode::PayloadTooLarge
    );

    cb.close().await;
    drop(eb);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        code(
            &ca.send_envelope(b.device_id(), vec![1; 10])
                .await
                .unwrap_err()
        ),
        SignalErrorCode::RecipientOffline
    );

    // Burst budget (30) then throttling.
    let mut limited = false;
    for _ in 0..40 {
        if let Err(e) = ca.send_envelope(b.device_id(), vec![1; 10]).await
            && code(&e) == SignalErrorCode::RateLimited
        {
            limited = true;
            break;
        }
    }
    assert!(limited, "envelope flood was not rate limited");
}

#[tokio::test]
async fn the_server_survives_a_malformed_stream() {
    let server = TestServer::start().await;
    let a = device(1);
    let (nonce, audience);
    let mut raw = server.raw().await;
    (nonce, audience) = raw.hello().await;
    raw.send(RawClient::auth_message(&a, &audience, &nonce, &[4; 16]))
        .await;
    let _ = raw.recv().await;
    // Garbage protobuf, then an oversize frame: the connection dies, the server does not.
    use futures_util::SinkExt;
    let _ = raw
        .ws
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            vec![0xff; 40].into(),
        ))
        .await;
    assert!(raw.closed().await);
    let (ok, _e) = server.connect(&device(2)).await;
    ok.close().await;
}
