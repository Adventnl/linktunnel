//! The target side of one attached terminal.

use std::sync::Arc;

use nexus_proto::v1::{
    TermData, TermError as WireError, TermExit, TermOpened, TerminalMessage, terminal_message::Body,
};
use tracing::debug;

use crate::TermError;
use crate::host::{Out, TerminalHost};
use crate::io::TermIo;

fn msg(body: Body) -> TerminalMessage {
    TerminalMessage { body: Some(body) }
}

async fn refuse<I: TermIo>(io: &mut I, why: &TermError) {
    let _ = io
        .send(msg(Body::Error(WireError {
            message: why.to_string(),
        })))
        .await;
}

/// Serves one terminal: waits for `TermOpen`, attaches, then pumps data until the shell exits,
/// the controller closes it, or the stream ends.
pub async fn serve<I: TermIo>(host: &Arc<TerminalHost>, io: &mut I) -> Result<(), TermError> {
    let Some(first) = io.recv().await? else {
        return Ok(());
    };
    let Some(Body::Open(open)) = first.body else {
        let e = TermError::Stream("expected TermOpen first".into());
        refuse(io, &e).await;
        return Err(e);
    };
    let (session, mut attachment) = match host.open(&open) {
        Ok(ok) => ok,
        Err(e) => {
            refuse(io, &e).await;
            return Err(e);
        }
    };
    let attach_id = attachment.id;
    io.send(msg(Body::Opened(TermOpened {
        name: open.name.clone(),
        reattached: attachment.reattached,
        shell: session.shell.clone(),
    })))
    .await?;

    let result = loop {
        tokio::select! {
            incoming = io.recv() => match incoming {
                Ok(Some(m)) => match m.body {
                    Some(Body::Data(d)) => {
                        let s = Arc::clone(&session);
                        let written = tokio::task::spawn_blocking(move || s.write(&d.data)).await;
                        if !matches!(written, Ok(Ok(()))) {
                            debug!("terminal write failed; the shell has probably exited");
                        }
                    }
                    Some(Body::Resize(r)) => session.resize(
                        u16::try_from(r.cols).unwrap_or(80),
                        u16::try_from(r.rows).unwrap_or(24),
                    ),
                    Some(Body::Close(_)) => {
                        session.kill();
                    }
                    _ => {}
                },
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            },
            out = attachment.output.recv() => match out {
                Some(Out::Data(bytes)) => {
                    if let Err(e) = io.send(msg(Body::Data(TermData { data: bytes }))).await {
                        break Err(e);
                    }
                }
                Some(Out::Exit(code)) => {
                    let _ = io.send(msg(Body::Exit(TermExit { code }))).await;
                    break Ok(());
                }
                Some(Out::TakenOver) => {
                    refuse(io, &TermError::Stream("this terminal was taken over by another controller".into())).await;
                    break Ok(());
                }
                None => break Ok(()),
            },
        }
    };
    host.detached(&session, attach_id);
    result
}
