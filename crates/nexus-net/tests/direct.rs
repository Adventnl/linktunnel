//! Direct (LAN / loopback) paths: authentication, allow-listing, revocation, diagnostics.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::*;
use nexus_net::{CLOSE_NOT_PAIRED, CLOSE_REVOKED, NetError, PeerAddr, StreamKind, diag};
use nexus_proto::v1::{PathKind, VideoCodec};

#[tokio::test]
async fn paired_devices_connect_negotiate_and_see_a_lan_path() {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let b = loopback_node(&ib, &[key(&ia)]).await;
    let (a_session, b_session) = connect_pair(&a, &b, &loopback_peer(&b)).await;

    // Each side authenticated the *other's* identity key via TLS.
    assert_eq!(a_session.peer(), key(&ib));
    assert_eq!(b_session.peer(), key(&ia));
    assert!(a_session.dialed() && !b_session.dialed());
    assert_eq!(
        a_session.path().kind,
        PathKind::Lan,
        "{:?}",
        a_session.path()
    );

    // Hello exchange + negotiation, run concurrently as the real peers would.
    let (ha, hb) = (hello(1), hello(2));
    let (ea, eb) = tokio::join!(
        a_session.establish(&ha, CONTROLLER, &[VideoCodec::H264]),
        b_session.establish(&hb, TARGET, &[VideoCodec::H264]),
    );
    let (ea, eb) = (ea.unwrap(), eb.unwrap());
    assert_eq!(ea.negotiated, eb.negotiated);
    assert_eq!(ea.negotiated.video_codec, Some(VideoCodec::H264));
    assert!(ea.negotiated.has(nexus_proto::features::TERMINAL_PTY));
    assert_eq!(ea.peer_hello.session_nonce.as_ref(), [2u8; 16]);
}

#[tokio::test]
async fn an_unpaired_device_cannot_get_a_session() {
    let (ia, ib, stranger) = (identity(1), identity(2), identity(3));
    let _a = loopback_node(&ia, &[key(&ib)]).await;
    let b = loopback_node(&ib, &[key(&ia)]).await; // stranger is NOT allowed by B
    let s = loopback_node(&stranger, &[key(&ib)]).await; // the stranger is happy to dial B

    let b2 = Arc::clone(&b);
    let accept =
        tokio::spawn(
            async move { tokio::time::timeout(Duration::from_secs(2), b2.accept()).await },
        );
    let attempt = s.connect(&loopback_peer(&b)).await;

    // B must never hand the application a session from the stranger...
    assert!(
        accept.await.unwrap().is_err(),
        "B surfaced a session from an unpaired device"
    );
    // ...and the stranger must not be able to use whatever it got.
    if let Ok(session) = attempt {
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            session.establish(&hello(9), CONTROLLER, &[]).await
        })
        .await;
        assert!(
            matches!(result, Ok(Err(_))),
            "stranger completed a hello exchange: {result:?}"
        );
        let closed = tokio::time::timeout(Duration::from_secs(3), session.closed()).await;
        assert!(
            closed.is_ok(),
            "connection from an unpaired device was left open"
        );
    }
}

#[tokio::test]
async fn dialing_an_unpaired_device_is_refused_locally() {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[]).await; // A has paired with nobody
    let b = loopback_node(&ib, &[key(&ia)]).await;
    let err = a.connect(&loopback_peer(&b)).await.unwrap_err();
    assert!(matches!(err, NetError::NotPaired), "{err:?}");
}

#[tokio::test]
async fn connecting_to_the_wrong_key_at_a_known_address_fails() {
    // An attacker controls the network path and answers at B's address with *its own* key.
    // A dials the address expecting B's key and must refuse to talk to the impostor.
    let (ia, ib, mallory) = (identity(1), identity(2), identity(3));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let _impostor = loopback_node(&mallory, &[key(&ia)]).await;
    let mut target = loopback_peer(&_impostor);
    target.key = key(&ib); // expecting B, but the address belongs to Mallory
    let result = tokio::time::timeout(Duration::from_secs(8), a.connect(&target)).await;
    assert!(
        matches!(result, Ok(Err(_)) | Err(_)),
        "A connected to an impostor presenting the wrong key"
    );
}

#[tokio::test]
async fn revoking_a_peer_terminates_its_live_session_immediately() {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let b = loopback_node(&ib, &[key(&ia)]).await;
    let (a_session, b_session) = connect_pair(&a, &b, &loopback_peer(&b)).await;

    // B's owner revokes A. The session must die now, not at the next reconnect.
    b.allow_list().revoke(&key(&ia));
    let reason = tokio::time::timeout(Duration::from_secs(3), a_session.closed())
        .await
        .expect("A's session still open");
    assert!(
        reason.contains(&CLOSE_REVOKED.to_string()) || reason.to_lowercase().contains("revoked"),
        "unexpected close reason: {reason}"
    );
    assert!(
        b_session.is_closed()
            || tokio::time::timeout(Duration::from_secs(2), b_session.closed())
                .await
                .is_ok()
    );

    // And A cannot come back.
    let again = tokio::time::timeout(Duration::from_secs(4), a.connect(&loopback_peer(&b))).await;
    if let Ok(Ok(s)) = again {
        let r = tokio::time::timeout(Duration::from_secs(3), s.closed()).await;
        assert!(r.is_ok(), "A re-established a session after revocation");
    }
    let _ = CLOSE_NOT_PAIRED;
}

#[tokio::test]
async fn streams_are_routed_by_kind_and_datagrams_flow() {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let b = loopback_node(&ib, &[key(&ia)]).await;
    let (sa, sb) = connect_pair(&a, &b, &loopback_peer(&b)).await;

    let mut input = sa.open_stream(StreamKind::Input).await.unwrap();
    input.send.write_all(b"keydown").await.unwrap();
    let (kind, mut got) = sb.accept_stream().await.unwrap();
    assert_eq!(kind, StreamKind::Input);
    let mut buf = [0u8; 7];
    got.recv.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"keydown");

    let mut video = sa.open_uni(StreamKind::Video).await.unwrap();
    video.write_all(&[7; 1000]).await.unwrap();
    video.finish().unwrap();
    let (kind, mut rx) = sb.accept_uni().await.unwrap();
    assert_eq!(kind, StreamKind::Video);
    assert_eq!(rx.read_to_end(2000).await.unwrap(), vec![7; 1000]);

    sa.send_datagram(Bytes::from_static(b"cursor")).unwrap();
    assert_eq!(&sb.recv_datagram().await.unwrap()[..], b"cursor");
    assert!(sa.max_datagram_size().unwrap() >= 1000);
}

#[tokio::test]
async fn a_stream_with_an_unknown_kind_is_rejected() {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let b = loopback_node(&ib, &[key(&ia)]).await;
    let (sa, sb) = connect_pair(&a, &b, &loopback_peer(&b)).await;
    let (mut send, _recv) = sa.raw().open_bi().await.unwrap();
    send.write_all(&[99]).await.unwrap();
    assert!(matches!(
        sb.accept_stream().await,
        Err(NetError::Protocol(_))
    ));
}

#[tokio::test]
async fn silent_peers_are_timed_out_during_hello() {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let b = loopback_node(&ib, &[key(&ia)]).await;
    let (_sa, sb) = connect_pair(&a, &b, &loopback_peer(&b)).await;
    // A never sends a hello: B's establish must give up rather than wait forever.
    tokio::time::pause();
    let waiting = tokio::spawn(async move { sb.establish(&hello(2), TARGET, &[]).await });
    tokio::time::advance(Duration::from_secs(11)).await;
    assert!(matches!(waiting.await.unwrap(), Err(NetError::Timeout)));
}

#[tokio::test]
async fn diagnostics_report_sane_loopback_numbers() {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let b = loopback_node(&ib, &[key(&ia)]).await;
    let (sa, sb) = connect_pair(&a, &b, &loopback_peer(&b)).await;
    let (ha, hb) = (hello(1), hello(2));
    let (ea, eb) = tokio::join!(
        sa.establish(&ha, CONTROLLER, &[VideoCodec::H264]),
        sb.establish(&hb, TARGET, &[VideoCodec::H264]),
    );
    let (mut ea, eb) = (ea.unwrap(), eb.unwrap());
    tokio::spawn(diag::serve(sb.clone(), eb.control));

    let report = diag::run(
        &sa,
        &mut ea.control,
        &diag::Options {
            bulk_bytes: 64 * 1024 * 1024,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    println!("control  {}", report.control);
    println!("stream   {}", report.stream);
    println!(
        "datagram {} (loss {:.1}%)",
        report.datagram,
        report.datagram_loss * 100.0
    );
    println!(
        "bulk     {:.0} Mbit/s over {:?}",
        report.throughput_mbps, report.path_after.kind
    );

    assert_eq!(report.control.count, 200);
    assert_eq!(report.stream.count, 200);
    assert!(report.datagram_loss < 0.05, "loss {}", report.datagram_loss);
    // Generous bounds: this checks "the stack is not pathologically slow", not a benchmark.
    assert!(
        report.control.p95_us < 50_000,
        "control p95 {} µs",
        report.control.p95_us
    );
    assert!(
        report.stream.p95_us < 50_000,
        "stream p95 {} µs",
        report.stream.p95_us
    );
    assert!(
        report.throughput_mbps > 100.0,
        "bulk {} Mbit/s",
        report.throughput_mbps
    );
    assert_eq!(report.path_after.kind, PathKind::Lan);
}

#[tokio::test]
async fn peer_addr_without_hints_cannot_connect_without_discovery() {
    // With no relay and no address hints there is nothing to dial: fail fast, do not hang.
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        a.connect(&PeerAddr::from_key(key(&ib))),
    )
    .await;
    assert!(matches!(result, Ok(Err(_))), "{result:?}");
}
