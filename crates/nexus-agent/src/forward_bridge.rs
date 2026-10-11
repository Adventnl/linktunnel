//! Local port forwards on behalf of clients (`nexus forward`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nexus_proto::v1::{ForwardStarted, StartForward, StopForward};
use tokio::net::TcpListener;
use tracing::info;

use crate::error::AgentError;
use crate::state::Shared;
use crate::viewer_bridge::{ConnCtx, controller_for};

static NEXT_FORWARD: AtomicU64 = AtomicU64::new(1);

/// A listening forward.
pub(crate) struct ForwardEntry {
    task: tokio::task::JoinHandle<()>,
    owner: Arc<ConnCtx>,
}

pub(crate) async fn start(
    shared: &Arc<Shared>,
    ctx: &Arc<ConnCtx>,
    req: StartForward,
) -> Result<ForwardStarted, AgentError> {
    let controller = controller_for(shared, ctx, req.session_id)?;
    let port = u16::try_from(req.remote_port)
        .ok()
        .filter(|p| *p != 0)
        .ok_or_else(|| AgentError::Invalid("remote port out of range".into()))?;
    let local_port = u16::try_from(req.local_port)
        .map_err(|_| AgentError::Invalid("local port out of range".into()))?;
    if req.remote_host.is_empty() || req.remote_host.len() > 255 {
        return Err(AgentError::Invalid(
            "remote host must be 1-255 bytes".into(),
        ));
    }
    // Loopback only: a forward must not turn this machine into an open relay for its LAN.
    let listener = TcpListener::bind(("127.0.0.1", local_port))
        .await
        .map_err(|e| AgentError::Failed(format!("cannot listen on 127.0.0.1:{local_port}: {e}")))?;
    let bound = listener
        .local_addr()
        .map_err(|e| AgentError::Failed(e.to_string()))?
        .port();
    let id = NEXT_FORWARD.fetch_add(1, Ordering::Relaxed);
    let host = req.remote_host.clone();
    let open = move || {
        let controller = controller.clone();
        async move { controller.forward_stream().await.map_err(|e| e.to_string()) }
    };
    let task = tokio::spawn(nexus_portfwd::listen_and_forward(
        listener,
        host.clone(),
        port,
        open,
    ));
    shared.forwards.lock().insert(
        id,
        ForwardEntry {
            task,
            owner: Arc::clone(ctx),
        },
    );
    info!(forward = id, local = bound, remote = %format!("{host}:{port}"), "port forward started");
    Ok(ForwardStarted {
        forward_id: id,
        local_port: u32::from(bound),
    })
}

pub(crate) fn stop(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    req: &StopForward,
) -> Result<(), AgentError> {
    let mut forwards = shared.forwards.lock();
    match forwards.get(&req.forward_id) {
        None => return Err(AgentError::NotFound("no such forward".into())),
        Some(e) if !Arc::ptr_eq(&e.owner, ctx) => {
            return Err(AgentError::Denied(
                "that forward belongs to another client".into(),
            ));
        }
        Some(_) => {}
    }
    if let Some(e) = forwards.remove(&req.forward_id) {
        e.task.abort();
    }
    Ok(())
}

/// Stops every forward a disconnected client had.
pub(crate) fn close_all(shared: &Shared, ctx: &Arc<ConnCtx>) {
    shared.forwards.lock().retain(|_, e| {
        let mine = Arc::ptr_eq(&e.owner, ctx);
        if mine {
            e.task.abort();
        }
        !mine
    });
}
