//! Power actions and Wake-on-LAN on behalf of local clients.

use std::sync::Arc;

use nexus_media::MediaError;
use nexus_media::controller::{ControllerOptions, ControllerSession};
use nexus_proto::v1::PowerCommand;
use tracing::info;

use crate::error::AgentError;
use crate::sessions;
use crate::state::Shared;
use crate::viewer_bridge::{ConnCtx, controller_for};

impl From<MediaError> for AgentError {
    fn from(e: MediaError) -> Self {
        match e {
            MediaError::Denied(m) => Self::Denied(m),
            other => Self::Failed(other.to_string()),
        }
    }
}

/// Lock / sleep / restart ... the device at the other end of an open session.
pub(crate) async fn power(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    cmd: PowerCommand,
) -> Result<(), AgentError> {
    let controller = controller_for(shared, ctx, cmd.session_id)?;
    let kind = nexus_proto::v1::PowerActionKind::try_from(cmd.action)
        .map_err(|_| AgentError::Invalid("unknown power action".into()))?;
    Ok(controller.power(kind).await?)
}

/// Wakes a sleeping device: from this machine's network, or through another device's.
pub(crate) async fn wake(
    shared: &Arc<Shared>,
    device_id: &[u8],
    via: &[u8],
) -> Result<(), AgentError> {
    let peer = shared
        .find_peer(device_id)
        .ok_or_else(|| AgentError::NotFound("no paired device with that id".into()))?;
    let macs = shared.wakebook.lock().macs(&peer.identity.ed25519_bytes());
    if macs.is_empty() {
        return Err(AgentError::Invalid(
            "no hardware address known for that device yet: it has to be seen online once".into(),
        ));
    }
    if via.is_empty() {
        let n = macs.len();
        tokio::task::spawn_blocking(move || {
            for m in &macs {
                let mac = nexus_power::parse_mac(m).map_err(|e| e.to_string())?;
                nexus_power::send_magic_packet(mac, std::net::Ipv4Addr::BROADCAST, 9)
                    .map_err(|e| e.to_string())?;
            }
            Ok::<(), String>(())
        })
        .await
        .map_err(|e| AgentError::Failed(e.to_string()))?
        .map_err(AgentError::Failed)?;
        info!(peer = %peer.name, packets = n, "sent Wake-on-LAN packets");
        return Ok(());
    }
    let relay = shared.find_peer(via).ok_or_else(|| {
        AgentError::NotFound("no paired device with that id to relay through".into())
    })?;
    let (session, established) = sessions::dial(shared, via).await?;
    let (controller, _streams) = ControllerSession::start(
        session,
        established.control,
        ControllerOptions {
            want_video: false,
            ..Default::default()
        },
    )
    .await?;
    let result = controller.wake_relay(macs).await;
    controller.close().await;
    info!(peer = %peer.name, via = %relay.name, ok = result.is_ok(), "asked a device to relay Wake-on-LAN");
    Ok(result?)
}
