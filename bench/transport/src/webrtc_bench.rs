//! webrtc-rs 0.21 data channels over loopback (SCTP over DTLS over ICE-UDP).
//!
//! Unreliable traffic uses an unordered, zero-retransmit channel, the WebRTC equivalent of
//! QUIC datagrams.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use nexus_core::clock::now_us;
use nexus_core::telemetry::LatencyStats;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceGatheringState, Registry, register_default_interceptors,
};
use webrtc::runtime::{Runtime, Sender, channel, default_runtime};

use crate::Outcome;

const CHUNK: usize = 16 * 1024;

#[derive(Clone)]
struct Gather(Sender<()>);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Gather {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.0.try_send(());
        }
    }
}

/// Responder: echoes `echo*` channels; counts `bulk` channels and reports completion.
#[derive(Clone)]
struct Responder {
    runtime: Arc<dyn Runtime>,
    gather: Sender<()>,
    bulk_target: usize,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Responder {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gather.try_send(());
        }
    }

    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        let target = self.bulk_target;
        self.runtime.spawn(Box::pin(async move {
            let label = dc.label().await.unwrap_or_default();
            let bulk = label == "bulk";
            let received = AtomicUsize::new(0);
            while let Some(event) = dc.poll().await {
                match event {
                    DataChannelEvent::OnMessage(msg) => {
                        if bulk {
                            if received.fetch_add(msg.data.len(), Ordering::Relaxed) + msg.data.len() >= target {
                                let _ = dc.send(BytesMut::from(&b"done"[..])).await;
                                break;
                            }
                        } else {
                            let _ = dc.send(BytesMut::from(msg.data.as_ref())).await;
                        }
                    }
                    DataChannelEvent::OnClose | DataChannelEvent::OnError => break,
                    _ => {}
                }
            }
        }));
    }
}

async fn build(runtime: Arc<dyn Runtime>, handler: Arc<dyn PeerConnectionEventHandler>) -> anyhow::Result<Arc<dyn PeerConnection>> {
    let mut media = MediaEngine::default();
    media.register_default_codecs()?;
    let registry = register_default_interceptors(Registry::new(), &mut media)?;
    let pc = PeerConnectionBuilder::new()
        .with_configuration(RTCConfigurationBuilder::new().build())
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_handler(handler)
        .with_runtime(runtime)
        .with_udp_addrs(vec!["127.0.0.1:0".to_string()])
        .build()
        .await?;
    Ok(Arc::new(pc) as Arc<dyn PeerConnection>)
}

async fn wait_open(dc: &Arc<dyn DataChannel>) -> anyhow::Result<()> {
    loop {
        match dc.poll().await {
            Some(DataChannelEvent::OnOpen) => return Ok(()),
            Some(DataChannelEvent::OnClose) | None => anyhow::bail!("channel closed before opening"),
            _ => {}
        }
    }
}

/// Sends one probe and waits for its echo; returns the RTT in µs or `None` on timeout.
async fn probe(dc: &Arc<dyn DataChannel>, wait: Duration) -> anyhow::Result<Option<u64>> {
    let t = now_us();
    let mut msg = BytesMut::zeroed(64);
    msg[..8].copy_from_slice(&t.to_le_bytes());
    dc.send(msg).await?;
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, dc.poll()).await {
            Ok(Some(DataChannelEvent::OnMessage(m))) if m.data.len() >= 8 => {
                let mut ts = [0u8; 8];
                ts.copy_from_slice(&m.data[..8]);
                return Ok(Some(now_us().saturating_sub(u64::from_le_bytes(ts))));
            }
            Ok(Some(_)) => {}
            Ok(None) => anyhow::bail!("channel closed"),
            Err(_) => return Ok(None),
        }
    }
}

pub async fn run(pings: usize, bulk_mb: usize) -> anyhow::Result<Outcome> {
    let runtime = default_runtime().ok_or_else(|| anyhow::anyhow!("no webrtc runtime"))?;
    let total = bulk_mb * 1024 * 1024;

    let (req_gather_tx, mut req_gather_rx) = channel::<()>(1);
    let requester = build(runtime.clone(), Arc::new(Gather(req_gather_tx))).await?;
    let reliable_dc = requester.create_data_channel("echo-reliable", None).await?;
    let unreliable_dc = requester
        .create_data_channel(
            "echo-unreliable",
            Some(RTCDataChannelInit { ordered: false, max_retransmits: Some(0), ..Default::default() }),
        )
        .await?;
    let bulk_dc = requester.create_data_channel("bulk", None).await?;

    let offer = requester.create_offer(None).await?;
    requester.set_local_description(offer).await?;
    req_gather_rx.recv().await;
    let offer_sdp = requester.local_description().await.ok_or_else(|| anyhow::anyhow!("no offer"))?;

    let (resp_gather_tx, mut resp_gather_rx) = channel::<()>(1);
    let responder = build(
        runtime.clone(),
        Arc::new(Responder { runtime: runtime.clone(), gather: resp_gather_tx, bulk_target: total }),
    )
    .await?;
    responder.set_remote_description(offer_sdp).await?;
    let answer = responder.create_answer(None).await?;
    responder.set_local_description(answer).await?;
    resp_gather_rx.recv().await;
    let answer_sdp = responder.local_description().await.ok_or_else(|| anyhow::anyhow!("no answer"))?;
    requester.set_remote_description(answer_sdp).await?;

    for dc in [&reliable_dc, &unreliable_dc, &bulk_dc] {
        tokio::time::timeout(Duration::from_secs(20), wait_open(dc)).await??;
    }

    let mut reliable = LatencyStats::new();
    for _ in 0..pings {
        if let Some(us) = probe(&reliable_dc, Duration::from_secs(5)).await? {
            reliable.record_us(us);
        }
    }
    let mut unreliable = LatencyStats::new();
    let mut lost = 0usize;
    for _ in 0..pings {
        match probe(&unreliable_dc, Duration::from_millis(500)).await? {
            Some(us) => unreliable.record_us(us),
            None => lost += 1,
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    let started = Instant::now();
    let chunk = BytesMut::zeroed(CHUNK);
    let mut sent = 0usize;
    while sent < total {
        bulk_dc.send(chunk.clone()).await?;
        sent += CHUNK;
    }
    // Wait for the responder's "done".
    let finish = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match bulk_dc.poll().await {
                Some(DataChannelEvent::OnMessage(m)) if m.data.as_ref() == b"done" => return Ok(()),
                Some(DataChannelEvent::OnClose) | None => return Err(anyhow::anyhow!("closed")),
                _ => {}
            }
        }
    })
    .await;
    let mbps = match finish {
        Ok(Ok(())) => total as f64 * 8.0 / started.elapsed().as_secs_f64() / 1e6,
        _ => 0.0,
    };

    Ok(Outcome {
        name: "webrtc-rs 0.21 (data channels)",
        reliable: reliable.summary(),
        unreliable: unreliable.summary(),
        loss: lost as f64 / pings as f64,
        throughput_mbps: mbps,
    })
}
