//! Diagnostics: the behaviour behind `nexus ping` and the transport benchmark.
//!
//! The target runs [`serve`]; the controller runs [`run`] and gets latency percentiles for each
//! traffic class the product uses (control RPC, reliable input-sized messages, unreliable
//! datagrams) plus bulk throughput and the path in use.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::{BufMut, Bytes, BytesMut};
use nexus_core::clock::now_us;
use nexus_core::telemetry::{LatencyStats, LatencySummary};
use nexus_proto::v1::{ControlMessage, Ping, Pong, control_message::Body};

use crate::NetError;
use crate::session::{PathInfo, Session};
use crate::stream::{FramedStream, StreamKind};

const MODE_ECHO: u8 = 0;
const MODE_SINK: u8 = 1;
const SMALL_MESSAGE: usize = 64;
const DATAGRAM_SIZE: usize = 100;

/// What to measure.
#[derive(Debug, Clone)]
pub struct Options {
    /// Control-plane ping/pong round trips.
    pub pings: usize,
    /// Small reliable-stream echo round trips (input-event sized).
    pub stream_echoes: usize,
    /// Datagrams sent (cursor/audio sized).
    pub datagrams: usize,
    /// Bytes to upload for the throughput test (0 skips it).
    pub bulk_bytes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            pings: 200,
            stream_echoes: 200,
            datagrams: 200,
            bulk_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Measured results.
#[derive(Debug, Clone)]
pub struct Report {
    /// Control ping/pong RTT.
    pub control: LatencySummary,
    /// Small reliable echo RTT (what an input event experiences).
    pub stream: LatencySummary,
    /// Datagram echo RTT (what the cursor channel experiences).
    pub datagram: LatencySummary,
    /// Fraction of datagrams that never came back (0.0 – 1.0).
    pub datagram_loss: f64,
    /// Upload throughput in megabits per second (0 if skipped).
    pub throughput_mbps: f64,
    /// Path when the test started.
    pub path_before: PathInfo,
    /// Path when it finished (it may have migrated).
    pub path_after: PathInfo,
}

/// Answers diagnostics requests on `session` until it closes. Run on the target side.
pub async fn serve(session: Session, mut control: FramedStream) {
    // Datagram echo.
    let echo_session = session.clone();
    let datagrams = tokio::spawn(async move {
        while let Ok(data) = echo_session.recv_datagram().await {
            if echo_session.send_datagram(data).is_err() {
                break;
            }
        }
    });
    // Stream handlers.
    let stream_session = session.clone();
    let streams = tokio::spawn(async move {
        while let Ok((kind, stream)) = stream_session.accept_stream().await {
            if kind == StreamKind::Diagnostics {
                tokio::spawn(serve_stream(stream));
            }
        }
    });
    // Control pings.
    while let Ok(Some(msg)) = control.recv::<ControlMessage>().await {
        if let Some(Body::Ping(Ping { sent_us })) = msg.body {
            let now = now_us();
            let pong = ControlMessage {
                in_reply_to: msg.id,
                body: Some(Body::Pong(Pong {
                    echo_sent_us: sent_us,
                    recv_us: now,
                    reply_us: now,
                })),
                ..Default::default()
            };
            if control.send(&pong).await.is_err() {
                break;
            }
        }
    }
    datagrams.abort();
    streams.abort();
}

async fn serve_stream(stream: crate::stream::Stream) {
    let crate::stream::Stream { mut send, mut recv } = stream;
    let mut mode = [0u8; 1];
    if recv.read_exact(&mut mode).await.is_err() {
        return;
    }
    let mut buf = vec![0u8; 64 * 1024];
    match mode[0] {
        MODE_ECHO => {
            while let Ok(Some(n)) = recv.read(&mut buf).await {
                if send.write_all(&buf[..n]).await.is_err() {
                    return;
                }
            }
            let _ = send.finish();
        }
        MODE_SINK => {
            let mut total: u64 = 0;
            while let Ok(Some(n)) = recv.read(&mut buf).await {
                total += n as u64;
            }
            let _ = send.write_all(&total.to_le_bytes()).await;
            let _ = send.finish();
        }
        _ => {}
    }
}

/// Runs the measurements against a peer that is running [`serve`].
pub async fn run(
    session: &Session,
    control: &mut FramedStream,
    opts: &Options,
) -> Result<Report, NetError> {
    let path_before = session.path();

    // 1. Control-plane round trips.
    let mut control_stats = LatencyStats::new();
    for i in 0..opts.pings {
        let sent = now_us();
        let msg = ControlMessage {
            id: i as u64 + 1,
            body: Some(Body::Ping(Ping { sent_us: sent })),
            ..Default::default()
        };
        control.send(&msg).await?;
        match control.recv::<ControlMessage>().await? {
            Some(ControlMessage {
                body: Some(Body::Pong(p)),
                ..
            }) if p.echo_sent_us == sent => {
                control_stats.record_us(now_us().saturating_sub(sent));
            }
            _ => return Err(NetError::Protocol("unexpected reply to ping")),
        }
    }

    // 2. Small reliable messages (input-sized) on a dedicated stream.
    let mut stream_stats = LatencyStats::new();
    if opts.stream_echoes > 0 {
        let mut stream = session.open_stream(StreamKind::Diagnostics).await?;
        stream
            .send
            .write_all(&[MODE_ECHO])
            .await
            .map_err(NetError::stream)?;
        let mut reply = [0u8; SMALL_MESSAGE];
        for _ in 0..opts.stream_echoes {
            let sent = now_us();
            let mut msg = [0u8; SMALL_MESSAGE];
            msg[..8].copy_from_slice(&sent.to_le_bytes());
            stream
                .send
                .write_all(&msg)
                .await
                .map_err(NetError::stream)?;
            stream
                .recv
                .read_exact(&mut reply)
                .await
                .map_err(NetError::stream)?;
            stream_stats.record_us(now_us().saturating_sub(sent));
        }
        let _ = stream.send.finish();
    }

    // 3. Datagrams: send paced, collect echoes concurrently.
    let mut datagram_stats = LatencyStats::new();
    let mut received = 0usize;
    if opts.datagrams > 0 {
        let rx_session = session.clone();
        let expected = opts.datagrams;
        let collector = tokio::spawn(async move {
            let mut rtts: HashMap<u32, u64> = HashMap::new();
            // Stop when the echoes dry up (1 s without one), not at a fixed deadline: the time
            // to send N paced datagrams depends on N and on timer resolution.
            while rtts.len() < expected {
                match tokio::time::timeout(Duration::from_secs(1), rx_session.recv_datagram()).await
                {
                    Ok(Ok(data)) if data.len() >= 12 => {
                        let seq = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                        let mut ts = [0u8; 8];
                        ts.copy_from_slice(&data[4..12]);
                        rtts.insert(seq, now_us().saturating_sub(u64::from_le_bytes(ts)));
                    }
                    _ => break,
                }
            }
            rtts
        });
        for seq in 0..opts.datagrams {
            let mut d = BytesMut::with_capacity(DATAGRAM_SIZE);
            d.put_u32_le(u32::try_from(seq).unwrap_or(u32::MAX));
            d.put_u64_le(now_us());
            d.resize(DATAGRAM_SIZE, 0);
            session.send_datagram(Bytes::from(d))?;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let rtts = collector.await.unwrap_or_default();
        received = rtts.len();
        for rtt in rtts.values() {
            datagram_stats.record_us(*rtt);
        }
    }

    // 4. Bulk upload throughput.
    let mut throughput_mbps = 0.0;
    if opts.bulk_bytes > 0 {
        let mut stream = session.open_stream(StreamKind::Diagnostics).await?;
        stream
            .send
            .write_all(&[MODE_SINK])
            .await
            .map_err(NetError::stream)?;
        let chunk = vec![0xA5u8; 64 * 1024];
        let started = Instant::now();
        let mut left = opts.bulk_bytes;
        while left > 0 {
            let n = left.min(chunk.len());
            stream
                .send
                .write_all(&chunk[..n])
                .await
                .map_err(NetError::stream)?;
            left -= n;
        }
        stream.send.finish().map_err(NetError::stream)?;
        let mut count = [0u8; 8];
        stream
            .recv
            .read_exact(&mut count)
            .await
            .map_err(NetError::stream)?;
        let elapsed = started.elapsed().as_secs_f64().max(1e-9);
        if u64::from_le_bytes(count) != opts.bulk_bytes as u64 {
            return Err(NetError::Protocol("peer received a different byte count"));
        }
        #[allow(clippy::cast_precision_loss)]
        {
            throughput_mbps = opts.bulk_bytes as f64 * 8.0 / elapsed / 1_000_000.0;
        }
    }

    #[allow(clippy::cast_precision_loss)]
    let datagram_loss = if opts.datagrams == 0 {
        0.0
    } else {
        1.0 - received as f64 / opts.datagrams as f64
    };
    Ok(Report {
        control: control_stats.summary(),
        stream: stream_stats.summary(),
        datagram: datagram_stats.summary(),
        datagram_loss,
        throughput_mbps,
        path_before,
        path_after: session.path(),
    })
}
