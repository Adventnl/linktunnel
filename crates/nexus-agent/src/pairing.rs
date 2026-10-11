//! Drives a pairing from the IPC request to the stored, registered pairing.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use nexus_crypto::noise::PairingCode;
use nexus_proto::v1::{PairingFinished, PairingSasReady, ipc_event::Body as Ev};
use nexus_trust::flow::{AbortReason, FlowEvent, PairingFlow, Proposal, ResponderPolicy};
use nexus_trust::grants;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::error::AgentError;
use crate::state::{PairingSlot, RendezvousMsg, Shared};

/// A pairing code is good for this long, start to finish.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(300);

/// Which side of the exchange the local device is.
enum Side {
    Offer {
        allow_peer_to: Vec<String>,
    },
    Join {
        request_on_peer: Vec<String>,
        grant_to_peer: Vec<String>,
    },
}

fn validate_grants(list: &[String]) -> Result<(), AgentError> {
    if list.len() > 32 || !list.iter().all(|g| grants::is_valid(g)) {
        return Err(AgentError::Invalid("invalid grant list".into()));
    }
    Ok(())
}

/// Shows a code and waits for another device to enter it. Returns `(pairing_id, code)`.
pub(crate) async fn offer(
    shared: &Arc<Shared>,
    allow_peer_to: Vec<String>,
) -> Result<(u64, String), AgentError> {
    validate_grants(&allow_peer_to)?;
    let code = PairingCode::generate().map_err(|e| AgentError::Failed(e.to_string()))?;
    let text = code.to_string();
    let id = start(shared, code, Side::Offer { allow_peer_to }).await?;
    Ok((id, text))
}

/// Enters a code shown by another device. Returns the pairing id.
pub(crate) async fn join(
    shared: &Arc<Shared>,
    code: &str,
    request_on_peer: Vec<String>,
    grant_to_peer: Vec<String>,
) -> Result<u64, AgentError> {
    validate_grants(&request_on_peer)?;
    validate_grants(&grant_to_peer)?;
    let code = PairingCode::parse(code).map_err(|e| AgentError::Invalid(e.to_string()))?;
    start(
        shared,
        code,
        Side::Join {
            request_on_peer,
            grant_to_peer,
        },
    )
    .await
}

async fn start(shared: &Arc<Shared>, code: PairingCode, side: Side) -> Result<u64, AgentError> {
    if shared.is_locked_down() {
        return Err(AgentError::Denied("lockdown is on".into()));
    }
    let signal = shared.signal().ok_or_else(|| {
        AgentError::Unavailable("pairing needs the signaling server, which is not connected".into())
    })?;

    let id = shared.next_pairing_id.fetch_add(1, Ordering::SeqCst);
    let (confirm_tx, confirm_rx) = mpsc::channel(1);
    let (rdv_tx, rdv_rx) = mpsc::channel(64);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    {
        let mut slot = shared.pairing.lock();
        if slot.is_some() {
            return Err(AgentError::Busy("a pairing is already in progress".into()));
        }
        *slot = Some(PairingSlot {
            id,
            confirm: confirm_tx,
            rendezvous: rdv_tx,
            cancel: cancel_tx,
        });
    }

    let flow = match &side {
        Side::Offer { allow_peer_to } => PairingFlow::responder(
            Arc::clone(&shared.identity),
            &shared.cfg.device_name,
            &code,
            ResponderPolicy {
                max_initiator_on_me: allow_peer_to.clone(),
            },
        ),
        Side::Join {
            request_on_peer,
            grant_to_peer,
        } => PairingFlow::initiator(
            Arc::clone(&shared.identity),
            &shared.cfg.device_name,
            &code,
            Proposal {
                initiator_on_responder: request_on_peer.clone(),
                responder_on_initiator: grant_to_peer.clone(),
            },
        ),
    };
    let flow = match flow {
        Ok(f) => f,
        Err(e) => {
            shared.pairing.lock().take();
            return Err(AgentError::Failed(e.to_string()));
        }
    };

    if let Err(e) = signal.rendezvous_join(code.rendezvous_id()).await {
        shared.pairing.lock().take();
        return Err(AgentError::from(e));
    }

    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        let outcome = tokio::time::timeout(
            PAIRING_TIMEOUT,
            drive(&shared, id, flow, rdv_rx, confirm_rx, cancel_rx),
        )
        .await
        .unwrap_or_else(|_| Outcome::failed("the pairing code expired", None));
        // Leave the room and free the slot *before* announcing the result, so a client that
        // reacts to `PairingFinished` by starting another pairing is never told "busy".
        if let Some(signal) = shared.signal() {
            let _ = signal.rendezvous_leave().await;
        }
        shared.pairing.lock().take();
        finished(&shared, id, outcome);
    });
    Ok(id)
}

/// How a pairing ended.
struct Outcome {
    success: bool,
    reason: String,
    peer: Option<(Vec<u8>, String)>,
}

impl Outcome {
    fn failed(reason: impl Into<String>, peer: Option<(Vec<u8>, String)>) -> Self {
        Self {
            success: false,
            reason: reason.into(),
            peer,
        }
    }
}

fn finished(shared: &Shared, id: u64, outcome: Outcome) {
    let (peer_device_id, peer_name) = outcome.peer.unwrap_or_default();
    shared.emit(Ev::PairingFinished(PairingFinished {
        pairing_id: id,
        success: outcome.success,
        reason: outcome.reason,
        peer_device_id: peer_device_id.into(),
        peer_name,
    }));
}

async fn drive(
    shared: &Arc<Shared>,
    id: u64,
    mut flow: PairingFlow,
    mut rendezvous: mpsc::Receiver<RendezvousMsg>,
    mut confirm: mpsc::Receiver<bool>,
    mut cancel: watch::Receiver<bool>,
) -> Outcome {
    // The initiator must not send before the other device is in the room.
    let mut peer_present = false;
    let mut outbox: Vec<Vec<u8>> = Vec::new();
    let mut peer_info: Option<(Vec<u8>, String)> = None;

    loop {
        // Flush what the flow produced, once the other side is there.
        outbox.extend(flow.take_outgoing());
        if peer_present
            && !outbox.is_empty()
            && let Some(signal) = shared.signal()
        {
            for msg in outbox.drain(..) {
                if let Err(e) = signal.rendezvous_send(msg).await {
                    debug!(error = %e, "sending a pairing message failed");
                }
            }
        }
        while let Some(event) = flow.next_event() {
            match event {
                FlowEvent::SasReady {
                    sas,
                    peer_name,
                    peer,
                } => {
                    peer_info = Some((peer.device_id().as_bytes().to_vec(), peer_name.clone()));
                    shared.emit(Ev::PairingSas(PairingSasReady {
                        pairing_id: id,
                        words: sas.words().iter().map(ToString::to_string).collect(),
                        digits: sas.digits(),
                        peer_name: sanitize(&peer_name),
                        peer_device_id: peer.device_id().as_bytes().to_vec().into(),
                        peer_fingerprint: peer.fingerprint().to_string(),
                    }));
                }
                FlowEvent::Completed(pairing) => {
                    let outcome = store_pairing(shared, &pairing.signed).await;
                    return match outcome {
                        Ok(()) => {
                            info!("pairing complete");
                            Outcome {
                                success: true,
                                reason: String::new(),
                                peer: peer_info,
                            }
                        }
                        Err(e) => Outcome::failed(e.to_string(), peer_info),
                    };
                }
                FlowEvent::Aborted(reason) => return Outcome::failed(describe(&reason), peer_info),
            }
        }
        if flow.is_finished() {
            return Outcome::failed("the pairing ended unexpectedly", peer_info);
        }

        tokio::select! {
            msg = rendezvous.recv() => match msg {
                None => return Outcome::failed("the connection to the server ended", peer_info),
                Some(RendezvousMsg::Peer(true)) => peer_present = true,
                Some(RendezvousMsg::Peer(false)) => {
                    return Outcome::failed("the other device left", peer_info);
                }
                Some(RendezvousMsg::Data(data)) => {
                    if let Err(e) = flow.handle_incoming(&data, Shared::now_ms()) {
                        debug!(error = %e, "pairing flow stopped");
                    }
                }
            },
            decision = confirm.recv() => match decision {
                Some(words_match) => {
                    if let Err(e) = flow.confirm_sas(words_match, Shared::now_ms()) {
                        debug!(error = %e, "confirmation rejected");
                    }
                }
                None => return Outcome::failed("cancelled", peer_info),
            },
            _ = cancel.changed() => return Outcome::failed("cancelled", peer_info),
        }
    }
}

/// Peer names are untrusted text shown to the user: strip control characters and cap the length.
fn sanitize(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(64).collect()
}

fn describe(reason: &AbortReason) -> String {
    match reason {
        AbortReason::LocalDecline => "you reported that the words did not match".into(),
        AbortReason::PeerDeclined => {
            "the other device reported that the words did not match".into()
        }
        AbortReason::Handshake => {
            "the code did not match (or the exchange was tampered with)".into()
        }
        AbortReason::Protocol(m) => format!("protocol error: {m}"),
        AbortReason::PolicyViolation(m) => format!("refused by policy: {m}"),
        AbortReason::Mismatch(m) => format!("inconsistent proposal: {m}"),
    }
}

/// Persists a completed pairing, opens the transport to the new peer, and tells the server.
async fn store_pairing(
    shared: &Arc<Shared>,
    signed: &nexus_proto::v1::SignedPairingRecord,
) -> Result<(), AgentError> {
    let key = shared
        .trust
        .lock()
        .add(signed)
        .map(|p| p.identity.ed25519_bytes())?;
    shared.refresh_allowlist();
    shared.emit_device_changed(&key);
    // Either party can register the (identical) record; the server treats a repeat as a no-op.
    if let Some(signal) = shared.signal() {
        if let Err(e) = signal.register_pairing(signed.clone()).await
            && e.server_code() != Some(nexus_proto::v1::SignalErrorCode::Conflict)
        {
            warn!(error = %e, "could not register the pairing with the server yet; will retry on reconnect");
        } else if let Ok(snapshot) = signal.subscribe_presence().await {
            for p in snapshot {
                crate::supervisor::apply_presence_pub(shared, &p);
            }
        }
    }
    Ok(())
}

/// Cancels the pairing `id`, if it is the one in progress.
pub(crate) fn cancel(shared: &Shared, id: u64) -> Result<(), AgentError> {
    let slot = shared.pairing.lock();
    match slot.as_ref() {
        Some(s) if s.id == id => {
            let _ = s.cancel.send(true);
            Ok(())
        }
        _ => Err(AgentError::NotFound("no such pairing in progress".into())),
    }
}

/// Delivers the local user's SAS decision.
pub(crate) async fn confirm(shared: &Shared, id: u64, words_match: bool) -> Result<(), AgentError> {
    let tx = {
        let slot = shared.pairing.lock();
        match slot.as_ref() {
            Some(s) if s.id == id => s.confirm.clone(),
            _ => return Err(AgentError::NotFound("no such pairing in progress".into())),
        }
    };
    tx.send(words_match)
        .await
        .map_err(|_| AgentError::NotFound("the pairing already ended".into()))
}
