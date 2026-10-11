//! Commands that talk to the running agent over IPC.

use std::io::Write;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use nexus_core::i18n::{Locale, tr, tr_args};
use nexus_ipc::{IpcClient, IpcPaths};
use nexus_proto::v1::{
    DeviceView, IpcEvent, PathKind, PingDevice, PingReport, PresenceState, ipc_event::Body as Ev,
};
use nexus_trust::grants;

use crate::AppContext;

#[derive(Debug, Subcommand)]
pub enum PairCommand {
    /// Show a one-time code and wait for another device to enter it.
    Offer {
        /// What the other device may do on this one (default: everything).
        #[arg(long = "allow", value_name = "FEATURE")]
        allow: Vec<String>,
        /// Skip the human word comparison. TESTING ONLY: it disables man-in-the-middle detection.
        #[arg(long)]
        assume_words_match: bool,
    },
    /// Enter the code shown by another device.
    Join {
        /// The six words shown by `nexus pair offer`.
        code: String,
        /// What you ask to be allowed to do on the other device (default: everything).
        #[arg(long = "request", value_name = "FEATURE")]
        request: Vec<String>,
        /// What the other device may do on this one (default: view the screen).
        #[arg(long = "grant", value_name = "FEATURE")]
        grant: Vec<String>,
        /// Skip the human word comparison. TESTING ONLY: it disables man-in-the-middle detection.
        #[arg(long)]
        assume_words_match: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum LockdownCommand {
    /// Refuse all remote access and drop live sessions.
    On,
    /// Allow remote access again.
    Off,
}

pub async fn connect(ctx: &AppContext) -> Result<IpcClient> {
    let paths = IpcPaths::in_dir(&ctx.paths.runtime_dir);
    match IpcClient::connect(&paths, "nexus-cli").await {
        Ok((client, _events, _welcome)) => Ok(client),
        Err(e) => bail!(
            "{}",
            tr_args(
                ctx.locale,
                "cli.agent.unreachable",
                &[("reason", &e.to_string())]
            )
        ),
    }
}

pub(crate) async fn connect_with_events(
    ctx: &AppContext,
) -> Result<(IpcClient, nexus_ipc::EventReceiver)> {
    let paths = IpcPaths::in_dir(&ctx.paths.runtime_dir);
    match IpcClient::connect(&paths, "nexus-cli").await {
        Ok((client, events, _welcome)) => Ok((client, events)),
        Err(e) => bail!(
            "{}",
            tr_args(
                ctx.locale,
                "cli.agent.unreachable",
                &[("reason", &e.to_string())]
            )
        ),
    }
}

fn state_label(l: Locale, state: i32, online: bool) -> String {
    let key = match PresenceState::try_from(state).unwrap_or(PresenceState::Unspecified) {
        PresenceState::Online => "state.online",
        PresenceState::Idle => "state.idle",
        PresenceState::Locked => "state.locked",
        PresenceState::InUse => "state.in_use",
        PresenceState::Sleeping => "state.sleeping",
        PresenceState::Hibernating => "state.hibernating",
        PresenceState::Offline => "state.offline",
        PresenceState::Unspecified => "state.unknown",
    };
    let _ = online;
    tr(l, key)
}

fn path_label(l: Locale, path: i32) -> String {
    let key = match PathKind::try_from(path).unwrap_or(PathKind::Unspecified) {
        PathKind::Lan => "path.lan",
        PathKind::Direct => "path.direct",
        PathKind::Relay => "path.relay",
        PathKind::Unspecified => "path.none",
    };
    tr(l, key)
}

fn short_id(id: &[u8]) -> String {
    nexus_crypto::DeviceId::from_slice(id).map_or_else(|_| "?".into(), |d| d.to_string())
}

/// Finds one paired device by exact/prefix ID or by name (case-insensitive).
pub(crate) fn resolve(l: Locale, devices: &[DeviceView], query: &str) -> Result<DeviceView> {
    let q = query.trim().to_lowercase();
    let matches: Vec<&DeviceView> = devices
        .iter()
        .filter(|d| {
            let id = short_id(&d.device_id).to_lowercase();
            d.name.to_lowercase() == q
                || id == q
                || id.replace('-', "").starts_with(&q.replace('-', ""))
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!(
            "{}",
            tr_args(l, "cli.device_not_found", &[("query", query)])
        ),
        _ => bail!(
            "{}",
            tr_args(l, "cli.device_ambiguous", &[("query", query)])
        ),
    }
}

pub async fn status(ctx: &AppContext) -> Result<ExitCode> {
    let l = ctx.locale;
    let s = connect(ctx).await?.status().await?;
    let onoff = |b: bool| tr(l, if b { "cli.on" } else { "cli.off" });
    println!(
        "{:<18} {} ({})",
        tr(l, "cli.status.name"),
        s.device_name,
        s.agent_version
    );
    println!(
        "{:<18} {}",
        tr(l, "cli.status.device"),
        short_id(&s.device_id)
    );
    println!(
        "{:<18} {}",
        tr(l, "cli.identity.fingerprint"),
        s.fingerprint
    );
    println!(
        "{:<18} {}",
        tr(l, "cli.status.signal"),
        if s.signal_connected {
            format!("{} ({})", tr(l, "cli.status.connected"), s.signal_url)
        } else {
            tr(l, "cli.status.disconnected")
        }
    );
    println!(
        "{:<18} {}",
        tr(l, "cli.status.relay"),
        if s.relay_url.is_empty() {
            tr(l, "cli.none")
        } else {
            s.relay_url.clone()
        }
    );
    println!(
        "{:<18} {}",
        tr(l, "cli.status.addresses"),
        if s.direct_addresses.is_empty() {
            tr(l, "cli.none")
        } else {
            s.direct_addresses.join(", ")
        }
    );
    println!("{:<18} {}", tr(l, "cli.status.paired"), s.paired_devices);
    println!("{:<18} {}", tr(l, "cli.status.sessions"), s.active_sessions);
    println!("{:<18} {}", tr(l, "cli.status.lockdown"), onoff(s.lockdown));
    Ok(ExitCode::SUCCESS)
}

pub async fn devices(ctx: &AppContext) -> Result<ExitCode> {
    let l = ctx.locale;
    let list = connect(ctx).await?.list_devices().await?;
    if list.devices.is_empty() {
        println!("{}", tr(l, "cli.devices.empty"));
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{:<22} {:<26} {:<10} {:<12} {}",
        tr(l, "cli.devices.col_name"),
        tr(l, "cli.devices.col_state"),
        tr(l, "cli.devices.col_path"),
        tr(l, "cli.devices.col_access"),
        tr(l, "cli.devices.col_id")
    );
    for d in &list.devices {
        println!(
            "{:<22} {:<26} {:<10} {:<12} {}",
            d.name,
            state_label(l, d.state, d.online),
            path_label(l, d.path),
            tr(
                l,
                if d.unattended {
                    "cli.devices.unattended"
                } else {
                    "cli.devices.attended"
                }
            ),
            short_id(&d.device_id)
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Waits for the pairing's words, asks the human, then waits for the result.
async fn finish_pairing(
    ctx: &AppContext,
    client: &IpcClient,
    mut events: nexus_ipc::EventReceiver,
    pairing_id: u64,
    assume_match: bool,
) -> Result<ExitCode> {
    let l = ctx.locale;
    if assume_match {
        eprintln!("{}", tr(l, "cli.pair.auto_warning"));
    }
    loop {
        let Some(IpcEvent { body: Some(body) }) = events.next().await else {
            bail!("lost the connection to the agent");
        };
        match body {
            Ev::PairingSas(s) if s.pairing_id == pairing_id => {
                println!("\n{}\n", tr(l, "cli.pair.compare"));
                println!("    {}   ({})\n", s.words.join("  "), s.digits);
                println!("  {} ({})\n", s.peer_name, s.peer_fingerprint);
                let matches = if assume_match {
                    true
                } else {
                    ask_yes_no(&tr(l, "cli.pair.confirm")).await?
                };
                client.pairing_confirm(pairing_id, matches).await?;
            }
            Ev::PairingFinished(f) if f.pairing_id == pairing_id => {
                return if f.success {
                    println!(
                        "{}",
                        tr_args(
                            l,
                            "cli.pair.ok",
                            &[("name", &f.peer_name), ("id", &short_id(&f.peer_device_id))]
                        )
                    );
                    Ok(ExitCode::SUCCESS)
                } else {
                    eprintln!(
                        "{}",
                        tr_args(l, "cli.pair.failed", &[("reason", &f.reason)])
                    );
                    Ok(ExitCode::FAILURE)
                };
            }
            _ => {}
        }
    }
}

async fn ask_yes_no(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let answer = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| line)
    })
    .await??;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

pub async fn pair(ctx: &AppContext, cmd: &PairCommand) -> Result<ExitCode> {
    let l = ctx.locale;
    let (client, events) = connect_with_events(ctx).await?;
    match cmd {
        PairCommand::Offer {
            allow,
            assume_words_match,
        } => {
            let allow = if allow.is_empty() {
                grants::all()
            } else {
                allow.clone()
            };
            let started = client.pairing_offer(allow).await?;
            println!(
                "{}\n",
                tr_args(l, "cli.pair.offer_code", &[("code", &started.code)])
            );
            println!("{}", tr(l, "cli.pair.waiting"));
            finish_pairing(
                ctx,
                &client,
                events,
                started.pairing_id,
                *assume_words_match,
            )
            .await
        }
        PairCommand::Join {
            code,
            request,
            grant,
            assume_words_match,
        } => {
            let request = if request.is_empty() {
                grants::all()
            } else {
                request.clone()
            };
            let grant = if grant.is_empty() {
                vec![grants::SCREEN.to_owned()]
            } else {
                grant.clone()
            };
            let started = client.pairing_join(code, request, grant).await?;
            println!("{}", tr(l, "cli.pair.waiting"));
            finish_pairing(
                ctx,
                &client,
                events,
                started.pairing_id,
                *assume_words_match,
            )
            .await
        }
    }
}

pub async fn unpair(ctx: &AppContext, query: &str) -> Result<ExitCode> {
    let l = ctx.locale;
    let client = connect(ctx).await?;
    let device = resolve(l, &client.list_devices().await?.devices, query)?;
    client.unpair(device.device_id.to_vec()).await?;
    println!("{}", tr_args(l, "cli.unpair.ok", &[("name", &device.name)]));
    Ok(ExitCode::SUCCESS)
}

fn print_latency(l: Locale, label_key: &str, v: Option<&nexus_proto::v1::LatencyView>) {
    let ms = |us: u64| {
        #[allow(clippy::cast_precision_loss)]
        let m = us as f64 / 1000.0;
        m
    };
    if let Some(v) = v {
        println!(
            "  {:<26} n={:<5} p50 {:>7.2} ms   p95 {:>7.2} ms   p99 {:>7.2} ms   max {:>7.2} ms",
            tr(l, label_key),
            v.count,
            ms(v.p50_us),
            ms(v.p95_us),
            ms(v.p99_us),
            ms(v.max_us)
        );
    }
}

pub async fn ping(ctx: &AppContext, query: &str, bulk_mb: u64, json: bool) -> Result<ExitCode> {
    let l = ctx.locale;
    let client = connect(ctx).await?;
    let device = resolve(l, &client.list_devices().await?.devices, query)?;
    let report: PingReport = client
        .ping(PingDevice {
            device_id: device.device_id.clone(),
            bulk_bytes: bulk_mb * 1024 * 1024,
            ..Default::default()
        })
        .await
        .context("diagnostics")?;
    if json {
        let ms = |v: &Option<nexus_proto::v1::LatencyView>| {
            v.as_ref().map(|v| {
                serde_json::json!({"count": v.count, "p50_us": v.p50_us, "p95_us": v.p95_us, "p99_us": v.p99_us, "max_us": v.max_us})
            })
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "device": device.name,
                "control": ms(&report.control),
                "stream": ms(&report.stream),
                "datagram": ms(&report.datagram),
                "datagram_loss": report.datagram_loss,
                "throughput_mbps": report.throughput_mbps,
                "path": path_label(Locale::En, report.path_after),
                "remote": report.remote,
            }))?
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{}\n",
        tr_args(l, "cli.ping.title", &[("name", &device.name)])
    );
    print_latency(l, "cli.ping.control", report.control.as_ref());
    print_latency(l, "cli.ping.stream", report.stream.as_ref());
    print_latency(l, "cli.ping.datagram", report.datagram.as_ref());
    println!(
        "  {:<26} {:.1} %",
        tr(l, "cli.ping.loss"),
        report.datagram_loss * 100.0
    );
    println!(
        "  {:<26} {:.0} Mbit/s",
        tr(l, "cli.ping.throughput"),
        report.throughput_mbps
    );
    println!(
        "  {:<26} {} ({})",
        tr(l, "cli.ping.path"),
        path_label(l, report.path_after),
        report.remote
    );
    Ok(ExitCode::SUCCESS)
}

pub async fn lockdown(ctx: &AppContext, cmd: &LockdownCommand) -> Result<ExitCode> {
    let l = ctx.locale;
    let client = connect(ctx).await?;
    let enable = matches!(cmd, LockdownCommand::On);
    client.set_lockdown(enable).await?;
    println!(
        "{}",
        tr(
            l,
            if enable {
                "cli.lockdown.on"
            } else {
                "cli.lockdown.off"
            }
        )
    );
    Ok(ExitCode::SUCCESS)
}

pub async fn stop(ctx: &AppContext) -> Result<ExitCode> {
    // The agent may close the connection as it exits, before or just after acknowledging.
    match connect(ctx).await?.shutdown().await {
        Ok(()) | Err(nexus_ipc::IpcError::Closed) => {}
        Err(e) => return Err(e.into()),
    }
    println!("{}", tr(ctx.locale, "cli.agent.stopping"));
    Ok(ExitCode::SUCCESS)
}
