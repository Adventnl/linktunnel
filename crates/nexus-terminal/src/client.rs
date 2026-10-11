//! The controller side of a terminal.

use bytes::Bytes;
use nexus_proto::v1::{
    TermClose, TermData, TermOpen, TermResize, TerminalMessage, terminal_message::Body,
};
use tokio::sync::mpsc;

use crate::TermError;
use crate::io::TermIo;

/// What the user's side asks for.
#[derive(Debug)]
pub enum TermCmd {
    /// Keystrokes.
    Input(Bytes),
    /// The window changed size.
    Resize {
        /// New width in columns.
        cols: u32,
        /// New height in rows.
        rows: u32,
    },
    /// Kill the shell (even a persistent one).
    Kill,
    /// Leave; a persistent shell keeps running.
    Detach,
}

/// What comes back.
#[derive(Debug, PartialEq, Eq)]
pub enum TermEvent {
    /// The terminal is open.
    Opened {
        /// Persistent name, if any.
        name: String,
        /// True if this attached to a shell that was already running.
        reattached: bool,
        /// Program that runs.
        shell: String,
    },
    /// Output.
    Output(Bytes),
    /// The shell ended.
    Exited(i32),
    /// The target refused or the stream failed.
    Failed(String),
}

fn msg(body: Body) -> TerminalMessage {
    TerminalMessage { body: Some(body) }
}

/// Opens a terminal on `io` and pumps it until it ends. Commands arrive on `cmds`, events leave
/// on `events`; the function returns when the terminal is over or either channel closes.
pub async fn run_client<I: TermIo>(
    mut io: I,
    open: TermOpen,
    mut cmds: mpsc::UnboundedReceiver<TermCmd>,
    events: mpsc::Sender<TermEvent>,
) -> Result<(), TermError> {
    io.send(msg(Body::Open(open))).await?;
    loop {
        tokio::select! {
            incoming = io.recv() => {
                let Some(m) = incoming? else {
                    let _ = events.send(TermEvent::Failed("the device closed the terminal".into())).await;
                    return Ok(());
                };
                let event = match m.body {
                    Some(Body::Opened(o)) => TermEvent::Opened { name: o.name, reattached: o.reattached, shell: o.shell },
                    Some(Body::Data(d)) => TermEvent::Output(d.data),
                    Some(Body::Exit(e)) => {
                        let _ = events.send(TermEvent::Exited(e.code)).await;
                        return Ok(());
                    }
                    Some(Body::Error(e)) => {
                        let _ = events.send(TermEvent::Failed(e.message)).await;
                        return Ok(());
                    }
                    _ => continue,
                };
                if events.send(event).await.is_err() {
                    return Ok(());
                }
            }
            cmd = cmds.recv() => match cmd {
                Some(TermCmd::Input(data)) => io.send(msg(Body::Data(TermData { data }))).await?,
                Some(TermCmd::Resize { cols, rows }) => io.send(msg(Body::Resize(TermResize { cols, rows }))).await?,
                Some(TermCmd::Kill) => io.send(msg(Body::Close(TermClose {}))).await?,
                Some(TermCmd::Detach) | None => return Ok(()),
            },
        }
    }
}
