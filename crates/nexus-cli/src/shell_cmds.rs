//! `nexus shell`: a terminal on a paired device.

use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
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

/// Shared options for `nexus shell` and `nexus remote terminal`.
#[derive(Debug, Args)]
pub struct TerminalOptions {
    /// Paired device name (quote names containing spaces) or ID.
    pub device: String,
    /// Persistent session name. Ctrl-] . detaches; the same name takes it over later.
    #[arg(short, long, conflicts_with = "command")]
    pub name: Option<String>,
    /// Allowed program to run instead of the remote default shell.
    #[arg(long)]
    pub program: Option<String>,
    /// Only take over an existing named terminal.
    #[arg(long, requires = "name")]
    pub attach_only: bool,
    /// Starting directory on the remote computer.
    #[arg(long)]
    pub cwd: Option<String>,
    /// Run a command in the remote shell and return its exit code (PTY output is merged).
    #[arg(short = 'c', long, conflicts_with = "args")]
    pub command: Option<String>,
    /// Stream bytes without local raw mode or escape shortcuts (automatic for pipes).
    #[arg(long)]
    pub no_tty: bool,
    /// End the attachment after this many seconds (1–604800); named terminals keep running.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=604800))]
    pub timeout: Option<u64>,
    /// Arguments for the remote shell, after `--`; never expanded by a local shell.
    #[arg(last = true)]
    pub args: Vec<String>,
}

impl TerminalOptions {
    fn shell_args(&self) -> Vec<String> {
        match &self.command {
            Some(command) => {
                // Both Unix shells and PowerShell accept -c. Explicit cmd.exe uses /C.
                let cmd = self.program.as_deref().is_some_and(|p| {
                    p.rsplit(['/', '\\']).next().is_some_and(|p| {
                        p.eq_ignore_ascii_case("cmd.exe") || p.eq_ignore_ascii_case("cmd")
                    })
                });
                vec![if cmd { "/C" } else { "-c" }.into(), command.clone()]
            }
            None => self.args.clone(),
        }
    }
}

/// Bounded so a fast pipe cannot fill memory while the remote shell is slow.
fn spawn_stdin_reader() -> Result<mpsc::Receiver<std::io::Result<Vec<u8>>>> {
    let (tx, rx) = mpsc::channel(16);
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.blocking_send(Ok(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.blocking_send(Err(e));
                        break;
                    }
                }
            }
        })
        .context("reading terminal input")?;
    Ok(rx)
}

// A short command can exit while stdin/EOF is in flight. The agent removes the
// terminal before queuing TerminalEnded, so NotFound here means drain that event
// rather than replacing the real command result with an input error.
async fn send_input(client: &nexus_ipc::IpcClient, id: u64, data: Vec<u8>) -> Result<bool> {
    match client.terminal_input(id, data).await {
        Ok(()) => Ok(true),
        Err(nexus_ipc::IpcError::Agent {
            code: nexus_proto::v1::IpcErrorCode::NotFound,
            ..
        }) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) async fn shell(ctx: &AppContext, options: &TerminalOptions) -> Result<ExitCode> {
    let l = ctx.locale;
    let interactive = std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && !options.no_tty
        && options.command.is_none();
    let (client, mut events) = connect_with_events(ctx).await?;
    let devices = client.list_devices().await?.devices;
    let dev = resolve(l, &devices, &options.device)?;
    let opened = client
        .open_session(OpenSession {
            device_id: dev.device_id.clone(),
            want_video: false,
            ..Default::default()
        })
        .await?;
    let (cols, rows) = if interactive {
        crossterm::terminal::size().unwrap_or((80, 24))
    } else {
        (80, 24)
    };
    let started = client
        .open_terminal(OpenTerminal {
            session_id: opened.session_id,
            cols: u32::from(cols),
            rows: u32::from(rows),
            args: options.shell_args(),
            shell: options.program.clone().unwrap_or_default(),
            cwd: options.cwd.clone().unwrap_or_default(),
            name: options.name.clone().unwrap_or_default(),
            reattach_only: options.attach_only,
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
    // Keep cleanup outside the pump: raw-mode, IO, timeout and transport errors all close
    // the attachment, leaving only deliberately named shells alive.
    let result: Result<ExitCode> = async {
        let _raw = if interactive {
            eprintln!("{}", tr_args(l, "cli.shell.hint", &[("name", &dev.name)]));
            Some(RawMode::enter()?)
        } else { None };
        let read_input = options.command.is_none() || !std::io::stdin().is_terminal();
        let mut typed = if read_input { spawn_stdin_reader()? } else {
            let (_, rx) = mpsc::channel(1); rx
        };
        let mut stdin_done = !read_input;
        let mut pending_escape = false;
        let mut size = (cols, rows);
        let mut resize_tick = tokio::time::interval(Duration::from_millis(250));
        let mut stdout = std::io::stdout();
        let deadline = async {
            match options.timeout {
                Some(seconds) => tokio::time::sleep(Duration::from_secs(seconds)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                ev = events.next() => {
                    let Some(ev) = ev else { bail!("{}", tr_args(l, "cli.shell.ended", &[("reason", "the agent closed the connection")])); };
                    match ev.body {
                        Some(Ev::TerminalOutput(o)) if o.terminal_id == id => {
                            stdout.write_all(&o.data).context("writing remote terminal output")?;
                            stdout.flush()?;
                        }
                        Some(Ev::TerminalReady(r)) if r.terminal_id == id && r.reattached && interactive => {
                            eprintln!("{}", tr_args(l, "cli.shell.reattached", &[("name", &r.name)]));
                        }
                        Some(Ev::TerminalEnded(e)) if e.terminal_id == id => {
                            if !e.exited { bail!("{}", tr_args(l, "cli.shell.ended", &[("reason", &e.reason)])); }
                            break Ok(ExitCode::from(u8::try_from(e.exit_code.min(255)).unwrap_or(1)));
                        }
                        Some(Ev::SessionEnded(e)) if e.session_id == opened.session_id => {
                            bail!("{}", tr_args(l, "cli.shell.ended", &[("reason", &e.reason)]));
                        }
                        _ => {}
                    }
                }
                bytes = typed.recv(), if !stdin_done => {
                    let Some(bytes) = bytes else {
                        stdin_done = true;
                        // A PTY has no half-close. In canonical Unix mode, the first EOF
                        // delivers a partial line and the second ends input. Keep draining
                        // output until TerminalEnded, so piped commands are not truncated.
                        let windows = dev.os.as_ref().is_some_and(|os| os.family == nexus_proto::v1::OsFamily::Windows as i32);
                        let eof = if windows { b"\x1a\r".to_vec() } else { b"\x04\x04".to_vec() };
                        let _ = send_input(&client, id, eof).await?;
                        continue;
                    };
                    let bytes = bytes.context("reading terminal input")?;
                    let actions = if interactive { filter_input(&mut pending_escape, &bytes) }
                        else { vec![Typed::Send(bytes)] };
                    for action in actions {
                        match action {
                            Typed::Send(data) => { if !send_input(&client, id, data).await? { stdin_done = true; break; } }
                            Typed::Detach => {
                                client.close_terminal(id, false).await?;
                                eprintln!("{}", tr_args(l, "cli.shell.detached", &[]));
                                return Ok(ExitCode::SUCCESS);
                            }
                            Typed::Kill => client.close_terminal(id, true).await?,
                        }
                    }
                }
                _ = resize_tick.tick(), if interactive => {
                    if let Ok(now) = crossterm::terminal::size() && now != size {
                        size = now;
                        client.terminal_resize(id, u32::from(now.0), u32::from(now.1)).await?;
                    }
                }
                signal = tokio::signal::ctrl_c(), if !interactive => {
                    signal?;
                    return Ok(ExitCode::from(130));
                }
                _ = &mut deadline => {
                    bail!("{}", nexus_core::i18n::tr(l, "cli.shell.timeout"));
                }
            }
        }
    }.await;
    let _ = client.close_terminal(id, false).await;
    let _ = client.close_session(opened.session_id).await;
    result
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
