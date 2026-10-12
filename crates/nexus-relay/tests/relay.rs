//! A real standalone child binary carrying authenticated peers through a TLS-only relay path.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use nexus_net::{AllowList, Node, NodeConfig, PeerAddr, RelayServer, RelaySetting, RelayTrust};
use std::sync::Arc;
use std::time::Duration;
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[tokio::test]
async fn standalone_binary_authenticates_and_relays_encrypted_traffic() {
    let dir = tempfile::tempdir().unwrap();
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let cert = dir.path().join("cert.pem");
    let private = dir.path().join("key.pem");
    std::fs::write(&cert, certified.cert.pem()).unwrap();
    std::fs::write(&private, certified.signing_key.serialize_pem()).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let token = "test-only-admission-token-at-least-32-bytes";
    let _child = Child(
        std::process::Command::new(env!("CARGO_BIN_EXE_nexus-relay"))
            .args([
                "--listen",
                "127.0.0.1:0",
                "--tls-listen",
                &addr.to_string(),
                "--cert",
            ])
            .arg(cert)
            .arg("--key")
            .arg(private)
            .env("NEXUS_RELAY_TOKEN", token)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let url = format!("https://{addr}");
    let mut config = NodeConfig {
        direct: false,
        port_mapping: false,
        relay: RelaySetting::Custom(vec![RelayServer {
            url: url.clone(),
            quic_port: None,
            auth_token: Some(token.into()),
        }]),
        relay_trust: RelayTrust::CustomRoots(vec![certified.cert.der().to_vec()]),
        ..Default::default()
    };
    let ia =
        nexus_crypto::DeviceIdentity::from_seed(&nexus_crypto::IdentitySeed::from_bytes([1; 32]));
    let ib =
        nexus_crypto::DeviceIdentity::from_seed(&nexus_crypto::IdentitySeed::from_bytes([2; 32]));
    let a = Arc::new(
        Node::start(
            &ia,
            &config,
            AllowList::from_keys([ib.public().ed25519_bytes()]),
        )
        .await
        .unwrap(),
    );
    let b = Arc::new(
        Node::start(
            &ib,
            &config,
            AllowList::from_keys([ia.public().ed25519_bytes()]),
        )
        .await
        .unwrap(),
    );
    assert!(a.wait_online(Duration::from_secs(10)).await);
    assert!(b.wait_online(Duration::from_secs(10)).await);
    let accept = tokio::spawn({
        let b = b.clone();
        async move { b.accept().await.unwrap() }
    });
    let sa = a
        .connect(&PeerAddr::from_key(b.id()).with_relay(url.clone()))
        .await
        .unwrap();
    let sb = tokio::time::timeout(Duration::from_secs(10), accept)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sa.path().kind, nexus_proto::v1::PathKind::Relay);
    let mut sent = sa
        .open_stream(nexus_net::StreamKind::Diagnostics)
        .await
        .unwrap();
    sent.send
        .write_all(b"encrypted remote screen/control bytes")
        .await
        .unwrap();
    sent.send.finish().unwrap();
    let (_, mut received) = sb.accept_stream().await.unwrap();
    let bytes = received.recv.read_to_end(4096).await.unwrap();
    assert_eq!(bytes, b"encrypted remote screen/control bytes");
    // The admission token is enforced before a node can register on the relay.
    if let RelaySetting::Custom(servers) = &mut config.relay {
        servers[0].auth_token = Some("wrong-token".into());
    }
    let stranger =
        nexus_crypto::DeviceIdentity::from_seed(&nexus_crypto::IdentitySeed::from_bytes([3; 32]));
    let denied = Node::start(&stranger, &config, AllowList::from_keys([]))
        .await
        .unwrap();
    assert!(!denied.wait_online(Duration::from_secs(2)).await);
    sa.close_normal();
    sb.close_normal();
    a.close().await;
    b.close().await;
    denied.close().await;
}
