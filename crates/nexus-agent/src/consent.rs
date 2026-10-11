//! Attended access: ask the person at this device before another device uses it.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use nexus_core::config::ConsentMode;
use nexus_media::target::{Consent, ConsentGate};
use nexus_proto::v1::{ConsentDecision, ConsentRequest, ConsentResolved, ipc_event::Body as Ev};
use nexus_trust::PairedPeer;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::error::AgentError;
use crate::state::Shared;

/// A prompt waiting for an answer.
#[derive(Debug)]
pub(crate) struct Pending {
    answer: oneshot::Sender<ConsentDecision>,
    peer: [u8; 32],
}

/// The gate for a session from `peer`: `None` when that device is pre-authorised.
pub(crate) fn gate(
    shared: &Arc<Shared>,
    peer: &PairedPeer,
    path: nexus_proto::v1::PathKind,
) -> Option<ConsentGate> {
    let key = peer.identity.ed25519_bytes();
    if shared.policy.lock().is_unattended(&key) {
        return None;
    }
    let (shared, peer) = (Arc::clone(shared), peer.clone());
    Some(Arc::new(move || {
        let (shared, peer) = (Arc::clone(&shared), peer.clone());
        Box::pin(async move { request(&shared, &peer, path).await })
    }))
}

/// Asks every connected client and waits for the first answer, the timeout, or shutdown.
pub(crate) async fn request(
    shared: &Arc<Shared>,
    peer: &PairedPeer,
    path: nexus_proto::v1::PathKind,
) -> Consent {
    let key = peer.identity.ed25519_bytes();
    // Nobody to ask means nobody can say yes: refuse straight away instead of making the
    // controller wait out the timeout. (A headless machine is made unattended on purpose with
    // `nexus trust unattended`.)
    if shared.events.receiver_count() == 0 {
        warn!(peer = %peer.name, "nobody is connected to approve this request; refusing (see `nexus consent`)");
        return Consent::Denied;
    }
    let id = shared.next_consent_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    shared.consents.lock().insert(
        id,
        Pending {
            answer: tx,
            peer: key,
        },
    );
    let timeout = shared.cfg.consent_timeout;
    shared.emit(Ev::ConsentRequest(ConsentRequest {
        request_id: id,
        peer_device_id: peer.identity.device_id().as_bytes().to_vec().into(),
        peer_name: peer.name.clone(),
        peer_fingerprint: peer.identity.fingerprint().to_string(),
        grants: peer.grants_from_peer.iter().cloned().collect(),
        timeout_secs: u32::try_from(timeout.as_secs()).unwrap_or(u32::MAX),
        path: path as i32,
    }));
    info!(peer = %peer.name, request = id, "waiting for approval");
    let mut shutdown = shared.shutdown.subscribe();
    let outcome = tokio::select! {
        answer = rx => match answer {
            Ok(d) if d.allow => {
                if d.remember {
                    match shared.policy.lock().set(&key, ConsentMode::Unattended) {
                        Ok(()) => info!(peer = %peer.name, "device is now pre-authorised"),
                        Err(e) => warn!(error = %e, "could not save the \"always allow\" choice"),
                    }
                    shared.emit_device_changed(&key);
                }
                Consent::Allowed
            }
            Ok(_) | Err(_) => Consent::Denied,
        },
        () = tokio::time::sleep(timeout) => Consent::TimedOut,
        _ = shutdown.changed() => Consent::Denied,
    };
    shared.consents.lock().remove(&id);
    shared.emit(Ev::ConsentResolved(ConsentResolved {
        request_id: id,
        allowed: outcome == Consent::Allowed,
        timed_out: outcome == Consent::TimedOut,
    }));
    info!(peer = %peer.name, ?outcome, "approval decided");
    outcome
}

/// An IPC client's answer to a prompt.
pub(crate) fn decide(shared: &Shared, decision: ConsentDecision) -> Result<(), AgentError> {
    let pending = shared
        .consents
        .lock()
        .remove(&decision.request_id)
        .ok_or_else(|| AgentError::NotFound("that request is no longer pending".into()))?;
    let _ = pending.peer;
    // The waiting task may have timed out in the same instant; that is the same as "too late".
    pending
        .answer
        .send(decision)
        .map_err(|_| AgentError::NotFound("that request is no longer pending".into()))
}

/// Changes whether a paired device is pre-authorised.
pub(crate) fn set_unattended(
    shared: &Shared,
    device_id: &[u8],
    unattended: bool,
) -> Result<(), AgentError> {
    let peer = shared
        .find_peer(device_id)
        .ok_or_else(|| AgentError::NotFound("no paired device with that id".into()))?;
    let key = peer.identity.ed25519_bytes();
    let mode = if unattended {
        ConsentMode::Unattended
    } else {
        ConsentMode::Attended
    };
    shared
        .policy
        .lock()
        .set(&key, mode)
        .map_err(|e| AgentError::Failed(format!("could not save the policy: {e}")))?;
    shared.emit_device_changed(&key);
    info!(peer = %peer.name, unattended, "consent policy changed");
    Ok(())
}
