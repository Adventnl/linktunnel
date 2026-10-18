//! The shapes the UI sees (`src/lib/types.ts`), and the conversions from the agent's protobuf.

use nexus_proto::v1::{self, OsFamily, PathKind, PresenceState};
use serde::Serialize;

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

pub fn unhex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return Err("device id is not hex".into());
    }
    (0..s.len() / 2)
        .map(|i| {
            u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|_| "device id is not hex".to_owned())
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub state: &'static str,
    pub online: bool,
    pub last_seen_ms: u64,
    pub os_family: &'static str,
    pub os_version: String,
    pub path: &'static str,
    pub active_user: String,
    pub battery_percent: Option<u32>,
    pub fingerprint: String,
    pub unattended: bool,
    pub grants_on_peer: Vec<String>,
}

pub fn state_name(state: i32) -> &'static str {
    match PresenceState::try_from(state).unwrap_or(PresenceState::Unspecified) {
        PresenceState::Online => "online",
        PresenceState::Idle => "idle",
        PresenceState::Locked => "locked",
        PresenceState::InUse => "in_use",
        PresenceState::Sleeping => "sleeping",
        PresenceState::Hibernating => "hibernating",
        PresenceState::Offline => "offline",
        PresenceState::Unspecified => "unknown",
    }
}

pub fn path_name(path: i32) -> &'static str {
    match PathKind::try_from(path).unwrap_or(PathKind::Unspecified) {
        PathKind::Lan => "lan",
        PathKind::Direct => "direct",
        PathKind::Relay => "relay",
        PathKind::Unspecified => "none",
    }
}

impl From<&v1::DeviceView> for Device {
    fn from(d: &v1::DeviceView) -> Self {
        let (family, version) = d.os.as_ref().map_or(("unknown", String::new()), |os| {
            (
                match OsFamily::try_from(os.family).unwrap_or(OsFamily::Unspecified) {
                    OsFamily::Windows => "windows",
                    OsFamily::Macos => "macos",
                    OsFamily::Linux => "linux",
                    OsFamily::Unspecified => "unknown",
                },
                os.version.clone(),
            )
        });
        Self {
            id: hex(&d.device_id),
            name: d.name.clone(),
            state: state_name(d.state),
            online: d.online,
            last_seen_ms: d.last_seen_ms,
            os_family: family,
            os_version: version,
            path: path_name(d.path),
            active_user: d.active_user.clone(),
            // 255 is the agent's "no battery / unknown".
            battery_percent: (1..=100)
                .contains(&d.battery_percent)
                .then_some(d.battery_percent),
            fingerprint: d.fingerprint.clone(),
            unattended: d.unattended,
            grants_on_peer: d.grants_on_peer.clone(),
        }
    }
}

/// Someone connected to this machine right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveSession {
    pub id: u64,
    pub device_id: String,
    pub peer_name: String,
    pub started_ms: u64,
    pub path: &'static str,
    pub grants: Vec<String>,
}

impl From<&v1::ActiveSession> for ActiveSession {
    fn from(s: &v1::ActiveSession) -> Self {
        Self {
            id: s.id,
            device_id: hex(&s.peer_device_id),
            peer_name: s.peer_name.clone(),
            started_ms: s.started_ms,
            path: path_name(s.path),
            grants: s.grants.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub device_id: String,
    pub device_name: String,
    pub fingerprint: String,
    pub agent_version: String,
    pub signal_connected: bool,
    pub signal_url: String,
    pub lockdown: bool,
    pub paired_devices: u32,
    pub active_sessions: u32,
}

impl From<&v1::Status> for AgentStatus {
    fn from(s: &v1::Status) -> Self {
        Self {
            device_id: hex(&s.device_id),
            device_name: s.device_name.clone(),
            fingerprint: s.fingerprint.clone(),
            agent_version: s.agent_version.clone(),
            signal_connected: s.signal_connected,
            signal_url: s.signal_url.clone(),
            lockdown: s.lockdown,
            paired_devices: s.paired_devices,
            active_sessions: s.active_sessions,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub agent: &'static str,
    pub status: Option<AgentStatus>,
    pub devices: Vec<Device>,
    pub sessions: Vec<ActiveSession>,
    pub error: Option<String>,
}

impl Snapshot {
    pub fn unreachable(error: impl Into<String>) -> Self {
        Self {
            agent: "unreachable",
            status: None,
            devices: Vec::new(),
            sessions: Vec::new(),
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentRequest {
    pub request_id: u64,
    pub peer_name: String,
    pub peer_fingerprint: String,
    pub grants: Vec<String>,
    pub timeout_secs: u32,
    pub path: &'static str,
    pub received_at_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingSas {
    pub pairing_id: u64,
    pub words: Vec<String>,
    pub digits: String,
    pub peer_name: String,
    pub peer_fingerprint: String,
}

/// Everything pushed to the UI on the single `nexus://event` channel.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AppEvent {
    Notice {
        level: String,
        text: String,
    },
    Agent {
        agent: &'static str,
        error: Option<String>,
    },
    Snapshot {
        snapshot: Snapshot,
    },
    DeviceChanged {
        device: Device,
    },
    DeviceRemoved {
        id: String,
    },
    SignalState {
        connected: bool,
    },
    SessionsChanged {
        sessions: Vec<ActiveSession>,
    },
    ConsentRequest {
        request: ConsentRequest,
    },
    ConsentResolved {
        request_id: u64,
    },
    PairingSas {
        sas: PairingSas,
    },
    PairingFinished {
        pairing_id: u64,
        success: bool,
        reason: String,
        peer_name: String,
    },
    TerminalReady {
        terminal_id: u64,
        name: String,
        reattached: bool,
    },
    TerminalOutput {
        terminal_id: u64,
        data_b64: String,
    },
    TerminalEnded {
        terminal_id: u64,
        exited: bool,
        exit_code: i32,
        reason: String,
    },
}

/// Turns a pushed agent event into what the UI wants (or nothing: video and file traffic are
/// not the dashboard's business).
pub fn translate(ev: &v1::IpcEvent, now_ms: u64) -> Option<AppEvent> {
    use base64::Engine as _;
    use v1::ipc_event::Body;
    Some(match ev.body.as_ref()? {
        Body::DeviceChanged(c) => AppEvent::DeviceChanged {
            device: Device::from(c.device.as_ref()?),
        },
        Body::DeviceRemoved(r) => AppEvent::DeviceRemoved {
            id: hex(&r.device_id),
        },
        Body::SignalState(s) => AppEvent::SignalState {
            connected: s.connected,
        },
        Body::SessionsChanged(c) => AppEvent::SessionsChanged {
            sessions: c.sessions.iter().map(ActiveSession::from).collect(),
        },
        Body::ConsentRequest(r) => AppEvent::ConsentRequest {
            request: ConsentRequest {
                request_id: r.request_id,
                peer_name: r.peer_name.clone(),
                peer_fingerprint: r.peer_fingerprint.clone(),
                grants: r.grants.clone(),
                timeout_secs: r.timeout_secs,
                path: path_name(r.path),
                received_at_ms: now_ms,
            },
        },
        Body::ConsentResolved(r) => AppEvent::ConsentResolved {
            request_id: r.request_id,
        },
        Body::PairingSas(s) => AppEvent::PairingSas {
            sas: PairingSas {
                pairing_id: s.pairing_id,
                words: s.words.clone(),
                digits: s.digits.clone(),
                peer_name: s.peer_name.clone(),
                peer_fingerprint: s.peer_fingerprint.clone(),
            },
        },
        Body::PairingFinished(f) => AppEvent::PairingFinished {
            pairing_id: f.pairing_id,
            success: f.success,
            reason: f.reason.clone(),
            peer_name: f.peer_name.clone(),
        },
        Body::TerminalReady(t) => AppEvent::TerminalReady {
            terminal_id: t.terminal_id,
            name: t.name.clone(),
            reattached: t.reattached,
        },
        Body::TerminalOutput(o) => AppEvent::TerminalOutput {
            terminal_id: o.terminal_id,
            data_b64: base64::engine::general_purpose::STANDARD.encode(&o.data),
        },
        Body::TerminalEnded(e) => AppEvent::TerminalEnded {
            terminal_id: e.terminal_id,
            exited: e.exited,
            exit_code: e.exit_code,
            reason: e.reason.clone(),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> v1::DeviceView {
        v1::DeviceView {
            device_id: vec![0xab, 0x01].into(),
            name: "Windows PC".into(),
            state: PresenceState::Locked as i32,
            online: true,
            last_seen_ms: 123,
            os: Some(v1::OsInfo {
                family: OsFamily::Windows as i32,
                version: "Windows 11".into(),
                ..Default::default()
            }),
            path: PathKind::Lan as i32,
            active_user: "alex".into(),
            battery_percent: 255,
            fingerprint: "FP".into(),
            unattended: true,
            grants_on_peer: vec!["screen".into()],
            ..Default::default()
        }
    }

    #[test]
    fn hex_roundtrip_and_rejects_junk() {
        assert_eq!(hex(&[0xab, 0x01, 0xff]), "ab01ff");
        assert_eq!(unhex("ab01ff").unwrap(), [0xab, 0x01, 0xff]);
        assert!(unhex("abc").is_err());
        assert!(unhex("zz").is_err());
        assert!(unhex("é1").is_err());
    }

    #[test]
    fn device_conversion() {
        let d = Device::from(&view());
        assert_eq!(d.id, "ab01");
        assert_eq!(d.state, "locked");
        assert_eq!(d.os_family, "windows");
        assert_eq!(d.path, "lan");
        assert_eq!(d.battery_percent, None, "255 means unknown");
        let mut v = view();
        v.battery_percent = 64;
        assert_eq!(Device::from(&v).battery_percent, Some(64));
    }

    #[test]
    fn json_shape_matches_the_typescript_types() {
        let json = serde_json::to_value(Device::from(&view())).unwrap();
        for key in [
            "id",
            "name",
            "state",
            "online",
            "lastSeenMs",
            "osFamily",
            "osVersion",
            "path",
            "activeUser",
            "batteryPercent",
            "fingerprint",
            "unattended",
            "grantsOnPeer",
        ] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
        let ev = serde_json::to_value(AppEvent::TerminalOutput {
            terminal_id: 7,
            data_b64: "aGk=".into(),
        })
        .unwrap();
        assert_eq!(ev["kind"], "terminal_output");
        assert_eq!(ev["terminalId"], 7);
        assert_eq!(ev["dataB64"], "aGk=");
        let ev = serde_json::to_value(AppEvent::PairingFinished {
            pairing_id: 1,
            success: false,
            reason: "x".into(),
            peer_name: "p".into(),
        })
        .unwrap();
        assert_eq!(ev["kind"], "pairing_finished");
        assert_eq!(ev["pairingId"], 1);
    }

    #[test]
    fn ipc_events_translate_and_irrelevant_ones_are_dropped() {
        use v1::ipc_event::Body;
        let changed = v1::IpcEvent {
            body: Some(Body::DeviceChanged(v1::DeviceChanged {
                device: Some(view()),
            })),
        };
        assert!(matches!(
            translate(&changed, 0),
            Some(AppEvent::DeviceChanged { .. })
        ));
        let video = v1::IpcEvent {
            body: Some(Body::SessionVideo(v1::SessionVideo::default())),
        };
        assert!(translate(&video, 0).is_none());
        let consent = v1::IpcEvent {
            body: Some(Body::ConsentRequest(v1::ConsentRequest {
                request_id: 9,
                peer_name: "P".into(),
                timeout_secs: 30,
                ..Default::default()
            })),
        };
        let Some(AppEvent::ConsentRequest { request }) = translate(&consent, 55) else {
            panic!()
        };
        assert_eq!(
            (
                request.request_id,
                request.received_at_ms,
                request.timeout_secs
            ),
            (9, 55, 30)
        );
        let out = v1::IpcEvent {
            body: Some(Body::TerminalOutput(v1::TerminalOutput {
                terminal_id: 3,
                data: vec![1, 2, 3].into(),
            })),
        };
        let Some(AppEvent::TerminalOutput { data_b64, .. }) = translate(&out, 0) else {
            panic!()
        };
        assert_eq!(data_b64, "AQID");
        let who = v1::IpcEvent {
            body: Some(Body::SessionsChanged(v1::SessionsChanged {
                sessions: vec![v1::ActiveSession {
                    id: 4,
                    peer_device_id: vec![0xab, 0x01].into(),
                    peer_name: "MacBook".into(),
                    path: v1::PathKind::Relay as i32,
                    grants: vec!["screen".into()],
                    ..Default::default()
                }],
            })),
        };
        let Some(AppEvent::SessionsChanged { sessions }) = translate(&who, 0) else {
            panic!()
        };
        assert_eq!(
            (
                sessions[0].peer_name.as_str(),
                sessions[0].device_id.as_str(),
                sessions[0].path
            ),
            ("MacBook", "ab01", "relay")
        );
        assert!(translate(&v1::IpcEvent { body: None }, 0).is_none());
    }
}
