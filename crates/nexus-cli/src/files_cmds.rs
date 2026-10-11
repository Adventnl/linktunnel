//! File commands: `nexus ls`, `nexus cp`, `nexus mkdir`, `nexus rm`.
//!
//! A remote path is written `DEVICE:PATH` (the device by name or ID, the path as the device
//! itself spells it); anything else is a local path. Every command opens a short control-only
//! session through the local agent, does its work and closes it.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use nexus_core::i18n::{tr, tr_args};
use nexus_ipc::{EventReceiver, IpcClient};
use nexus_proto::v1::{FileTransfer, OpenSession, ipc_event::Body as Ev};

use crate::AppContext;
use crate::agent_cmds::{connect_with_events, resolve};

/// One side of a copy.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Location {
    Local(PathBuf),
    Remote { device: String, path: String },
}

/// Parses `DEVICE:PATH`. A leading single letter followed by `\` or `/` is a Windows drive, not
/// a device name; a spec without a colon is local.
pub(crate) fn parse_location(spec: &str) -> Location {
    if let Some((head, tail)) = spec.split_once(':') {
        let drive = head.len() == 1
            && head.chars().all(|c| c.is_ascii_alphabetic())
            && (tail.starts_with('\\') || tail.starts_with('/'));
        if !drive && !head.is_empty() {
            return Location::Remote {
                device: head.to_owned(),
                path: tail.to_owned(),
            };
        }
    }
    Location::Local(PathBuf::from(spec))
}

struct Live {
    client: IpcClient,
    events: EventReceiver,
    session_id: u64,
}

async fn open(ctx: &AppContext, device: &str) -> Result<Live> {
    let (client, events) = connect_with_events(ctx).await?;
    let devices = client.list_devices().await?.devices;
    let dev = resolve(ctx.locale, &devices, device)?;
    let opened = client
        .open_session(OpenSession {
            device_id: dev.device_id.clone(),
            want_video: false,
            ..Default::default()
        })
        .await
        .with_context(|| tr_args(ctx.locale, "cli.remote.open_failed", &[("name", &dev.name)]))?;
    Ok(Live {
        client,
        events,
        session_id: opened.session_id,
    })
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    #[allow(clippy::cast_precision_loss)]
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

fn remote_only(spec: &str) -> Result<(String, String)> {
    match parse_location(spec) {
        Location::Remote { device, path } => Ok((device, path)),
        Location::Local(_) => bail!("{spec}: expected DEVICE:PATH"),
    }
}

pub(crate) async fn ls(ctx: &AppContext, spec: &str) -> Result<ExitCode> {
    // `DEVICE` or `DEVICE:` lists the shared folders.
    let (device, path) = match parse_location(spec) {
        Location::Remote { device, path } => (device, path),
        Location::Local(p) => (p.to_string_lossy().into_owned(), String::new()),
    };
    let live = open(ctx, &device).await?;
    let result = live.client.file_list(live.session_id, &path).await;
    let _ = live.client.close_session(live.session_id).await;
    let listing = result?;
    if path.is_empty() {
        println!(
            "{}",
            tr_args(ctx.locale, "cli.files.roots", &[("name", &device)])
        );
    }
    for e in &listing.entries {
        let kind = if e.is_symlink {
            "l"
        } else if e.is_dir {
            "d"
        } else {
            "-"
        };
        let size = if e.is_dir {
            String::new()
        } else {
            human(e.size)
        };
        println!(
            "{}",
            tr_args(
                ctx.locale,
                "cli.files.entry",
                &[
                    ("kind", kind),
                    ("size", &format!("{size:>10}")),
                    ("name", &e.name)
                ]
            )
        );
    }
    if listing.truncated {
        println!("{}", tr(ctx.locale, "cli.files.truncated"));
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) async fn mkdir(ctx: &AppContext, spec: &str) -> Result<ExitCode> {
    let (device, path) = remote_only(spec)?;
    let live = open(ctx, &device).await?;
    let result = live.client.file_mkdir(live.session_id, &path).await;
    let _ = live.client.close_session(live.session_id).await;
    result?;
    println!("{}", tr(ctx.locale, "cli.files.created"));
    Ok(ExitCode::SUCCESS)
}

pub(crate) async fn rm(ctx: &AppContext, spec: &str, recursive: bool) -> Result<ExitCode> {
    let (device, path) = remote_only(spec)?;
    let live = open(ctx, &device).await?;
    let result = live
        .client
        .file_remove(live.session_id, &path, recursive)
        .await;
    let _ = live.client.close_session(live.session_id).await;
    result?;
    println!("{}", tr(ctx.locale, "cli.files.removed"));
    Ok(ExitCode::SUCCESS)
}

pub(crate) async fn cp(
    ctx: &AppContext,
    src: &str,
    dst: &str,
    overwrite: bool,
) -> Result<ExitCode> {
    let (device, remote, local, upload) = match (parse_location(src), parse_location(dst)) {
        (Location::Remote { device, path }, Location::Local(local)) => {
            // Copying into a directory keeps the file name.
            let name = remote_name(&path);
            let local = if local.is_dir() {
                local.join(name)
            } else {
                local
            };
            (device, path, local, false)
        }
        (Location::Local(local), Location::Remote { device, path }) => {
            let path = if path.is_empty() || path.ends_with('/') || path.ends_with('\\') {
                let name = local
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .context("the source has no file name")?;
                format!("{path}{name}")
            } else {
                path
            };
            (device, path, local, true)
        }
        _ => bail!("{}", tr(ctx.locale, "cli.files.need_remote")),
    };
    let local = absolute(&local)?;
    let mut live = open(ctx, &device).await?;
    let started = live
        .client
        .file_transfer(FileTransfer {
            session_id: live.session_id,
            remote_path: remote,
            local_path: local.to_string_lossy().into_owned(),
            upload,
            overwrite,
        })
        .await;
    let started = match started {
        Ok(s) => s,
        Err(e) => {
            let _ = live.client.close_session(live.session_id).await;
            return Err(e.into());
        }
    };
    let dest_label = if upload {
        dst.to_owned()
    } else {
        local.display().to_string()
    };
    let outcome = loop {
        let Some(ev) = live.events.next().await else {
            break Err(anyhow::anyhow!("the agent closed the connection"));
        };
        match ev.body {
            Some(Ev::TransferProgress(p)) if p.transfer_id == started.transfer_id => {
                let percent = (p.done * 100).checked_div(p.total).unwrap_or(100);
                eprint!(
                    "\r{}   ",
                    tr_args(
                        ctx.locale,
                        "cli.files.progress",
                        &[
                            ("done", &human(p.done)),
                            ("total", &human(p.total)),
                            ("percent", &percent.to_string())
                        ]
                    )
                );
                let _ = std::io::stderr().flush();
            }
            Some(Ev::TransferFinished(f)) if f.transfer_id == started.transfer_id => {
                eprintln!();
                break if f.ok {
                    Ok(f)
                } else {
                    Err(anyhow::anyhow!(
                        "{}",
                        tr_args(
                            ctx.locale,
                            "cli.files.failed",
                            &[("reason", &f.error_message)]
                        )
                    ))
                };
            }
            Some(Ev::SessionEnded(e)) if e.session_id == live.session_id => {
                break Err(anyhow::anyhow!("{}", e.reason));
            }
            _ => {}
        }
    };
    let _ = live.client.close_session(live.session_id).await;
    let f = outcome?;
    let hash: String = f
        .blake3
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    let resumed = if f.resumed_from > 0 {
        tr_args(
            ctx.locale,
            "cli.files.resumed",
            &[("offset", &human(f.resumed_from))],
        )
    } else {
        String::new()
    };
    println!(
        "{}",
        tr_args(
            ctx.locale,
            "cli.files.done",
            &[
                ("size", &human(f.size)),
                ("dest", &dest_label),
                ("hash", &format!("{hash}…")),
                ("resumed", &resumed)
            ]
        )
    );
    Ok(ExitCode::SUCCESS)
}

fn remote_name(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("download")
        .to_owned()
}

fn absolute(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_parse() {
        assert_eq!(
            parse_location("mac:/Users/me/a.txt"),
            Location::Remote {
                device: "mac".into(),
                path: "/Users/me/a.txt".into()
            }
        );
        assert_eq!(
            parse_location("pc:C:\\Users\\me"),
            Location::Remote {
                device: "pc".into(),
                path: "C:\\Users\\me".into()
            }
        );
        assert_eq!(
            parse_location("C:\\Users\\me"),
            Location::Local("C:\\Users\\me".into())
        );
        assert_eq!(parse_location("D:/x"), Location::Local("D:/x".into()));
        assert_eq!(
            parse_location("./rel/file"),
            Location::Local("./rel/file".into())
        );
        assert_eq!(
            parse_location("server:"),
            Location::Remote {
                device: "server".into(),
                path: String::new()
            }
        );
    }

    #[test]
    fn names_and_sizes() {
        assert_eq!(remote_name("/a/b/c.txt"), "c.txt");
        assert_eq!(remote_name("C:\\a\\b\\"), "b");
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KiB");
    }
}
