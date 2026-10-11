//! Terminals on behalf of local clients (`nexus shell`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nexus_proto::v1::{
    CloseTerminal, IpcEvent, OpenTerminal, TermOpen, TerminalEnded, TerminalInput, TerminalOutput,
    TerminalReady, TerminalResize, TerminalStarted, ipc_event::Body as Ev,
};
use nexus_terminal::{TermCmd, TermEvent};
use tokio::sync::mpsc;

use crate::error::AgentError;
use crate::state::Shared;
use crate::viewer_bridge::{ConnCtx, controller_for};

static NEXT_TERMINAL: AtomicU64 = AtomicU64::new(1);
/// Largest input chunk accepted from a client in one request.
const MAX_INPUT: usize = 256 * 1024;

/// A terminal a client has open.
pub(crate) struct TerminalEntry {
    cmds: mpsc::UnboundedSender<TermCmd>,
    owner: Arc<ConnCtx>,
}

pub(crate) async fn open(
    shared: &Arc<Shared>,
    ctx: &Arc<ConnCtx>,
    req: OpenTerminal,
) -> Result<TerminalStarted, AgentError> {
    let controller = controller_for(shared, ctx, req.session_id)?;
    let (cmds, mut events) = controller
        .terminal(TermOpen {
            cols: req.cols.clamp(1, 1000),
            rows: req.rows.clamp(1, 1000),
            shell: req.shell,
            args: Vec::new(),
            cwd: req.cwd,
            name: req.name,
            reattach_only: req.reattach_only,
        })
        .await?;
    let id = NEXT_TERMINAL.fetch_add(1, Ordering::Relaxed);
    shared.terminals.lock().insert(
        id,
        TerminalEntry {
            cmds,
            owner: Arc::clone(ctx),
        },
    );
    let (shared, out) = (Arc::clone(shared), ctx.events.clone());
    tokio::spawn(async move {
        let ended = loop {
            let Some(ev) = events.recv().await else {
                break ended(id, false, 0, "the terminal channel closed");
            };
            let body = match ev {
                TermEvent::Opened {
                    name,
                    reattached,
                    shell,
                } => Ev::TerminalReady(TerminalReady {
                    terminal_id: id,
                    name,
                    reattached,
                    shell,
                }),
                TermEvent::Output(data) => Ev::TerminalOutput(TerminalOutput {
                    terminal_id: id,
                    data,
                }),
                TermEvent::Exited(code) => break ended(id, true, code, ""),
                TermEvent::Failed(why) => break ended(id, false, 0, &why),
            };
            // Blocking send is the back-pressure path: a slow client slows the shell's output.
            if out.send(IpcEvent { body: Some(body) }).await.is_err() {
                break ended(id, false, 0, "client disconnected");
            }
        };
        shared.terminals.lock().remove(&id);
        let _ = out.send(IpcEvent { body: Some(ended) }).await;
    });
    Ok(TerminalStarted { terminal_id: id })
}

fn ended(id: u64, exited: bool, code: i32, reason: &str) -> Ev {
    Ev::TerminalEnded(TerminalEnded {
        terminal_id: id,
        exited,
        exit_code: code,
        reason: reason.to_owned(),
    })
}

fn cmds_for(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    id: u64,
) -> Result<mpsc::UnboundedSender<TermCmd>, AgentError> {
    let terminals = shared.terminals.lock();
    let entry = terminals
        .get(&id)
        .ok_or_else(|| AgentError::NotFound("no such terminal".into()))?;
    if !Arc::ptr_eq(&entry.owner, ctx) {
        return Err(AgentError::Denied(
            "that terminal belongs to another client".into(),
        ));
    }
    Ok(entry.cmds.clone())
}

pub(crate) fn input(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    req: TerminalInput,
) -> Result<(), AgentError> {
    if req.data.len() > MAX_INPUT {
        return Err(AgentError::Invalid("input chunk too large".into()));
    }
    cmds_for(shared, ctx, req.terminal_id)?
        .send(TermCmd::Input(req.data))
        .map_err(|_| AgentError::NotFound("the terminal has ended".into()))
}

pub(crate) fn resize(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    req: &TerminalResize,
) -> Result<(), AgentError> {
    cmds_for(shared, ctx, req.terminal_id)?
        .send(TermCmd::Resize {
            cols: req.cols.clamp(1, 1000),
            rows: req.rows.clamp(1, 1000),
        })
        .map_err(|_| AgentError::NotFound("the terminal has ended".into()))
}

pub(crate) fn close(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    req: &CloseTerminal,
) -> Result<(), AgentError> {
    let cmds = cmds_for(shared, ctx, req.terminal_id)?;
    let _ = cmds.send(if req.kill {
        TermCmd::Kill
    } else {
        TermCmd::Detach
    });
    Ok(())
}

/// Ends every terminal a disconnected client had open (persistent shells keep running on the
/// target).
pub(crate) fn close_all(shared: &Shared, ctx: &Arc<ConnCtx>) {
    let mut terminals = shared.terminals.lock();
    terminals.retain(|_, entry| {
        let mine = Arc::ptr_eq(&entry.owner, ctx);
        if mine {
            let _ = entry.cmds.send(TermCmd::Detach);
        }
        !mine
    });
}
