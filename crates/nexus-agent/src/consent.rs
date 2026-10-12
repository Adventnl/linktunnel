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

/// Something that can ask the person at this machine without going through an IPC client: a
/// message box on the console session, a desktop notification, ...
pub trait ConsentPrompter: Send + Sync {
    /// Asks whether `peer_name` (with this key `fingerprint` and these `grants`) may use the
    /// machine. `Some(true)` = yes, `Some(false)` = no, `None` = no answer in time.
    fn ask(
        &self,
        peer_name: String,
        fingerprint: String,
        grants: Vec<String>,
        timeout: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<bool>> + Send>>;
}

impl std::fmt::Debug for dyn ConsentPrompter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConsentPrompter")
    }
}

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
    if shared
        .limits
        .lock()
        .locked_out(&key, std::time::Instant::now())
    {
        warn!(peer = %peer.name, "refused without asking: too many recent refusals");
        shared.audit.record(
            "consent.locked_out",
            Some(&peer.identity.device_id().to_string()),
            &[("name", &peer.name)],
        );
        return Consent::Denied;
    }
    // Nobody to ask means nobody can say yes: refuse straight away instead of making the
    // controller wait out the timeout. (A headless machine is made unattended on purpose with
    // `nexus trust unattended`.)
    let prompter = shared.cfg.consent_prompter.clone();
    if shared.events.receiver_count() == 0 && prompter.is_none() {
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
    let actor = peer.identity.device_id().to_string();
    shared
        .audit
        .record("consent.requested", Some(&actor), &[("name", &peer.name)]);
    let mut shutdown = shared.shutdown.subscribe();
    // The direct prompter races the IPC clients; the first answer wins.
    let direct = async {
        match &prompter {
            Some(p) => {
                p.ask(
                    peer.name.clone(),
                    peer.identity.fingerprint().to_string(),
                    peer.grants_from_peer.iter().cloned().collect(),
                    timeout,
                )
                .await
            }
            None => std::future::pending().await,
        }
    };
    let outcome = tokio::select! {
        said = direct => if said == Some(true) { Consent::Allowed } else { Consent::Denied },
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
    {
        let mut limits = shared.limits.lock();
        if outcome == Consent::Allowed {
            limits.approved(&key);
        } else if limits.refused(key, std::time::Instant::now()) {
            warn!(peer = %peer.name, "locking this device out of approval prompts for a while");
            shared.audit.record(
                "consent.lockout_started",
                Some(&actor),
                &[("name", &peer.name)],
            );
        }
    }
    shared.emit(Ev::ConsentResolved(ConsentResolved {
        request_id: id,
        allowed: outcome == Consent::Allowed,
        timed_out: outcome == Consent::TimedOut,
    }));
    info!(peer = %peer.name, ?outcome, "approval decided");
    shared.audit.record(
        "consent.decided",
        Some(&actor),
        &[("outcome", &format!("{outcome:?}"))],
    );
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
    shared.audit.record(
        "consent.policy_changed",
        Some(&peer.identity.device_id().to_string()),
        &[("unattended", if unattended { "yes" } else { "no" })],
    );
    info!(peer = %peer.name, unattended, "consent policy changed");
    Ok(())
}
