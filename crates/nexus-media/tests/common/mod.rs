//! Shared helpers for the transport tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;

use nexus_crypto::{DeviceIdentity, IdentitySeed};
use nexus_net::{AllowList, Node, NodeConfig, PeerAddr, Session, SessionRole};
use nexus_proto::features;
use nexus_proto::v1::{Capabilities, Hello, VideoCodec};
use nexus_proto::version::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};

pub fn identity(byte: u8) -> Arc<DeviceIdentity> {
    Arc::new(DeviceIdentity::from_seed(&IdentitySeed::from_bytes(
        [byte; 32],
    )))
}

pub fn key(id: &DeviceIdentity) -> [u8; 32] {
    id.public().ed25519_bytes()
}

/// Loopback-only, direct-only node allowing exactly `allowed`.
pub async fn loopback_node(id: &DeviceIdentity, allowed: &[[u8; 32]]) -> Arc<Node> {
    let config = NodeConfig {
        bind: Some("127.0.0.1:0".parse().unwrap()),
        port_mapping: false,
        ..NodeConfig::default()
    };
    Arc::new(
        Node::start(id, &config, AllowList::from_keys(allowed.iter().copied()))
            .await
            .unwrap(),
    )
}

/// How to reach `node` on loopback.
pub fn loopback_peer(node: &Node) -> PeerAddr {
    let addr: SocketAddr = *node
        .local_addrs()
        .iter()
        .find(|a| a.is_ipv4())
        .expect("an IPv4 address");
    PeerAddr::from_key(node.id()).with_direct(addr)
}

pub fn hello(role_nonce: u8) -> Hello {
    Hello {
        version: Some(PROTOCOL_VERSION),
        min_version: Some(MIN_PROTOCOL_VERSION),
        capabilities: Some(Capabilities {
            video_encode: vec![VideoCodec::H264 as i32],
            video_decode: vec![VideoCodec::H264 as i32],
            features: vec![
                features::CLIPBOARD_TEXT.into(),
                features::TERMINAL_PTY.into(),
            ],
            ..Default::default()
        }),
        session_nonce: vec![role_nonce; 16].into(),
        agent_version: "test".into(),
        ..Default::default()
    }
}

/// Connects `a` to `b` (which accepts) and returns both session handles.
pub async fn connect_pair(a: &Arc<Node>, b: &Arc<Node>, peer: &PeerAddr) -> (Session, Session) {
    let b2 = Arc::clone(b);
    let accept = tokio::spawn(async move { b2.accept().await.expect("incoming session") });
    let dialed = a.connect(peer).await.unwrap();
    let accepted = accept.await.unwrap();
    (dialed, accepted)
}

pub const CONTROLLER: SessionRole = SessionRole::Controller;
pub const TARGET: SessionRole = SessionRole::Target;
