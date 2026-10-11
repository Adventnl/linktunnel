//! iroh, through the real `nexus-net` stack (identity pinning, allow-list and all).

use std::sync::Arc;

use nexus_crypto::{DeviceIdentity, IdentitySeed};
use nexus_net::{AllowList, Node, NodeConfig, PeerAddr, SessionRole, diag};
use nexus_proto::v1::{Hello, VideoCodec};
use nexus_proto::version::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};

use crate::Outcome;

fn identity(b: u8) -> DeviceIdentity {
    DeviceIdentity::from_seed(&IdentitySeed::from_bytes([b; 32]))
}

pub async fn run(pings: usize, bulk_mb: usize) -> anyhow::Result<Outcome> {
    let (ia, ib) = (identity(1), identity(2));
    let cfg = NodeConfig { bind: Some("127.0.0.1:0".parse()?), port_mapping: false, ..NodeConfig::default() };
    let a = Node::start(&ia, &cfg, AllowList::from_keys([ib.public().ed25519_bytes()])).await?;
    let b = Arc::new(Node::start(&ib, &cfg, AllowList::from_keys([ia.public().ed25519_bytes()])).await?);
    let target = PeerAddr::from_key(b.id()).with_direct(*b.local_addrs().iter().find(|x| x.is_ipv4()).ok_or_else(|| anyhow::anyhow!("no ipv4"))?);

    let b2 = Arc::clone(&b);
    let accept = tokio::spawn(async move { b2.accept().await });
    let sa = a.connect(&target).await?;
    let sb = accept.await?.ok_or_else(|| anyhow::anyhow!("no session"))?;
    let hello = |n: u8| Hello {
        version: Some(PROTOCOL_VERSION),
        min_version: Some(MIN_PROTOCOL_VERSION),
        session_nonce: vec![n; 16].into(),
        ..Default::default()
    };
    let (ha, hb) = (hello(1), hello(2));
    let (ea, eb) = tokio::join!(
        sa.establish(&ha, SessionRole::Controller, &[VideoCodec::H264]),
        sb.establish(&hb, SessionRole::Target, &[VideoCodec::H264]),
    );
    let (mut ea, eb) = (ea?, eb?);
    tokio::spawn(diag::serve(sb.clone(), eb.control));
    let report = diag::run(
        &sa,
        &mut ea.control,
        &diag::Options { pings: 0, stream_echoes: pings, datagrams: pings, bulk_bytes: bulk_mb * 1024 * 1024 },
    )
    .await?;
    Ok(Outcome {
        name: "iroh 1.3 (via nexus-net)",
        reliable: report.stream,
        unreliable: report.datagram,
        loss: report.datagram_loss,
        throughput_mbps: report.throughput_mbps,
    })
}
