//! Remote-desktop commands: look at a paired device's screen and drive its keyboard and mouse.
//!
//! These run a short viewer session through the local agent: open, do one thing, close. The full
//! interactive viewer is a separate window (`nexus view`).

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::BytesMut;
use clap::Subcommand;
use nexus_core::i18n::{tr, tr_args};
use nexus_input::keys;
use nexus_ipc::{EventReceiver, IpcClient};
use nexus_media::controller::VideoFrame;
use nexus_media::viewer::{CursorOverlay, VideoDecoder};
use nexus_proto::v1::{OpenSession, SessionOpened, ipc_event::Body as Ev};
use nexus_proto::wire::cursor::CursorUpdate;
use nexus_proto::wire::input::{
    InputEvent, InputPacket, KeyState, Modifiers, MouseButton, TimedInput,
};
use nexus_proto::wire::video::{FIXED_LEN, VideoFrameHeader};

use crate::AppContext;
use crate::agent_cmds::{connect_with_events, resolve};

#[derive(Debug, Subcommand)]
pub enum RemoteCommand {
    /// Save a picture of a paired device's screen.
    Screenshot {
        /// Device name or ID.
        device: String,
        /// Output file (PNG).
        #[arg(short, long, default_value = "screenshot.png")]
        output: String,
        /// Which monitor.
        #[arg(long, default_value_t = 0)]
        monitor: u32,
        /// Draw the mouse cursor into the picture.
        #[arg(long)]
        cursor: bool,
    },
    /// Open a live window onto a paired device's screen and control it.
    View {
        /// Device name or ID.
        device: String,
        /// Which monitor.
        #[arg(long, default_value_t = 0)]
        monitor: u32,
        /// Frame-rate cap.
        #[arg(long, default_value_t = 0)]
        fps: u32,
        /// Starting bitrate in Mbit/s.
        #[arg(long, default_value_t = 0)]
        mbps: u32,
        /// Watch only; send no input.
        #[arg(long)]
        view_only: bool,
        /// Do not share the clipboard with the device.
        #[arg(long)]
        no_clipboard: bool,
    },
    /// List a paired device's monitors.
    Monitors {
        /// Device name or ID.
        device: String,
    },
    /// Type text on a paired device (layout-independent, handles any Unicode).
    Type {
        /// Device name or ID.
        device: String,
        /// The text.
        text: String,
    },
    /// Press a key or chord on a paired device, e.g. `enter`, `ctrl+shift+t`, `alt+f4`.
    Key {
        /// Device name or ID.
        device: String,
        /// The chord.
        chord: String,
    },
    /// Move the pointer and click on a paired device.
    Click {
        /// Device name or ID.
        device: String,
        /// X in monitor pixels.
        x: i32,
        /// Y in monitor pixels.
        y: i32,
        /// `left`, `right` or `middle`.
        #[arg(long, default_value = "left")]
        button: String,
        /// Click twice.
        #[arg(long)]
        double: bool,
        /// Which monitor.
        #[arg(long, default_value_t = 0)]
        monitor: u32,
    },
}

/// An open viewer session plus what is needed to drive it.
struct Live {
    client: IpcClient,
    events: EventReceiver,
    opened: SessionOpened,
}

async fn open(ctx: &AppContext, query: &str, monitor: u32, want_video: bool) -> Result<Live> {
    let (client, events) = connect_with_events(ctx).await?;
    let devices = client.list_devices().await?.devices;
    let device = resolve(ctx.locale, &devices, query)?;
    let opened = client
        .open_session(OpenSession {
            device_id: device.device_id.clone(),
            monitor,
            want_video,
            ..Default::default()
        })
        .await
        .with_context(|| {
            tr_args(
                ctx.locale,
                "cli.remote.open_failed",
                &[("name", &device.name)],
            )
        })?;
    Ok(Live {
        client,
        events,
        opened,
    })
}

fn encode_packet(events: Vec<InputEvent>) -> Result<Vec<u8>> {
    let packet = InputPacket {
        seq: 0,
        base_us: 0,
        events: events
            .into_iter()
            .map(|event| TimedInput { dt_us: 0, event })
            .collect(),
        skipped: 0,
    };
    let mut buf = BytesMut::new();
    packet
        .encode(&mut buf)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(buf.to_vec())
}

async fn send(live: &Live, events: Vec<InputEvent>) -> Result<()> {
    for chunk in events.chunks(nexus_proto::wire::input::MAX_EVENTS) {
        live.client
            .session_input(live.opened.session_id, encode_packet(chunk.to_vec())?)
            .await?;
    }
    Ok(())
}

pub async fn run(ctx: &AppContext, cmd: RemoteCommand) -> Result<ExitCode> {
    match cmd {
        RemoteCommand::Screenshot {
            device,
            output,
            monitor,
            cursor,
        } => screenshot(ctx, &device, &output, monitor, cursor).await,
        RemoteCommand::View {
            device,
            monitor,
            fps,
            mbps,
            view_only,
            no_clipboard,
        } => view(ctx, &device, monitor, fps, mbps, view_only, !no_clipboard).await,
        RemoteCommand::Monitors { device } => monitors(ctx, &device).await,
        RemoteCommand::Type { device, text } => {
            let live = open(ctx, &device, 0, false).await?;
            send(&live, vec![InputEvent::Text { text }]).await?;
            finish(ctx, live).await
        }
        RemoteCommand::Key { device, chord } => {
            let parsed = keys::parse_chord(&chord).map_err(|e| anyhow::anyhow!("{e}"))?;
            let key = |usage, state| InputEvent::Key {
                page: nexus_proto::wire::input::PAGE_KEYBOARD,
                usage,
                state,
                modifiers: Modifiers::default(),
            };
            let mut events: Vec<InputEvent> = parsed
                .usages
                .iter()
                .map(|u| key(*u, KeyState::Down))
                .collect();
            events.extend(parsed.usages.iter().rev().map(|u| key(*u, KeyState::Up)));
            let live = open(ctx, &device, 0, false).await?;
            send(&live, events).await?;
            finish(ctx, live).await
        }
        RemoteCommand::Click {
            device,
            x,
            y,
            button,
            double,
            monitor,
        } => {
            let button = match button.to_lowercase().as_str() {
                "left" => MouseButton::LEFT,
                "right" => MouseButton::RIGHT,
                "middle" => MouseButton::MIDDLE,
                other => bail!("unknown mouse button '{other}' (left, right, middle)"),
            };
            let mut events = vec![InputEvent::MouseMoveAbs {
                monitor: u8::try_from(monitor).context("monitor")?,
                x,
                y,
            }];
            for _ in 0..(if double { 2 } else { 1 }) {
                events.push(InputEvent::MouseButton {
                    button,
                    pressed: true,
                });
                events.push(InputEvent::MouseButton {
                    button,
                    pressed: false,
                });
            }
            let live = open(ctx, &device, monitor, false).await?;
            send(&live, events).await?;
            finish(ctx, live).await
        }
    }
}

async fn finish(ctx: &AppContext, live: Live) -> Result<ExitCode> {
    // Let the input drain through the agent before the session is torn down.
    tokio::time::sleep(Duration::from_millis(150)).await;
    live.client.close_session(live.opened.session_id).await?;
    println!("{}", tr(ctx.locale, "cli.remote.done"));
    Ok(ExitCode::SUCCESS)
}

async fn view(
    ctx: &AppContext,
    query: &str,
    monitor: u32,
    fps: u32,
    mbps: u32,
    view_only: bool,
    clipboard: bool,
) -> Result<ExitCode> {
    let client = crate::agent_cmds::connect(ctx).await?;
    let devices = client.list_devices().await?.devices;
    let device = resolve(ctx.locale, &devices, query)?;
    drop(client);
    let options = nexus_render::ViewerOptions {
        ipc: nexus_ipc::IpcPaths::in_dir(&ctx.paths.runtime_dir),
        device_id: device.device_id.to_vec(),
        device_name: device.name.clone(),
        monitor,
        max_fps: fps,
        bitrate_bps: mbps.saturating_mul(1_000_000),
        view_only,
        clipboard,
    };
    // The window owns the main thread; it runs its own network thread.
    let reason = tokio::task::block_in_place(|| nexus_render::run(options))
        .map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "{}",
        tr_args(ctx.locale, "cli.view.ended", &[("reason", &reason)])
    );
    Ok(ExitCode::SUCCESS)
}

async fn monitors(ctx: &AppContext, query: &str) -> Result<ExitCode> {
    let live = open(ctx, query, 0, true).await?;
    for m in &live.opened.monitors {
        println!(
            "{}",
            tr_args(
                ctx.locale,
                "cli.monitors.row",
                &[
                    ("index", &m.index.to_string()),
                    ("name", &m.name),
                    ("size", &format!("{}x{}", m.width, m.height)),
                    ("pos", &format!("{},{}", m.x, m.y)),
                    ("primary", if m.primary { "*" } else { "" }),
                ]
            )
        );
    }
    live.client.close_session(live.opened.session_id).await?;
    Ok(ExitCode::SUCCESS)
}

async fn screenshot(
    ctx: &AppContext,
    query: &str,
    output: &str,
    monitor: u32,
    with_cursor: bool,
) -> Result<ExitCode> {
    let mut live = open(ctx, query, monitor, true).await?;
    let mut decoder = VideoDecoder::new();
    let mut overlay = CursorOverlay::new();
    let mut cursor: Option<CursorUpdate> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut picture = None;
    let mut cursor_wait: Option<tokio::time::Instant> = None;
    while tokio::time::Instant::now() < deadline {
        let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(500), live.events.next()).await
        else {
            if picture.is_some() {
                break;
            }
            continue;
        };
        match ev.body {
            Some(Ev::SessionVideo(v)) if picture.is_none() => {
                let Some(frame) = parse_frame(&v.frame) else {
                    continue;
                };
                match decoder.decode(&frame) {
                    Ok(Some(p)) => {
                        picture = Some(p);
                        // Give the cursor channel a moment: positions arrive separately.
                        cursor_wait = Some(
                            tokio::time::Instant::now()
                                + Duration::from_millis(if with_cursor { 400 } else { 0 }),
                        );
                    }
                    Ok(None) => {}
                    Err(_) => {
                        live.client.session_keyframe(live.opened.session_id).await?;
                    }
                }
            }
            Some(Ev::SessionCursorShape(s)) => {
                if let Some(shape) = s.shape {
                    overlay.add_shape(shape);
                }
            }
            Some(Ev::SessionCursorPos(c)) => cursor = CursorUpdate::decode(&c.update).ok(),
            Some(Ev::SessionEnded(e)) => bail!("{}", e.reason),
            _ => {}
        }
        if picture.is_some()
            && cursor_wait.is_none_or(|t| tokio::time::Instant::now() >= t)
            && (!with_cursor || cursor.is_some())
        {
            break;
        }
    }
    let Some(pic) = picture else {
        bail!("{}", tr(ctx.locale, "cli.screenshot.timeout"))
    };
    let mut bgra = pic.bgra.to_vec();
    if with_cursor && let Some(c) = &cursor {
        overlay.compose(&mut bgra, pic.width, pic.height, c);
    }
    write_png(Path::new(output), pic.width, pic.height, &bgra)?;
    live.client.close_session(live.opened.session_id).await?;
    println!(
        "{}",
        tr_args(
            ctx.locale,
            "cli.screenshot.saved",
            &[
                ("path", output),
                ("size", &format!("{}x{}", pic.width, pic.height))
            ]
        )
    );
    Ok(ExitCode::SUCCESS)
}

/// Splits an IPC video blob into header and payload.
pub fn parse_frame(blob: &[u8]) -> Option<VideoFrame> {
    let fixed: [u8; FIXED_LEN] = blob.get(..FIXED_LEN)?.try_into().ok()?;
    let head_len = FIXED_LEN + VideoFrameHeader::extra_len(&fixed).ok()?;
    let header = VideoFrameHeader::decode(blob.get(..head_len)?).ok()?;
    let payload = blob.get(head_len..)?;
    (payload.len() == header.payload_len as usize).then(|| VideoFrame {
        header,
        payload: bytes::Bytes::copy_from_slice(payload),
        recv_us: nexus_core::clock::now_us(),
    })
}

fn write_png(path: &Path, width: u32, height: u32, bgra: &[u8]) -> Result<()> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    let file =
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip_swaps_channels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.png");
        // One BGRA pixel: blue=1, green=2, red=3.
        write_png(&path, 1, 1, &[1, 2, 3, 255]).unwrap();
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();
        assert_eq!(&buf[..4], &[3, 2, 1, 255]);
    }

    #[test]
    fn truncated_or_lying_frames_are_rejected() {
        assert!(parse_frame(&[]).is_none());
        assert!(parse_frame(&[0u8; 10]).is_none());
        assert!(parse_frame(&[1u8; 60]).is_none());
    }
}
