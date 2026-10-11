//! Relay fallback and path migration, against a real local `iroh-relay` server.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

mod common;

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use iroh_relay::server::{
    AllowAll, CertConfig, QuicConfig, RelayConfig as RelayServerConfig, Server, ServerConfig,
    TlsConfig,
};
use nexus_net::{
    AllowList, Node, NodeConfig, PeerAddr, RelayServer, RelaySetting, RelayTrust, diag,
};
use nexus_proto::v1::{PathKind, VideoCodec};

/// A relay with a freshly generated self-signed certificate that nodes pin as their only
/// trusted root (so the relay's TLS is genuinely verified, not skipped).
struct TestRelay {
    url: String,
    quic_port: u16,
    cert_der: Vec<u8>,
    _server: Server,
}

async fn start_relay() -> TestRelay {
    let (certs, server_config) = iroh_relay::server::testing::self_signed_tls_certs_and_config();
    let mut relay = RelayServerConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new(
        (Ipv4Addr::LOCALHOST, 0),
        CertConfig::Manual { server_config },
    ));
    relay.key_cache_capacity = Some(1024);
    relay.access = Arc::new(AllowAll);
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    config.quic = Some(QuicConfig::new((Ipv4Addr::LOCALHOST, 0)));
    let server = Server::spawn(config).await.unwrap();
    TestRelay {
        url: format!("https://{}", server.https_addr().unwrap()),
        quic_port: server.quic_addr().unwrap().port(),
        cert_der: certs[0].to_vec(),
        _server: server,
    }
}

async fn relay_node(
    id: &nexus_crypto::DeviceIdentity,
    allowed: &[[u8; 32]],
    relay: &TestRelay,
    direct: bool,
) -> Arc<Node> {
    let config = NodeConfig {
        direct,
        port_mapping: false,
        relay: RelaySetting::Custom(vec![RelayServer {
            url: relay.url.clone(),
            quic_port: Some(relay.quic_port),
            auth_token: None,
        }]),
        relay_trust: RelayTrust::CustomRoots(vec![relay.cert_der.clone()]),
        ..NodeConfig::default()
    };
    let node = Node::start(id, &config, AllowList::from_keys(allowed.iter().copied()))
        .await
        .unwrap();
    assert!(
        node.wait_online(Duration::from_secs(10)).await,
        "node never reached the relay"
    );
    Arc::new(node)
}

#[tokio::test]
async fn relay_only_peers_connect_and_the_path_is_reported_as_relay() {
    let relay = start_relay().await;
    let (ia, ib) = (identity(1), identity(2));
    // `direct = false`: no UDP paths exist at all, so the relay is the only way.
    let a = relay_node(&ia, &[key(&ib)], &relay, false).await;
    let b = relay_node(&ib, &[key(&ia)], &relay, false).await;
    let target = PeerAddr::from_key(b.id()).with_relay(relay.url.clone());
    let (sa, sb) = connect_pair(&a, &b, &target).await;

    assert_eq!(sa.peer(), key(&ib));
    assert_eq!(sa.path().kind, PathKind::Relay, "{:?}", sa.path());
    assert_eq!(
        sa.path().remote.trim_end_matches('/'),
        relay.url.trim_end_matches('/')
    );

    // The whole session works over the relay: hello, control RPC, streams, datagrams, bulk.
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
            pings: 50,
            stream_echoes: 50,
            datagrams: 50,
            bulk_bytes: 8 * 1024 * 1024,
        },
    )
    .await
    .unwrap();
    println!("relay control  {}", report.control);
    println!(
        "relay datagram {} (loss {:.1}%)",
        report.datagram,
        report.datagram_loss * 100.0
    );
    println!("relay bulk     {:.0} Mbit/s", report.throughput_mbps);
    assert_eq!(report.control.count, 50);
    assert!(report.datagram_loss < 0.2);
    assert!(report.throughput_mbps > 5.0);
    assert_eq!(report.path_after.kind, PathKind::Relay);
}

#[tokio::test]
async fn relay_cannot_be_used_to_reach_an_unpaired_device() {
    let relay = start_relay().await;
    let (ia, ib, stranger) = (identity(1), identity(2), identity(3));
    let b = relay_node(&ib, &[key(&ia)], &relay, false).await;
    let s = relay_node(&stranger, &[key(&ib)], &relay, false).await;
    let b2 = Arc::clone(&b);
    let accepted =
        tokio::spawn(
            async move { tokio::time::timeout(Duration::from_secs(3), b2.accept()).await },
        );
    let attempt = s
        .connect(&PeerAddr::from_key(b.id()).with_relay(relay.url.clone()))
        .await;
    assert!(
        accepted.await.unwrap().is_err(),
        "B surfaced a relayed session from a stranger"
    );
    if let Ok(session) = attempt {
        assert!(
            tokio::time::timeout(Duration::from_secs(3), session.closed())
                .await
                .is_ok()
        );
    }
}

#[tokio::test]
async fn a_session_that_starts_on_the_relay_migrates_to_a_direct_path() {
    let relay = start_relay().await;
    let (ia, ib) = (identity(1), identity(2));
    let a = relay_node(&ia, &[key(&ib)], &relay, true).await;
    let b = relay_node(&ib, &[key(&ia)], &relay, true).await;
    // Only the relay hint is given: any direct path must be discovered and hole-punched.
    let target = PeerAddr::from_key(b.id()).with_relay(relay.url.clone());
    let (sa, _sb) = connect_pair(&a, &b, &target).await;

    let upgraded = sa.wait_for_direct_path(Duration::from_secs(15)).await;
    println!("path after probing: {:?}", sa.path());
    assert!(
        upgraded,
        "session never moved off the relay; path = {:?}",
        sa.path()
    );
    assert!(matches!(sa.path().kind, PathKind::Lan | PathKind::Direct));
}
