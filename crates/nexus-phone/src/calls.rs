//! Native Bluetooth cellular calls. Independent of adb, scrcpy and desktop agents.
//!
//! Windows owns the HFP audio path and uses its default communications endpoints.
//! No call recording, phone microphone capture or credential injection is used.

use serde::{Deserialize, Serialize};
use std::sync::mpsc::{self, SyncSender};
use tokio::sync::oneshot;

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

/// A Bluetooth phone explicitly selected for calling (not an adb identifier).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// Opaque Windows transport identifier.
    pub id: String,
    /// Bluetooth device's friendly name.
    pub name: String,
}

/// A SIM/line belonging to the selected Bluetooth phone.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    /// Opaque line identifier; only current selected-phone lines may be used.
    pub id: String,
    /// Phone-provided line label.
    pub name: String,
    /// Whether the phone currently permits outgoing calls.
    pub can_dial: bool,
}

/// One live call. This list contains no historical calls or contacts.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Call {
    /// Opaque call identifier.
    pub id: String,
    /// Current state reported by Windows.
    pub state: String,
    /// True only when Windows reports LocalDevice for this specific call.
    pub on_computer: bool,
    /// Windows call mute state.
    pub muted: bool,
}

/// Read-only diagnostics and current call state. Capability is not audio proof.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Whether the Windows Bluetooth phone transport API exists.
    pub available: bool,
    /// Whether Windows exposes a local Bluetooth adapter.
    pub bluetooth: bool,
    /// Whether the process has the package identity required for phone permissions.
    pub packaged: bool,
    /// Whether this Windows version exposes per-call control and route reporting.
    pub call_control: bool,
    /// Phones offered by the Windows HFP transport enumerator.
    pub devices: Vec<Device>,
    /// Explicitly connected transport, if any.
    pub device_id: Option<String>,
    /// Whether the transport can route audio locally (not proof of a live route).
    pub can_route: bool,
    /// Lines from the selected transport only.
    pub lines: Vec<Line>,
    /// Live calls from those lines only.
    pub calls: Vec<Call>,
    /// Friendly default communications microphone, if one is available.
    pub input: Option<String>,
    /// Friendly default communications playback device, if available.
    pub output: Option<String>,
    /// An operation was sent but its subsequent status/route needs attention.
    pub notice: Option<String>,
}

/// Explicit user action; there are no automatic dial/answer/retry operations.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Action {
    /// Dial a number through an explicitly selected line.
    Dial {
        /// Current selected transport's line id.
        #[serde(rename = "lineId")]
        line_id: String,
        /// Telephone number, not a URI or service/USSD code.
        number: String,
    },
    /// Answer a currently ringing call.
    Answer {
        /// Current selected transport's call id.
        #[serde(rename = "callId")]
        call_id: String,
    },
    /// End a live call.
    End {
        /// Current selected transport's call id.
        #[serde(rename = "callId")]
        call_id: String,
    },
    /// Move an existing call's audio to this computer.
    TakeAudio {
        /// Current selected transport's call id.
        #[serde(rename = "callId")]
        call_id: String,
    },
    /// Change the call's microphone mute state.
    Mute {
        /// Current selected transport's call id.
        #[serde(rename = "callId")]
        call_id: String,
        /// Requested mute state.
        muted: bool,
    },
}

enum Request {
    Status,
    Connect(String),
    Disconnect,
    Action(Action),
}

type Reply = Result<Snapshot, String>;
type Message = (Request, oneshot::Sender<Reply>);

/// Serializes Windows telephony work on its own COM apartment, away from the UI.
pub struct Calls {
    sender: SyncSender<Message>,
}

impl Default for Calls {
    fn default() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Message>(4);
        // The last sender dropping closes this loop. Backend cleanup runs on the
        // same apartment that created its Windows objects.
        std::thread::spawn(move || {
            #[cfg(windows)]
            let mut backend = windows::Backend::new();
            while let Ok((request, reply)) = receiver.recv() {
                // A timed-out/closed caller cannot leave a queued dial behind.
                if reply.is_closed() {
                    continue;
                }
                #[cfg(windows)]
                let result = match &mut backend {
                    Ok(backend) => backend.run(request),
                    Err(error) => Err(error.clone()),
                };
                #[cfg(not(windows))]
                let result = {
                    let _ = request;
                    Err(
                        "Native Bluetooth cellular calls are currently available on Windows only."
                            .into(),
                    )
                };
                let _ = reply.send(result);
            }
        });
        Self { sender }
    }
}

impl Calls {
    async fn request(&self, request: Request) -> Reply {
        let (sender, receiver) = oneshot::channel();
        self.sender.try_send((request, sender)).map_err(|_| {
            "The call bridge is busy or stopped. Wait for the current operation to finish."
                .to_owned()
        })?;
        receiver
            .await
            .map_err(|_| "The native call bridge stopped.".to_owned())?
    }

    /// Discover call-capable phones and read the current route, without prompting.
    pub async fn status(&self) -> Reply {
        self.request(Request::Status).await
    }

    /// Request Windows permission and connect the exact selected Bluetooth phone.
    pub async fn connect(&self, device_id: String) -> Reply {
        if device_id.is_empty() || device_id.len() > 4096 || device_id.chars().any(char::is_control)
        {
            return Err("Choose a Bluetooth phone from the device list.".into());
        }
        self.request(Request::Connect(device_id)).await
    }

    /// Release this app's transport registration. Does not end a cellular call.
    pub async fn disconnect(&self) -> Reply {
        self.request(Request::Disconnect).await
    }

    /// Perform one deliberate call operation; operations are never retried.
    pub async fn action(&self, action: Action) -> Reply {
        if let Action::Dial { number, .. } = &action {
            validate_number(number)?;
        }
        self.request(Request::Action(action)).await
    }
}

fn validate_number(number: &str) -> Result<(), String> {
    let number = number.trim();
    let mut digits = 0;
    for (index, c) in number.chars().enumerate() {
        match c {
            '0'..='9' => digits += 1,
            '+' if index == 0 => {}
            ' ' | '-' | '(' | ')' | '.' => {}
            _ => return Err("Enter a phone number using digits and an optional leading +.".into()),
        }
    }
    if number.len() > 80 || !(1..=20).contains(&digits) {
        return Err("Enter a phone number with 1 to 20 digits.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_numbers_but_never_commands_uris_or_service_codes() {
        for number in ["+64 21 123 456", "(09) 123-4567", "123", "0"] {
            assert!(validate_number(number).is_ok(), "{number}");
        }
        for number in [
            "",
            "+",
            "tel:123",
            "*#06#",
            "123;456",
            "123\n456",
            "++123",
            "123abc",
            "123456789012345678901",
        ] {
            assert!(validate_number(number).is_err(), "{number}");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn native_read_and_invalid_actions_never_register_a_phone() {
        let calls = Calls::default();
        let initial = calls.status().await.unwrap();
        assert!(initial.device_id.is_none());
        assert!(initial.lines.is_empty());
        assert!(initial.calls.is_empty());
        assert!(calls.connect(String::new()).await.is_err());
        assert!(
            calls
                .action(Action::End {
                    call_id: "not-a-live-call".into()
                })
                .await
                .is_err()
        );
        assert!(
            calls
                .action(Action::Dial {
                    line_id: "not-a-line".into(),
                    number: "*#06#".into()
                })
                .await
                .is_err()
        );
        let final_state = calls.status().await.unwrap();
        assert!(final_state.device_id.is_none());
        assert!(final_state.lines.is_empty());
    }
}
