//! `nexus shell`: a terminal on a paired device.

use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use nexus_core::i18n::tr_args;
use nexus_proto::v1::{OpenSession, OpenTerminal, ipc_event::Body as Ev};
use tokio::sync::mpsc;

use crate::AppContext;
use crate::agent_cmds::{connect_with_events, resolve};

/// Ctrl-] : the escape key, like telnet's.
const ESCAPE: u8 = 0x1d;

/// Puts the terminal in raw mode and restores it however the function ends.
struct RawMode;

impl RawMode {
    fn enter() -> Result<Self> {
        crossterm::terminal::enable_raw_mode().context("switching the terminal to raw mode")?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// What the escape handling decided about a chunk of typed bytes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Typed {
    /// Send these bytes to the shell.
    Send(Vec<u8>),
    /// Leave the shell running and disconnect.
    Detach,
    /// End the shell.
    Kill,
}

/// Splits typed bytes into shell input and escape commands. `pending` carries a Ctrl-] seen at
/// the end of the previous chunk.
pub(crate) fn filter_input(pending: &mut bool, bytes: &[u8]) -> Vec<Typed> {
    let mut out = Vec::new();
    let mut plain = Vec::new();
    for &b in bytes {
        if *pending {
            *pending = false;
            match b {
                b'.' => {
                    flush(&mut plain, &mut out);
                    out.push(Typed::Detach);
                    return out;
                }
                b'k' | b'K' => {
                    flush(&mut plain, &mut out);
                    out.push(Typed::Kill);
                    return out;
                }
                // Ctrl-] twice sends one Ctrl-].
                ESCAPE => plain.push(ESCAPE),
                // Anything else: it was not an escape after all.
                other => {
                    plain.push(ESCAPE);
                    plain.push(other);
                }
            }
        } else if b == ESCAPE {
            *pending = true;
        } else {
            plain.push(b);
        }
    }
    flush(&mut plain, &mut out);
    out
}

fn flush(plain: &mut Vec<u8>, out: &mut Vec<Typed>) {
    if !plain.is_empty() {
        out.push(Typed::Send(std::mem::take(plain)));
    }
}

/// Bytes typed on the keyboard, from a thread (reading a terminal blocks).
fn spawn_stdin_reader() -> mpsc::UnboundedReceiver<Vec<u8>> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        })
        .ok();
    rx
}

pub(crate) async fn shell(
    ctx: &AppContext,
    device: &str,
    name: Option<String>,
    program: Option<String>,
    attach_only: bool,
) -> Result<ExitCode> {
    let l = ctx.locale;
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        bail!("`nexus shell` needs an interactive terminal");
    }
    let (client, mut events) = connect_with_events(ctx).await?;
    let devices = client.list_devices().await?.devices;
    let dev = resolve(l, &devices, device)?;
    let opened = client
        .open_session(OpenSession {
            device_id: dev.device_id.clone(),
            want_video: false,
            ..Default::default()
        })
        .await?;
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let started = client
        .open_terminal(OpenTerminal {
            session_id: opened.session_id,
            cols: u32::from(cols),
            rows: u32::from(rows),
            shell: program.unwrap_or_default(),
            cwd: String::new(),
            name: name.unwrap_or_default(),
            reattach_only: attach_only,
        })
        .await;
    let started = match started {
        Ok(s) => s,
        Err(e) => {
            let _ = client.close_session(opened.session_id).await;
            return Err(e.into());
        }
    };
    let id = started.terminal_id;
    eprintln!("{}", tr_args(l, "cli.shell.hint", &[("name", &dev.name)]));

    let raw = RawMode::enter()?;
    let mut typed = spawn_stdin_reader();
    let mut pending_escape = false;
    let mut size = (cols, rows);
    let mut resize_tick = tokio::time::interval(Duration::from_millis(250));
    let mut stdout = std::io::stdout();
    let mut exit = ExitCode::SUCCESS;
    let mut note = String::new();
    loop {
        tokio::select! {
            ev = events.next() => {
                let Some(ev) = ev else {
                    note = tr_args(l, "cli.shell.ended", &[("reason", "the agent closed the connection")]);
                    break;
                };
                match ev.body {
                    Some(Ev::TerminalOutput(o)) if o.terminal_id == id => {
                        let _ = stdout.write_all(&o.data);
                        let _ = stdout.flush();
                    }
                    Some(Ev::TerminalReady(r)) if r.terminal_id == id && r.reattached => {
                        let line = tr_args(l, "cli.shell.reattached", &[("name", &r.name)]);
                        let _ = write!(stdout, "\r\n{line}\r\n");
                        let _ = stdout.flush();
                    }
                    Some(Ev::TerminalEnded(e)) if e.terminal_id == id => {
                        if e.exited {
                            exit = ExitCode::from(u8::try_from(e.exit_code.clamp(0, 255)).unwrap_or(1));
                        } else {
                            exit = ExitCode::FAILURE;
                            note = tr_args(l, "cli.shell.ended", &[("reason", &e.reason)]);
                        }
                        break;
                    }
                    Some(Ev::SessionEnded(e)) if e.session_id == opened.session_id => {
                        exit = ExitCode::FAILURE;
                        note = tr_args(l, "cli.shell.ended", &[("reason", &e.reason)]);
                        break;
                    }
                    _ => {}
                }
            }
            bytes = typed.recv() => {
                let Some(bytes) = bytes else { break };
                let mut finished = false;
                for action in filter_input(&mut pending_escape, &bytes) {
                    match action {
                        Typed::Send(data) => { let _ = client.terminal_input(id, data).await; }
                        Typed::Detach => {
                            let _ = client.close_terminal(id, false).await;
                            note = tr_args(l, "cli.shell.detached", &[]);
                            finished = true;
                        }
                        Typed::Kill => { let _ = client.close_terminal(id, true).await; }
                    }
                }
                if finished { break; }
            }
            _ = resize_tick.tick() => {
                if let Ok(now) = crossterm::terminal::size() && now != size {
                    size = now;
                    let _ = client.terminal_resize(id, u32::from(now.0), u32::from(now.1)).await;
                }
            }
        }
    }
    drop(raw);
    let _ = client.close_session(opened.session_id).await;
    if !note.is_empty() {
        eprintln!("\n{note}");
    }
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_typing_passes_through() {
        let mut p = false;
        assert_eq!(
            filter_input(&mut p, b"ls -l\n"),
            [Typed::Send(b"ls -l\n".to_vec())]
        );
        assert!(!p);
    }

    #[test]
    fn escape_then_dot_detaches_and_k_kills() {
        let mut p = false;
        assert_eq!(
            filter_input(&mut p, &[b'a', ESCAPE, b'.', b'z']),
            [Typed::Send(vec![b'a']), Typed::Detach]
        );
        let mut p = false;
        assert_eq!(filter_input(&mut p, &[ESCAPE, b'k']), [Typed::Kill]);
    }

    #[test]
    fn escape_split_across_reads_and_double_escape() {
        let mut p = false;
        assert!(filter_input(&mut p, &[ESCAPE]).is_empty());
        assert!(p);
        assert_eq!(filter_input(&mut p, b"."), [Typed::Detach]);
        let mut p = false;
        assert_eq!(
            filter_input(&mut p, &[ESCAPE, ESCAPE]),
            [Typed::Send(vec![ESCAPE])]
        );
        let mut p = false;
        assert_eq!(
            filter_input(&mut p, &[ESCAPE, b'x']),
            [Typed::Send(vec![ESCAPE, b'x'])]
        );
    }
}
