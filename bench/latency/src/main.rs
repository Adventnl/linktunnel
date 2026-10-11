//! `nexus-bench`: measures the remote-desktop pipeline and writes machine-readable results.
//!
//! ```text
//! cargo run --release -p nexus-bench -- all --out docs/benchmarks/latest.json
//! ```
//!
//! Scenarios:
//! * `glass`: flashing-probe glass-to-glass latency over a real QUIC session on loopback
//!   (synthetic screen → capture → H.264 → QUIC → decode), with the per-stage breakdown;
//! * `x11`: the same through a real Xvfb desktop (draw → XDamage → … → decoded);
//! * `shaped`: adaptive vs. fixed bitrate behind a bandwidth-limited, delayed uplink;
//! * `codec`: raw encode / decode / colour-convert cost per frame at several resolutions.
//!
//! `scripts/bench-gate.sh` compares the output with a committed baseline and fails if the p95 of
//! any gated metric regressed by more than 10 %.

#![allow(clippy::print_stdout, clippy::print_stderr, missing_docs, dead_code)]

mod rig;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use clap::{Parser, Subcommand};
use nexus_codec::{Decoder as _, Encoder as _};
use nexus_codec::{EncoderConfig, Frame, FrameFormat, H264Decoder, H264Encoder};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(name = "nexus-bench", about = "Remote-desktop pipeline benchmarks")]
struct Cli {
    #[command(subcommand)]
    scenario: Scenario,
    /// Write the JSON report here (also printed to stdout).
    #[arg(long, global = true)]
    out: Option<PathBuf>,
    /// Measurements per latency scenario.
    #[arg(long, global = true, default_value_t = 150)]
    iterations: usize,
}

#[derive(Debug, Subcommand)]
enum Scenario {
    /// Everything below.
    All,
    /// Glass-to-glass flash latency over loopback QUIC with a synthetic screen.
    Glass,
    /// The same through a real X11 (Xvfb) desktop. Skipped when Xvfb is missing.
    X11,
    /// Adaptive vs. fixed bitrate behind a slow, delayed uplink.
    Shaped,
    /// Encode / decode / convert cost per frame.
    Codec,
}

#[derive(Debug, Default, Serialize)]
struct Report {
    commit: String,
    host: Host,
    glass: Option<Latency>,
    x11: Option<Latency>,
    shaped: Option<Shaped>,
    codec: Vec<CodecRow>,
}

#[derive(Debug, Default, Serialize)]
struct Host {
    os: String,
    arch: String,
    cpus: usize,
    profile: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Latency {
    pub n: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// Where the median time went: target-side (capture → handed to the network).
    pub stage_encode_p50_ms: f64,
    /// … network (send → fully received).
    pub stage_network_p50_ms: f64,
    /// … decode and colour conversion on the viewer.
    pub stage_decode_p50_ms: f64,
}

#[derive(Debug, Serialize)]
struct Shaped {
    link: String,
    fixed: ShapedRun,
    adaptive: ShapedRun,
}

#[derive(Debug, Serialize)]
pub struct ShapedRun {
    pub goodput_mbps: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
}

#[derive(Debug, Serialize)]
struct CodecRow {
    resolution: String,
    encode_p50_ms: f64,
    encode_p95_ms: f64,
    decode_convert_p50_ms: f64,
    keyframe_kib: f64,
    delta_kib_typing: f64,
    max_fps_encode: f64,
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut report = Report {
        commit: git_commit(),
        host: Host {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            cpus: std::thread::available_parallelism().map_or(1, usize::from),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
        },
        ..Report::default()
    };
    let all = matches!(cli.scenario, Scenario::All);
    if all || matches!(cli.scenario, Scenario::Glass) {
        eprintln!("== glass-to-glass over loopback QUIC (synthetic screen)");
        report.glass = Some(rig::glass(cli.iterations).await.context("glass scenario")?);
    }
    if all || matches!(cli.scenario, Scenario::X11) {
        eprintln!("== X11 desktop (Xvfb) draw → decoded");
        report.x11 = rig::x11(cli.iterations).await.context("x11 scenario")?;
    }
    if all || matches!(cli.scenario, Scenario::Shaped) {
        eprintln!("== slow uplink: adaptive vs fixed bitrate");
        report.shaped = Some(rig::shaped().await.context("shaped scenario")?);
    }
    if all || matches!(cli.scenario, Scenario::Codec) {
        eprintln!("== codec cost per frame");
        report.codec = codec_bench();
    }
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(path) = cli.out {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, &json).with_context(|| format!("writing {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    println!("{json}");
    Ok(())
}

fn git_commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".into(), |s| s.trim().to_owned())
}

/// A text-like screen with a small changing region ("typing").
fn text_screen(w: u32, h: u32, tick: u32) -> Frame {
    let mut data = vec![255u8; (w * h * 4) as usize];
    for y in (8..h).step_by(18) {
        for x in (8..w.saturating_sub(8)).step_by(7) {
            if (x / 7 + y / 18 + 3) % 5 != 0 {
                for dy in 0..9 {
                    let row = ((y + dy) * w + x) as usize * 4;
                    data[row..row + 12]
                        .copy_from_slice(&[30, 30, 30, 255, 30, 30, 30, 255, 30, 30, 30, 255]);
                }
            }
        }
    }
    // The "typed" line: a few glyphs that change every tick.
    let x0 = 16 + (tick % 40) * 7;
    for dy in 0..12 {
        let row = ((h / 2 + dy) * w + x0.min(w - 16)) as usize * 4;
        data[row..row + 28].copy_from_slice(&[
            200, 40, 20, 255, 200, 40, 20, 255, 200, 40, 20, 255, 200, 40, 20, 255, 200, 40, 20,
            255, 200, 40, 20, 255, 200, 40, 20, 255,
        ]);
    }
    Frame {
        width: w,
        height: h,
        stride: (w * 4) as usize,
        format: FrameFormat::Bgra8,
        data: Bytes::from(data),
        captured_us: 0,
        dirty: vec![],
    }
}

fn codec_bench() -> Vec<CodecRow> {
    let mut rows = Vec::new();
    for (w, h) in [(1280u32, 720u32), (1920, 1080), (2560, 1440), (3840, 2160)] {
        let Ok(mut enc) = H264Encoder::new(EncoderConfig::screen(w, h, 60.0, 20_000_000)) else {
            continue;
        };
        let Ok(mut dec) = H264Decoder::new() else {
            continue;
        };
        let mut enc_ms = Vec::new();
        let mut dec_ms = Vec::new();
        let (mut key_bytes, mut delta_bytes, mut deltas) = (0usize, 0usize, 0usize);
        let frames = 40u32;
        for i in 0..frames {
            let f = text_screen(w, h, i);
            let t = Instant::now();
            let Ok(Some(out)) = enc.encode(&f, false) else {
                continue;
            };
            enc_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            if out.keyframe {
                key_bytes = out.data.len();
            } else {
                delta_bytes += out.data.len();
                deltas += 1;
            }
            let t = Instant::now();
            if let Ok(Some(pic)) = dec.decode(&out.data) {
                let _ = nexus_codec::i420_to_bgra(pic.width, pic.height, &pic.planes);
            }
            dec_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        // Skip the keyframe in the steady-state numbers.
        let mut e: Vec<f64> = enc_ms.iter().skip(1).copied().collect();
        let mut d: Vec<f64> = dec_ms.iter().skip(1).copied().collect();
        e.sort_by(f64::total_cmp);
        d.sort_by(f64::total_cmp);
        let p50 = percentile(&e, 0.5);
        rows.push(CodecRow {
            resolution: format!("{w}x{h}"),
            encode_p50_ms: p50,
            encode_p95_ms: percentile(&e, 0.95),
            decode_convert_p50_ms: percentile(&d, 0.5),
            keyframe_kib: key_bytes as f64 / 1024.0,
            delta_kib_typing: if deltas == 0 {
                0.0
            } else {
                delta_bytes as f64 / deltas as f64 / 1024.0
            },
            max_fps_encode: if p50 > 0.0 { 1000.0 / p50 } else { 0.0 },
        });
        eprintln!(
            "   {w}x{h}: encode p50 {p50:.1} ms, decode+convert p50 {:.1} ms",
            percentile(&d, 0.5)
        );
    }
    rows
}

#[allow(dead_code)]
fn settle() -> Duration {
    Duration::from_millis(30)
}
