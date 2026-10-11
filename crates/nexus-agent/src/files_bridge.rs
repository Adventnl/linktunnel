//! File operations on behalf of local clients, over a session they opened (a control-only
//! session is enough).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nexus_files::{FileError, Progress};
use nexus_proto::v1::{
    DirListing, FileEntry, FileErrorCode, FileList, FileMkdir, FileRemove, FileStatRequest,
    FileTransfer, IpcEvent, TransferFinished, TransferProgress, TransferStarted,
    ipc_event::Body as Ev,
};

use crate::error::AgentError;
use crate::state::Shared;
use crate::viewer_bridge::{ConnCtx, controller_for};

static NEXT_TRANSFER: AtomicU64 = AtomicU64::new(1);
/// Progress events are throttled to this interval.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

impl From<FileError> for AgentError {
    fn from(e: FileError) -> Self {
        match e {
            FileError::NotFound(_) => Self::NotFound(e.to_string()),
            FileError::PermissionDenied(_) => Self::Denied(e.to_string()),
            FileError::InvalidPath(_) => Self::Invalid(e.to_string()),
            FileError::Busy(_) => Self::Busy(e.to_string()),
            other => Self::Failed(other.to_string()),
        }
    }
}

pub(crate) async fn list(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    r: FileList,
) -> Result<DirListing, AgentError> {
    let c = controller_for(shared, ctx, r.session_id)?;
    Ok(c.files().list(&r.path).await?)
}

pub(crate) async fn stat(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    r: FileStatRequest,
) -> Result<FileEntry, AgentError> {
    let c = controller_for(shared, ctx, r.session_id)?;
    Ok(c.files().stat(&r.path).await?)
}

pub(crate) async fn mkdir(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    r: FileMkdir,
) -> Result<(), AgentError> {
    let c = controller_for(shared, ctx, r.session_id)?;
    Ok(c.files().mkdir(&r.path).await?)
}

pub(crate) async fn remove(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    r: FileRemove,
) -> Result<(), AgentError> {
    let c = controller_for(shared, ctx, r.session_id)?;
    Ok(c.files().remove(&r.path, r.recursive).await?)
}

/// Starts a copy in the background; progress and the result arrive as events on the connection
/// that asked.
pub(crate) fn transfer(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    r: FileTransfer,
) -> Result<TransferStarted, AgentError> {
    let c = controller_for(shared, ctx, r.session_id)?;
    let local = PathBuf::from(&r.local_path);
    if !local.is_absolute() {
        return Err(AgentError::Invalid(
            "the local path must be absolute".into(),
        ));
    }
    let id = NEXT_TRANSFER.fetch_add(1, Ordering::Relaxed);
    let events = ctx.events.clone();
    tokio::spawn(async move {
        let files = c.files();
        let progress_tx = events.clone();
        let mut last = Instant::now()
            .checked_sub(PROGRESS_EVERY)
            .unwrap_or_else(Instant::now);
        let on_progress = move |p: Progress| {
            if p.done < p.total && last.elapsed() < PROGRESS_EVERY {
                return;
            }
            last = Instant::now();
            let _ = progress_tx.try_send(IpcEvent {
                body: Some(Ev::TransferProgress(TransferProgress {
                    transfer_id: id,
                    done: p.done,
                    total: p.total,
                })),
            });
        };
        let run = async {
            if r.upload {
                files
                    .upload(&local, &r.remote_path, r.overwrite, on_progress)
                    .await
            } else {
                files
                    .download(&r.remote_path, &local, r.overwrite, on_progress)
                    .await
            }
        };
        let result = tokio::select! {
            res = run => res,
            () = events.closed() => return,
        };
        let finished = match result {
            Ok(t) => TransferFinished {
                transfer_id: id,
                ok: true,
                error_code: FileErrorCode::Unspecified as i32,
                error_message: String::new(),
                size: t.size,
                blake3: t.blake3.to_vec().into(),
                resumed_from: t.resumed_from,
            },
            Err(e) => TransferFinished {
                transfer_id: id,
                ok: false,
                error_code: e.code() as i32,
                error_message: e.to_string(),
                ..TransferFinished::default()
            },
        };
        let _ = events
            .send(IpcEvent {
                body: Some(Ev::TransferFinished(finished)),
            })
            .await;
    });
    let _ = shared;
    Ok(TransferStarted { transfer_id: id })
}
