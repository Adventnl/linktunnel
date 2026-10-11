//! Two devices that have never met pair through the rendezvous, then see each other.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod common;

use std::sync::Arc;

use common::*;
use nexus_crypto::DeviceIdentity;
use nexus_crypto::noise::{PairingCode, ShortAuthString};
use nexus_proto::v1::SignalErrorCode;
use nexus_signal_client::{EventStream, ServerEvent as E, SignalClient};
use nexus_trust::TrustStore;
use nexus_trust::flow::{AbortReason, FlowEvent, PairingFlow, Proposal, ResponderPolicy};
use nexus_trust::grants;

/// Pumps one flow against its rendezvous connection until `until` holds or the flow ends.
/// Returns every event the flow produced.
async fn drive(
    flow: &mut PairingFlow,
    client: &SignalClient,
    events: &mut EventStream,
    now: u64,
    until: impl Fn(&[FlowEvent]) -> bool,
) -> Vec<FlowEvent> {
    let mut seen: Vec<FlowEvent> = Vec::new();
    loop {
        for msg in flow.take_outgoing() {
            client.rendezvous_send(msg).await.unwrap();
        }
        while let Some(ev) = flow.next_event() {
            seen.push(ev);
        }
        if until(&seen) || flow.is_finished() {
            return seen;
        }
        match next_event(events).await {
            E::RendezvousData(data) => {
                let _ = flow.handle_incoming(&data, now);
            }
            E::RendezvousPeer { .. } | E::PairingAdded(_) | E::PresenceUpdate(_) => {}
            other => panic!("unexpected event during pairing: {other:?}"),
        }
    }
}

fn sas(events: &[FlowEvent]) -> Option<ShortAuthString> {
    events.iter().find_map(|e| match e {
        FlowEvent::SasReady { sas, .. } => Some(*sas),
        _ => None,
    })
}

fn completed(events: &[FlowEvent]) -> bool {
    events.iter().any(|e| matches!(e, FlowEvent::Completed(_)))
}

#[tokio::test]
async fn two_unpaired_devices_pair_through_the_rendezvous_and_then_see_each_other() {
    let server = TestServer::start().await;
    let now = server.now();
    let existing: Arc<DeviceIdentity> = device(1); // the MacBook, scans the code
    let newcomer: Arc<DeviceIdentity> = device(2); // the Windows PC, shows the code
    let (c_existing, mut e_existing) = server.connect(&existing).await;
    let (c_new, mut e_new) = server.connect(&newcomer).await;

    // The new device shows a one-time code (QR + words); the existing device enters it.
    let code = PairingCode::generate().unwrap();
    let room = code.rendezvous_id();
    c_new.rendezvous_join(room).await.unwrap();
    c_existing.rendezvous_join(room).await.unwrap();

    let mut initiator = PairingFlow::initiator(
        Arc::clone(&existing),
        "MacBook",
        &code,
        Proposal {
            initiator_on_responder: grants::all(),
            responder_on_initiator: vec![grants::SCREEN.into()],
        },
    )
    .unwrap();
    let mut responder = PairingFlow::responder(
        Arc::clone(&newcomer),
        "Windows PC",
        &code,
        ResponderPolicy {
            max_initiator_on_me: grants::all(),
        },
    )
    .unwrap();

    // Run both devices concurrently up to the point where the humans must compare words.
    let (i_events, r_events) = tokio::join!(
        drive(&mut initiator, &c_existing, &mut e_existing, now, |ev| sas(
            ev
        )
        .is_some()),
        drive(&mut responder, &c_new, &mut e_new, now, |ev| sas(ev)
            .is_some()),
    );
    let (i_sas, r_sas) = (sas(&i_events).unwrap(), sas(&r_events).unwrap());
    assert_eq!(i_sas, r_sas, "both screens must show the same words");

    // Both humans confirm.
    initiator.confirm_sas(true, now).unwrap();
    responder.confirm_sas(true, now).unwrap();
    let (i_done, r_done) = tokio::join!(
        drive(&mut initiator, &c_existing, &mut e_existing, now, completed),
        drive(&mut responder, &c_new, &mut e_new, now, completed),
    );
    let (Some(FlowEvent::Completed(i_pairing)), Some(FlowEvent::Completed(r_pairing))) =
        (i_done.last(), r_done.last())
    else {
        panic!("pairing did not complete: {i_done:?} / {r_done:?}");
    };
    assert_eq!(i_pairing.signed, r_pairing.signed);

    // Each device records the other in its local allow-list...
    let mut existing_trust = TrustStore::in_memory(existing.public().clone());
    let mut new_trust = TrustStore::in_memory(newcomer.public().clone());
    existing_trust.add(&i_pairing.signed).unwrap();
    new_trust.add(&r_pairing.signed).unwrap();
    assert!(existing_trust.is_trusted(&newcomer.public().ed25519_bytes()));
    assert!(new_trust.allows(&existing.public().ed25519_bytes(), grants::INPUT));
    assert!(
        !existing_trust.allows(&newcomer.public().ed25519_bytes(), grants::INPUT),
        "the newcomer was only given view access on the MacBook"
    );

    // ...the initiator registers it with the server, the newcomer is told...
    c_existing.rendezvous_leave().await.unwrap();
    c_existing
        .register_pairing(i_pairing.signed.clone())
        .await
        .unwrap();
    loop {
        match next_event(&mut e_new).await {
            E::PairingAdded(s) => {
                assert_eq!(s, i_pairing.signed);
                break;
            }
            E::RendezvousPeer { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    // ...and from now on they see each other's presence.
    c_new
        .publish_presence(presence("Windows PC", 1))
        .await
        .unwrap();
    let snapshot = c_existing.subscribe_presence().await.unwrap();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].info.as_ref().unwrap().name, "Windows PC");
}

#[tokio::test]
async fn a_server_that_tampers_with_the_pairing_traffic_breaks_it() {
    // The rendezvous is run by an untrusted party. Flip one bit of one message and neither side
    // may end up with a pairing.
    let now = nexus_core::clock::unix_ms();
    let (existing, newcomer) = (device(1), device(2));
    let code = PairingCode::generate().unwrap();
    let mut i =
        PairingFlow::initiator(Arc::clone(&existing), "A", &code, Proposal::default()).unwrap();
    let mut r = PairingFlow::responder(
        Arc::clone(&newcomer),
        "B",
        &code,
        ResponderPolicy::default(),
    )
    .unwrap();

    let m1 = i.take_outgoing().remove(0);
    r.handle_incoming(&m1, now).unwrap();
    let mut m2 = r.take_outgoing().remove(0);
    let mid = m2.len() / 2;
    m2[mid] ^= 0x01; // the "server" flips a bit
    assert!(i.handle_incoming(&m2, now).is_err());
    let mut events = Vec::new();
    while let Some(e) = i.next_event() {
        events.push(e);
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, FlowEvent::Aborted(AbortReason::Handshake)))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, FlowEvent::Completed(_) | FlowEvent::SasReady { .. }))
    );
}

#[tokio::test]
async fn rendezvous_rooms_hold_exactly_two_devices() {
    let server = TestServer::start().await;
    let (a, b, c) = (device(1), device(2), device(3));
    let (ca, mut ea) = server.connect(&a).await;
    let (cb, mut eb) = server.connect(&b).await;
    let (cc, _ec) = server.connect(&c).await;
    let room = [7u8; 16];

    // Sending before the peer arrives is an error, not a silent drop.
    ca.rendezvous_join(room).await.unwrap();
    assert_eq!(
        ca.rendezvous_send(vec![1]).await.unwrap_err().server_code(),
        Some(SignalErrorCode::PeerAbsent)
    );

    cb.rendezvous_join(room).await.unwrap();
    assert_eq!(
        next_event(&mut ea).await,
        E::RendezvousPeer { joined: true }
    );
    assert_eq!(
        next_event(&mut eb).await,
        E::RendezvousPeer { joined: true }
    );

    // Relayed both ways, byte for byte.
    ca.rendezvous_send(vec![1, 2, 3]).await.unwrap();
    assert_eq!(next_event(&mut eb).await, E::RendezvousData(vec![1, 2, 3]));
    cb.rendezvous_send(vec![9; 100]).await.unwrap();
    assert_eq!(next_event(&mut ea).await, E::RendezvousData(vec![9; 100]));

    // A third device cannot eavesdrop or inject.
    assert_eq!(
        cc.rendezvous_join(room).await.unwrap_err().server_code(),
        Some(SignalErrorCode::RoomFull)
    );
    assert_eq!(
        cc.rendezvous_send(vec![6]).await.unwrap_err().server_code(),
        Some(SignalErrorCode::NotFound)
    );

    // Leaving tells the other side.
    cb.rendezvous_leave().await.unwrap();
    assert_eq!(
        next_event(&mut ea).await,
        E::RendezvousPeer { joined: false }
    );
    assert_eq!(
        ca.rendezvous_send(vec![1]).await.unwrap_err().server_code(),
        Some(SignalErrorCode::PeerAbsent)
    );
}

#[tokio::test]
async fn rendezvous_abuse_is_bounded() {
    let server = TestServer::start().await;
    let (ca, _ea) = server.connect(&device(1)).await;
    let (cb, _eb) = server.connect(&device(2)).await;
    ca.rendezvous_join([1; 16]).await.unwrap();
    cb.rendezvous_join([1; 16]).await.unwrap();
    // Oversized relay payloads are refused.
    assert_eq!(
        ca.rendezvous_send(vec![0; 17 * 1024])
            .await
            .unwrap_err()
            .server_code(),
        Some(SignalErrorCode::PayloadTooLarge)
    );
    // Guessing codes is rate limited: 10 joins per minute per device.
    let mut limited = false;
    for i in 2u8..40 {
        if let Err(e) = ca.rendezvous_join([i; 16]).await
            && e.server_code() == Some(SignalErrorCode::RateLimited)
        {
            limited = true;
            break;
        }
    }
    assert!(limited, "room guessing is not rate limited");
}

#[tokio::test]
async fn rooms_expire_and_are_cleaned_up() {
    let server = TestServer::start().await;
    let (ca, _ea) = server.connect(&device(1)).await;
    ca.rendezvous_join([5; 16]).await.unwrap();
    assert_eq!(server.hub.room_count(), 1);
    server.advance(299_000);
    server.hub.reap_presence().await;
    assert_eq!(server.hub.room_count(), 1);
    server.advance(2_000);
    server.hub.reap_presence().await;
    assert_eq!(
        server.hub.room_count(),
        0,
        "stale rooms must not accumulate"
    );
}
