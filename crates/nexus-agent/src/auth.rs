//! Session authentication runs before the target handler can accept any resource streams.
use crate::{error::AgentError, state::Shared, viewer_bridge::ConnCtx};
use nexus_net::{Established, Session};
use nexus_proto::features;
use nexus_proto::v1::{
    AuthRequest, AuthResponse, ControlMessage, Hello, StepUpChallenge, StepUpProof, StepUpResult,
    control_message::Body,
};
use nexus_trust::PairedPeer;
use std::sync::Arc;
use std::time::{Duration, Instant};
const TTL: Duration = Duration::from_secs(120);

pub(crate) struct RemotePending {
    owner: Arc<ConnCtx>,
    device: Vec<u8>,
    session: Session,
    established: Established,
    challenge: nexus_auth::Challenge,
    expires: Instant,
}
pub(crate) struct Ready {
    pub session: Session,
    pub established: Established,
    pub expires: Instant,
}
fn denied(e: impl std::fmt::Display) -> AgentError {
    AgentError::Denied(e.to_string())
}
fn capable(hello: &Hello) -> bool {
    hello
        .capabilities
        .as_ref()
        .is_some_and(|c| c.features.iter().any(|f| f == features::AUTH_STEPUP))
}
fn binding(local: &Hello, peer: &Hello) -> Result<[u8; 32], AgentError> {
    if local.session_nonce.len() != 16 || peer.session_nonce.len() != 16 {
        return Err(denied("invalid session binding"));
    }
    let mut out = [0; 32];
    out[..16].copy_from_slice(&local.session_nonce);
    out[16..].copy_from_slice(&peer.session_nonce);
    Ok(out)
}
pub(crate) async fn incoming(
    shared: &Arc<Shared>,
    peer: &PairedPeer,
    session: &Session,
    hello: &Hello,
    established: &mut Established,
) -> Result<(), AgentError> {
    let required = shared.cfg.unattended_requires_stepup
        && shared.policy.lock().is_unattended(&session.peer());
    if !capable(&established.peer_hello) {
        return if required {
            Err(denied("controller does not support required step-up"))
        } else {
            Ok(())
        };
    }
    if required
        && shared
            .limits
            .lock()
            .locked_out(&session.peer(), Instant::now())
    {
        return Err(denied("too many refused step-up attempts; try later"));
    }
    let challenge = if required {
        Some(
            shared
                .auth
                .lock()
                .begin(session.peer(), binding(hello, &established.peer_hello)?)
                .map_err(denied)?,
        )
    } else {
        None
    };
    established
        .control
        .send(&ControlMessage {
            body: Some(Body::StepUpChallenge(StepUpChallenge {
                required,
                challenge_json: challenge
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(denied)?
                    .unwrap_or_default(),
            })),
            ..Default::default()
        })
        .await?;
    let Some(challenge) = challenge else {
        return Ok(());
    };
    let response = tokio::time::timeout(TTL, established.control.recv::<ControlMessage>()).await;
    let proof = match response {
        Ok(Ok(Some(ControlMessage {
            body: Some(Body::StepUpProof(proof)),
            ..
        }))) => Some(proof),
        _ => None,
    };
    let supplied_proof = proof.is_some();
    let mut result = match proof {
        Some(proof) if proof.challenge_id == challenge.id => shared
            .auth
            .lock()
            .finish(
                &proof.challenge_id,
                session.peer(),
                binding(hello, &established.peer_hello)?,
                &proof.totp,
                &proof.assertion_json,
                nexus_core::clock::unix_ms() / 1000,
            )
            .map_err(denied),
        _ => Err(denied("step-up expired or malformed")),
    };
    shared.auth.lock().cancel(&challenge.id);
    if shared.is_locked_down()
        || shared
            .find_peer(peer.identity.device_id().as_bytes())
            .is_none()
    {
        result = Err(denied("access revoked during step-up"));
    }

    established
        .control
        .send(&ControlMessage {
            body: Some(Body::StepUpResult(StepUpResult { ok: result.is_ok() })),
            ..Default::default()
        })
        .await?;
    if supplied_proof && result.is_err() {
        shared.limits.lock().refused(session.peer(), Instant::now());
    }
    result?;
    shared.audit.record(
        "auth.verified",
        Some(&peer.identity.device_id().to_string()),
        &[],
    );
    Ok(())
}
pub(crate) async fn challenge(
    established: &mut Established,
) -> Result<Option<nexus_auth::Challenge>, AgentError> {
    if !capable(&established.peer_hello) {
        return Ok(None);
    }
    let message = tokio::time::timeout(
        Duration::from_secs(10),
        established.control.recv::<ControlMessage>(),
    )
    .await
    .map_err(|_| denied("step-up negotiation timed out"))??;
    match message.and_then(|m| m.body) {
        Some(Body::StepUpChallenge(c)) if !c.required => Ok(None),
        Some(Body::StepUpChallenge(c)) => serde_json::from_str(&c.challenge_json)
            .map(Some)
            .map_err(denied),
        _ => Err(denied("target did not negotiate step-up")),
    }
}
pub(crate) async fn dispatch(
    shared: &Arc<Shared>,
    owner: &Arc<ConnCtx>,
    req: AuthRequest,
) -> Result<AuthResponse, AgentError> {
    let response = |handle: String, json: serde_json::Value| AuthResponse {
        handle,
        json: json.to_string(),
    };
    match req.operation.as_str() {
        "status" => {
            let (passkeys, totp) = shared.auth.lock().methods();
            Ok(response(
                String::new(),
                serde_json::json!({"passkeys":passkeys,"totp":totp,"unattended_requires_stepup":shared.cfg.unattended_requires_stepup}),
            ))
        }
        "enroll-passkey" | "enroll-totp" => {
            let (id, json) = shared
                .auth
                .lock()
                .enroll(req.operation == "enroll-passkey", &shared.cfg.device_name)
                .map_err(denied)?;
            Ok(response(id, json))
        }
        "enroll-finish" => {
            let value = if req.totp.is_empty() {
                &req.response_json
            } else {
                &req.totp
            };
            shared
                .auth
                .lock()
                .finish_enrollment(&req.handle, value, nexus_core::clock::unix_ms() / 1000)
                .map_err(denied)?;
            shared.audit.record("auth.enrolled", None, &[]);
            Ok(response(String::new(), serde_json::json!({"ok":true})))
        }
        "remote-begin" => {
            {
                let mut pending = shared.remote_auth.lock();
                pending.retain(|_, p| p.expires > Instant::now());
                if pending.len() >= 16 {
                    return Err(AgentError::Busy("too many pending authentications".into()));
                }
            }
            let (session, mut established) =
                crate::sessions::dial_raw(shared, &req.device_id).await?;
            let challenge = challenge(&mut established).await?;
            let Some(challenge) = challenge else {
                shared.ready_auth.lock().insert(
                    req.device_id.to_vec(),
                    Ready {
                        session,
                        established,
                        expires: Instant::now() + Duration::from_secs(60),
                    },
                );
                return Ok(response(
                    String::new(),
                    serde_json::json!({"required":false}),
                ));
            };
            let id = challenge.id.clone();
            let json = serde_json::to_value(&challenge).map_err(denied)?;
            shared.remote_auth.lock().insert(
                id.clone(),
                RemotePending {
                    owner: owner.clone(),
                    device: req.device_id.to_vec(),
                    session,
                    established,
                    challenge,
                    expires: Instant::now() + TTL,
                },
            );
            Ok(response(id, json))
        }
        "remote-finish" => {
            let mut pending = shared
                .remote_auth
                .lock()
                .remove(&req.handle)
                .ok_or_else(|| denied("unknown authentication"))?;
            if !Arc::ptr_eq(owner, &pending.owner) || pending.expires <= Instant::now() {
                return Err(denied(
                    "authentication expired or belongs to another IPC connection",
                ));
            }
            pending
                .established
                .control
                .send(&ControlMessage {
                    body: Some(Body::StepUpProof(StepUpProof {
                        challenge_id: pending.challenge.id,
                        totp: req.totp,
                        assertion_json: req.response_json,
                    })),
                    ..Default::default()
                })
                .await?;
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                pending.established.control.recv::<ControlMessage>(),
            )
            .await
            .map_err(|_| denied("authentication response timed out"))??;
            if !matches!(
                result.and_then(|m| m.body),
                Some(Body::StepUpResult(StepUpResult { ok: true }))
            ) {
                return Err(denied("step-up refused"));
            }
            shared.ready_auth.lock().insert(
                pending.device,
                Ready {
                    session: pending.session,
                    established: pending.established,
                    expires: Instant::now() + Duration::from_secs(60),
                },
            );
            Ok(response(String::new(), serde_json::json!({"ok":true})))
        }
        _ => Err(AgentError::Invalid("unknown auth operation".into())),
    }
}
pub(crate) fn close_owned(shared: &Shared, owner: &Arc<ConnCtx>) {
    shared
        .remote_auth
        .lock()
        .retain(|_, p| !Arc::ptr_eq(&p.owner, owner));
}

/// Expired pending/verified connections are actively dropped even on an idle agent.
pub(crate) async fn expire(shared: Arc<Shared>) {
    let mut shutdown = shared.shutdown.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tick.tick() => {
                let now = Instant::now();
                shared.remote_auth.lock().retain(|_, p| p.expires > now && !p.session.is_closed());
                shared.ready_auth.lock().retain(|_, p| p.expires > now && !p.session.is_closed());
            }
        }
    }
    shared.remote_auth.lock().clear();
    shared.ready_auth.lock().clear();
}
