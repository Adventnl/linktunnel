//! Two real nodes on loopback, a target serving a screen, a controller decoding it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use nexus_capture::CaptureBackend;
use nexus_capture::synthetic::{PROBE_OFF, PROBE_ON, Probe, SyntheticBackend, SyntheticConfig};
use nexus_crypto::{DeviceIdentity, IdentitySeed};
use nexus_media::controller::{ControllerOptions, ControllerSession, MediaEvent, Streams};
use nexus_media::target::{self, EgressShaper, TargetConfig, TargetStats};
use nexus_media::viewer::{Picture, VideoDecoder};
use nexus_net::{AllowList, Node, NodeConfig, PeerAddr, SessionRole};
use nexus_proto::features;
use nexus_proto::v1::{Capabilities, Hello, VideoCodec};
use nexus_proto::version::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};
use nexus_trust::grants;

use crate::{Latency, Shaped, ShapedRun, percentile};

fn identity(byte: u8) -> Arc<DeviceIdentity> {
    Arc::new(DeviceIdentity::from_seed(&IdentitySeed::from_bytes(
        [byte; 32],
    )))
}

fn hello(nonce: u8) -> Hello {
    Hello {
        version: Some(PROTOCOL_VERSION),
        min_version: Some(MIN_PROTOCOL_VERSION),
        capabilities: Some(Capabilities {
            video_encode: vec![VideoCodec::H264 as i32],
            video_decode: vec![VideoCodec::H264 as i32],
            features: vec![features::CURSOR_CHANNEL.into()],
            ..Default::default()
        }),
        session_nonce: vec![nonce; 16].into(),
        agent_version: "bench".into(),
        ..Default::default()
    }
}

async fn node(id: &DeviceIdentity, allowed: [u8; 32]) -> Result<Arc<Node>> {
    let cfg = NodeConfig {
        bind: Some("127.0.0.1:0".parse()?),
        port_mapping: false,
        ..NodeConfig::default()
    };
    Ok(Arc::new(
        Node::start(id, &cfg, AllowList::from_keys([allowed])).await?,
    ))
}

pub struct Rig {
    pub controller: ControllerSession,
    pub streams: Streams,
    pub stats: Arc<TargetStats>,
    _nodes: (Arc<Node>, Arc<Node>),
}

pub async fn rig(
    capture: Arc<dyn CaptureBackend>,
    injector: Option<target::InjectorFactory>,
    grant: &[&str],
    egress: Option<EgressShaper>,
    options: ControllerOptions,
) -> Result<Rig> {
    let (ia, ib) = (identity(1), identity(2));
    let a = node(&ia, ib.public().ed25519_bytes()).await?;
    let b = node(&ib, ia.public().ed25519_bytes()).await?;
    let addr = *b
        .local_addrs()
        .iter()
        .find(|x| x.is_ipv4())
        .ok_or_else(|| anyhow!("no IPv4 address"))?;
    let peer = PeerAddr::from_key(b.id()).with_direct(addr);
    let b2 = Arc::clone(&b);
    let accept = tokio::spawn(async move { b2.accept().await });
    let a_session = a.connect(&peer).await?;
    let b_session = accept
        .await?
        .ok_or_else(|| anyhow!("target never saw the session"))?;
    let (ha, hb) = (hello(1), hello(2));
    let (ea, eb) = tokio::join!(
        a_session.establish(&ha, SessionRole::Controller, &[VideoCodec::H264]),
        b_session.establish(&hb, SessionRole::Target, &[VideoCodec::H264])
    );
    let (ea, eb) = (ea?, eb?);
    let stats = Arc::new(TargetStats::default());
    let config = TargetConfig {
        grants: grant
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>(),
        capture,
        injector,
        max_fps: 60,
        max_inflight: 12,
        egress,
        files: None,
        clipboard: None,
        consent: None,
    };
    tokio::spawn(target::serve(
        b_session,
        eb.control,
        config,
        Arc::clone(&stats),
    ));
    let (controller, streams) = ControllerSession::start(a_session, ea.control, options).await?;
    Ok(Rig {
        controller,
        streams,
        stats,
        _nodes: (a, b),
    })
}

/// Waits for a decoded picture satisfying `pred`.
async fn next_picture(
    rig: &mut Rig,
    dec: &mut VideoDecoder,
    limit: Duration,
    pred: impl Fn(&Picture) -> bool,
) -> Result<Picture> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(200), rig.streams.events.recv()).await
        else {
            continue;
        };
        match ev {
            MediaEvent::Frame(f) => match dec.decode(&f) {
                Ok(Some(p)) if pred(&p) => return Ok(p),
                Ok(_) => {}
                Err(_) => rig.controller.request_keyframe(),
            },
            MediaEvent::Closed(r) => return Err(anyhow!("session closed: {r}")),
            _ => {}
        }
    }
    Err(anyhow!("timed out waiting for a picture"))
}

fn probe_colour(p: &Picture) -> [u8; 3] {
    let i = (32 * p.width as usize + 32) * 4;
    [p.bgra[i], p.bgra[i + 1], p.bgra[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) < 48)
}

fn summarise(
    mut total: Vec<f64>,
    mut enc: Vec<f64>,
    mut net: Vec<f64>,
    mut dec: Vec<f64>,
) -> Latency {
    for v in [&mut total, &mut enc, &mut net, &mut dec] {
        v.sort_by(f64::total_cmp);
    }
    Latency {
        n: total.len(),
        p50_ms: percentile(&total, 0.5),
        p95_ms: percentile(&total, 0.95),
        p99_ms: percentile(&total, 0.99),
        max_ms: total.last().copied().unwrap_or(0.0),
        stage_encode_p50_ms: percentile(&enc, 0.5),
        stage_network_p50_ms: percentile(&net, 0.5),
        stage_decode_p50_ms: percentile(&dec, 0.5),
    }
}

/// Flips the synthetic probe and times how long until a decoded picture shows it.
pub async fn glass(iterations: usize) -> Result<Latency> {
    let (backend, probe) = SyntheticBackend::new(SyntheticConfig {
        width: 1280,
        height: 720,
        ..Default::default()
    });
    let mut rig = rig(
        Arc::new(backend),
        None,
        &[grants::SCREEN],
        None,
        ControllerOptions {
            bitrate_bps: 10_000_000,
            ..Default::default()
        },
    )
    .await?;
    let mut dec = VideoDecoder::new();
    next_picture(&mut rig, &mut dec, Duration::from_secs(10), |_| true).await?;
    let result = flash_loop(&mut rig, &mut dec, &probe, iterations).await;
    rig.controller.close().await;
    result
}

async fn flash_loop(
    rig: &mut Rig,
    dec: &mut VideoDecoder,
    probe: &Probe,
    iterations: usize,
) -> Result<Latency> {
    let (mut total, mut enc, mut net, mut decode) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for round in 0..iterations + 10 {
        let on = round % 2 == 0;
        let want = if on { PROBE_ON } else { PROBE_OFF };
        let t0 = Instant::now();
        probe.set_flash(on);
        let pic = next_picture(rig, dec, Duration::from_secs(5), |p| {
            near(probe_colour(p), want)
        })
        .await?;
        let elapsed = t0.elapsed().as_secs_f64() * 1000.0;
        if round >= 10 {
            total.push(elapsed);
            #[allow(clippy::cast_precision_loss)]
            {
                enc.push(pic.timing.send_us.saturating_sub(pic.timing.capture_us) as f64 / 1000.0);
                net.push(pic.timing.recv_us.saturating_sub(pic.timing.send_us) as f64 / 1000.0);
                decode.push(pic.timing.decode_us as f64 / 1000.0);
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(summarise(total, enc, net, decode))
}

/// The same measurement through a real Xvfb desktop, driven by drawing a rectangle.
#[cfg(target_os = "linux")]
pub async fn x11(iterations: usize) -> Result<Option<Latency>> {
    use nexus_testkit::{Painter, Xvfb};
    let Some(xvfb) = Xvfb::start(1280, 720) else {
        eprintln!("   Xvfb not available; skipping");
        return Ok(None);
    };
    let painter = Painter::connect(xvfb.display());
    painter.fill(0, 0, 1280, 720, 0xffffff);
    painter.fill(0, 0, 64, 64, 0x000000);
    let display = xvfb.display().to_owned();
    let capture = Arc::new(nexus_capture::x11::X11Backend::connect(Some(&display))?);
    let mut rig = rig(
        capture,
        None,
        &[grants::SCREEN],
        None,
        ControllerOptions {
            bitrate_bps: 10_000_000,
            ..Default::default()
        },
    )
    .await?;
    let mut dec = VideoDecoder::new();
    next_picture(&mut rig, &mut dec, Duration::from_secs(10), |_| true).await?;
    let (mut total, mut enc, mut net, mut decode) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for round in 0..iterations + 10 {
        let on = round % 2 == 0;
        let want = if on { PROBE_ON } else { PROBE_OFF };
        let t0 = Instant::now();
        painter.fill(0, 0, 64, 64, if on { 0xffffff } else { 0x000000 });
        let pic = next_picture(&mut rig, &mut dec, Duration::from_secs(5), |p| {
            near(probe_colour(p), want)
        })
        .await?;
        if round >= 10 {
            total.push(t0.elapsed().as_secs_f64() * 1000.0);
            #[allow(clippy::cast_precision_loss)]
            {
                enc.push(pic.timing.send_us.saturating_sub(pic.timing.capture_us) as f64 / 1000.0);
                net.push(pic.timing.recv_us.saturating_sub(pic.timing.send_us) as f64 / 1000.0);
                decode.push(pic.timing.decode_us as f64 / 1000.0);
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    rig.controller.close().await;
    drop(xvfb);
    Ok(Some(summarise(total, enc, net, decode)))
}

#[cfg(not(target_os = "linux"))]
pub async fn x11(_iterations: usize) -> Result<Option<Latency>> {
    Ok(None)
}

async fn shaped_run(adaptive: bool, rate_mbps: f64, delay_ms: u64) -> Result<ShapedRun> {
    let (backend, _probe) = SyntheticBackend::new(SyntheticConfig {
        width: 1280,
        height: 720,
        fps: 60.0,
        animate: true,
        noise_percent: 5,
        ..Default::default()
    });
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let egress = EgressShaper {
        rate_bps: (rate_mbps * 1e6) as u64,
        delay: Duration::from_millis(delay_ms),
    };
    let mut rig = rig(
        Arc::new(backend),
        None,
        &[grants::SCREEN],
        Some(egress),
        ControllerOptions {
            adaptive,
            bitrate_bps: 6_000_000,
            ..Default::default()
        },
    )
    .await?;
    let start = Instant::now();
    let mut window: Option<(Instant, u64)> = None;
    while start.elapsed() < Duration::from_secs(16) {
        let _ = tokio::time::timeout(Duration::from_millis(20), rig.streams.events.recv()).await;
        if window.is_none() && start.elapsed() >= Duration::from_secs(11) {
            let _ = rig.controller.stats().capture_to_receive.take_summary();
            window = Some((
                Instant::now(),
                rig.controller
                    .stats()
                    .bytes_received
                    .load(Ordering::Relaxed),
            ));
        }
    }
    let (t0, b0) = window.ok_or_else(|| anyhow!("no measurement window"))?;
    let bytes = rig
        .controller
        .stats()
        .bytes_received
        .load(Ordering::Relaxed)
        - b0;
    let s = rig.controller.stats().capture_to_receive.take_summary();
    rig.controller.close().await;
    #[allow(clippy::cast_precision_loss)]
    Ok(ShapedRun {
        goodput_mbps: bytes as f64 * 8.0 / t0.elapsed().as_secs_f64() / 1e6,
        p50_ms: s.p50_us as f64 / 1000.0,
        p95_ms: s.p95_us as f64 / 1000.0,
    })
}

pub async fn shaped() -> Result<Shaped> {
    let fixed = shaped_run(false, 4.0, 25).await?;
    let adaptive = shaped_run(true, 4.0, 25).await?;
    Ok(Shaped {
        link: "4 Mbit/s uplink, 25 ms one-way delay, source wants 6 Mbit/s".into(),
        fixed,
        adaptive,
    })
}
