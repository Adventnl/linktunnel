//! Loopback comparison of the three transport candidates in ADR-0003.
//!
//! Every candidate carries the same three traffic classes the product uses:
//!
//! * small reliable messages (input events, control RPC), measured as echo round trips;
//! * small unreliable messages (cursor, audio), measured as echo round trips plus loss;
//! * bulk reliable data (file transfer, a keyframe burst), measured as throughput.
//!
//! LOOPBACK MEASURES STACK OVERHEAD ONLY. It says nothing about NAT traversal, loss,
//! jitter or relay behaviour; it can veto a stack that is unacceptably slow, not prove
//! one fit. Run: `cargo run --release -- all`.

mod iroh_bench;
mod quinn_bench;
mod webrtc_bench;

use clap::{Parser, ValueEnum};
use nexus_core::telemetry::LatencySummary;

#[derive(Clone, Copy, ValueEnum)]
enum Which {
    Iroh,
    Quinn,
    Webrtc,
    All,
}

#[derive(Parser)]
struct Args {
    #[arg(value_enum, default_value = "all")]
    which: Which,
    /// Echo round trips per traffic class.
    #[arg(long, default_value_t = 2000)]
    pings: usize,
    /// Megabytes for the throughput test.
    #[arg(long, default_value_t = 64)]
    bulk_mb: usize,
}

pub struct Outcome {
    pub name: &'static str,
    pub reliable: LatencySummary,
    pub unreliable: LatencySummary,
    pub loss: f64,
    pub throughput_mbps: f64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut outcomes = Vec::new();
    if matches!(args.which, Which::Iroh | Which::All) {
        outcomes.push(iroh_bench::run(args.pings, args.bulk_mb).await?);
    }
    if matches!(args.which, Which::Quinn | Which::All) {
        outcomes.push(quinn_bench::run(args.pings, args.bulk_mb).await?);
    }
    if matches!(args.which, Which::Webrtc | Which::All) {
        outcomes.push(webrtc_bench::run(args.pings, args.bulk_mb).await?);
    }

    println!("\n| transport | reliable p50 | p95 | p99 | unreliable p50 | p95 | loss | bulk |");
    println!("|---|---|---|---|---|---|---|---|");
    for o in &outcomes {
        let ms = |us: u64| format!("{:.3} ms", us as f64 / 1000.0);
        println!(
            "| {} | {} | {} | {} | {} | {} | {:.1} % | {:.0} Mbit/s |",
            o.name,
            ms(o.reliable.p50_us),
            ms(o.reliable.p95_us),
            ms(o.reliable.p99_us),
            ms(o.unreliable.p50_us),
            ms(o.unreliable.p95_us),
            o.loss * 100.0,
            o.throughput_mbps
        );
    }
    Ok(())
}
