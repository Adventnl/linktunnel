//! Windows telephony boundary. All objects live on the dedicated MTA thread.
//!
//! The only unsafe operations initialize/uninitialize that thread's WinRT
//! apartment. Windows, not our process, transports HFP call audio.
use super::{Action, Call, Device, Line, Request, Snapshot};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use windows::ApplicationModel::{Calls::*, Package};
use windows::Devices::Bluetooth::BluetoothAdapter;
use windows::Devices::Enumeration::{DeviceAccessStatus, DeviceInformation};
use windows::Foundation::{Metadata::ApiInformation, TypedEventHandler};
use windows::Media::Devices::{AudioDeviceRole, MediaDevice};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::core::{GUID, HSTRING, RuntimeType};
use windows_future::{AsyncStatus, IAsyncOperation};

const IO_TIMEOUT: Duration = Duration::from_secs(10);
const ENUM_TIMEOUT: Duration = Duration::from_secs(5);

struct Apartment;
impl Apartment {
    fn new() -> Result<Self, String> {
        // SAFETY: This is a new dedicated thread, with no prior COM apartment.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(error)?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: Balanced against successful RoInitialize, on the same thread,
        // after all backend WinRT objects have been released.
        unsafe { RoUninitialize() };
    }
}

pub(super) struct Backend {
    transport: Option<PhoneLineTransportDevice>,
    watcher: Option<PhoneLineWatcher>,
    ids: Arc<Mutex<Vec<GUID>>>,
    // Declared last: WinRT objects must be released before the apartment.
    _apartment: Apartment,
}

impl Backend {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            transport: None,
            watcher: None,
            ids: Arc::default(),
            _apartment: Apartment::new()?,
        })
    }

    pub fn run(&mut self, request: Request) -> Result<Snapshot, String> {
        match request {
            Request::Status => self.status(),
            Request::Connect(id) => {
                if self.transport.is_some() {
                    return Err(
                        "Disconnect the current call-audio phone before choosing another.".into(),
                    );
                }
                let result = self.connect(&id);
                if result.is_err() {
                    // Roll back registration if permission/connection/watcher setup failed.
                    let _ = self.release();
                }
                result
            }
            Request::Disconnect => {
                self.release()?;
                self.status()
            }
            Request::Action(action) => self.action(action),
        }
    }

    fn status(&self) -> Result<Snapshot, String> {
        let mut snapshot = Snapshot {
            available: ApiInformation::IsTypePresent(&HSTRING::from(
                "Windows.ApplicationModel.Calls.PhoneLineTransportDevice",
            ))
            .map_err(error)?,
            packaged: Package::Current().is_ok(),
            call_control: ApiInformation::IsMethodPresent(
                &HSTRING::from("Windows.ApplicationModel.Calls.PhoneLine"),
                &HSTRING::from("GetAllActivePhoneCallsAsync"),
            )
            .map_err(error)?,
            ..Snapshot::default()
        };
        if !snapshot.available {
            return Ok(snapshot);
        }
        snapshot.bluetooth = BluetoothAdapter::GetDefaultAsync()
            .ok()
            .and_then(|op| wait(op, ENUM_TIMEOUT).ok())
            .is_some();
        snapshot.devices = devices()?;
        snapshot.input = endpoint_name(true);
        snapshot.output = endpoint_name(false);
        if let Some(transport) = &self.transport {
            snapshot.device_id = Some(transport.DeviceId().map_err(error)?.to_string());
            snapshot.can_route = transport.AudioRoutingStatus().map_err(error)?
                == TransportDeviceAudioRoutingStatus::CanRouteToLocalDevice;
            for line in self.lines()? {
                snapshot.lines.push(Line {
                    id: format!("{:?}", line.Id().map_err(error)?),
                    name: line.DisplayName().map_err(error)?.to_string(),
                    can_dial: line.CanDial().map_err(error)?,
                });
                if snapshot.call_control {
                    for call in active_calls(&line)? {
                        snapshot.calls.push(Call {
                            id: call.CallId().map_err(error)?.to_string(),
                            state: call_state(call.Status().map_err(error)?).into(),
                            on_computer: call.AudioDevice().map_err(error)?
                                == PhoneCallAudioDevice::LocalDevice,
                            muted: call.IsMuted().map_err(error)?,
                        });
                    }
                }
            }
        }
        Ok(snapshot)
    }

    fn connect(&mut self, id: &str) -> Result<Snapshot, String> {
        let status = self.status()?;
        if !status.available {
            return Err("This Windows version does not expose Bluetooth cellular calls.".into());
        }
        if !status.packaged {
            return Err("This Link Tunnel build has no Windows call permissions. Install the signed call-enabled Link Tunnel package, then reopen it. Phone Link is not required.".into());
        }
        if !status.bluetooth {
            return Err("Windows is not exposing a Bluetooth adapter to Link Tunnel. Turn on Bluetooth and check its driver and permissions, then refresh. Native cellular call audio needs a working Bluetooth connection.".into());
        }
        if !status.devices.iter().any(|device| device.id == id) {
            return Err(
                "That Bluetooth phone is no longer available. Refresh and choose it again.".into(),
            );
        }
        let transport = PhoneLineTransportDevice::FromId(&HSTRING::from(id)).map_err(error)?;
        match wait(transport.RequestAccessAsync().map_err(error)?, IO_TIMEOUT)? {
            DeviceAccessStatus::Allowed => {},
            DeviceAccessStatus::DeniedByUser => return Err("Windows call access was declined. Allow Link Tunnel to manage this Bluetooth phone, then reconnect.".into()),
            _ => return Err("Windows denied Bluetooth call access. This installation needs the phoneLineTransportManagement capability; some Windows builds may also restrict third-party calling apps.".into()),
        }
        self.transport = Some(transport.clone());
        transport.RegisterApp().map_err(error)?;
        // RegisterApp can succeed without registering on some Windows builds.
        // Never treat RequestAccessAsync == Allowed as proof of registration.
        if !transport.IsRegistered().map_err(error)? {
            return Err("Windows did not register Link Tunnel as a calling app. Call audio is unavailable on this Windows build; no call was placed.".into());
        }
        if !wait(transport.ConnectAsync().map_err(error)?, IO_TIMEOUT)? {
            return Err("Could not connect the Bluetooth calling service. Keep the phone nearby and approve its call-audio permissions.".into());
        }
        self.watch_lines()?;
        self.status()
    }

    fn watch_lines(&mut self) -> Result<(), String> {
        let store = wait(
            PhoneCallManager::RequestStoreAsync().map_err(error)?,
            IO_TIMEOUT,
        )?;
        let watcher = store.RequestLineWatcher().map_err(error)?;
        let ids = Arc::clone(&self.ids);
        watcher
            .LineAdded(&TypedEventHandler::new(
                move |_, args: windows::core::Ref<'_, PhoneLineWatcherEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        let id = args.LineId()?;
                        if let Ok(mut ids) = ids.lock()
                            && ids.len() < 32
                            && !ids.contains(&id)
                        {
                            ids.push(id);
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(error)?;
        let ids = Arc::clone(&self.ids);
        watcher
            .LineRemoved(&TypedEventHandler::new(
                move |_, args: windows::core::Ref<'_, PhoneLineWatcherEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        let id = args.LineId()?;
                        if let Ok(mut ids) = ids.lock() {
                            ids.retain(|line| *line != id);
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(error)?;
        let complete = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&complete);
        watcher
            .EnumerationCompleted(&TypedEventHandler::new(move |_, _| {
                done.store(true, Ordering::Release);
                Ok(())
            }))
            .map_err(error)?;
        self.watcher = Some(watcher.clone());
        watcher.Start().map_err(error)?;
        let started = Instant::now();
        while !complete.load(Ordering::Acquire) {
            if started.elapsed() >= ENUM_TIMEOUT {
                return Err(
                    "Windows timed out while discovering this phone's cellular lines.".into(),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn lines(&self) -> Result<Vec<PhoneLine>, String> {
        let Some(transport) = &self.transport else {
            return Ok(Vec::new());
        };
        let selected = transport.DeviceId().map_err(error)?;
        let ids = self
            .ids
            .lock()
            .map_err(|_| "The phone line watcher stopped.".to_owned())?
            .clone();
        let mut lines = Vec::new();
        for id in ids {
            let line = wait(PhoneLine::FromIdAsync(id).map_err(error)?, ENUM_TIMEOUT)?;
            if line.Transport().map_err(error)? == PhoneLineTransport::Bluetooth
                && line.TransportDeviceId().map_err(error)? == selected
            {
                lines.push(line);
            }
        }
        Ok(lines)
    }

    fn action(&self, action: Action) -> Result<Snapshot, String> {
        let before = self.status()?;
        if before.device_id.is_none() {
            return Err("Connect a Bluetooth phone for call audio first.".into());
        }
        if matches!(action, Action::Answer { .. } | Action::TakeAudio { .. })
            && (!before.can_route || before.input.is_none() || before.output.is_none())
        {
            return Err("The PC call-audio route, microphone and speakers must be available before answering or taking call audio.".into());
        }
        match action {
            Action::Dial { line_id, number } => {
                if !before.can_route || before.input.is_none() || before.output.is_none() {
                    return Err("The PC call-audio route, microphone and speakers must be available before dialing.".into());
                }
                if PhoneCallManager::IsCallActive().map_err(error)?
                    || PhoneCallManager::IsCallIncoming().map_err(error)?
                    || !before.calls.is_empty()
                {
                    return Err(
                        "A call is already in progress. End it before dialing again.".into(),
                    );
                }
                let line = self
                    .lines()?
                    .into_iter()
                    .find(|line| line.Id().is_ok_and(|id| format!("{id:?}") == line_id))
                    .ok_or("That line is no longer available on the selected phone.")?;
                if !line.CanDial().map_err(error)? {
                    return Err("This phone line cannot currently place a call.".into());
                }
                if before.call_control {
                    let result = wait(
                        line.DialWithResultAsync(&HSTRING::from(number.trim()), &HSTRING::new())
                            .map_err(error)?,
                        IO_TIMEOUT,
                    )?;
                    operation(result.DialCallStatus().map_err(error)?)?;
                    let call = result.DialedCall().map_err(error)?;
                    if let Err(reason) = set_route(&call, PhoneCallAudioDevice::LocalDevice) {
                        return self.after_action(Some(format!("Call placed, but PC audio transfer failed: {reason} Do not redial; use Take audio on PC for the existing call.")));
                    }
                } else {
                    // Windows 10 exposes dialing options but not the newer
                    // per-call APIs. Request local speakerphone explicitly;
                    // the UI labels the route as unverified on these versions.
                    let options = PhoneDialOptions::new().map_err(error)?;
                    options
                        .SetNumber(&HSTRING::from(number.trim()))
                        .map_err(error)?;
                    options
                        .SetAudioEndpoint(PhoneAudioRoutingEndpoint::Speakerphone)
                        .map_err(error)?;
                    line.DialWithOptions(&options).map_err(error)?;
                    return self.after_action(Some("Windows accepted the call request, but its audio-routing hint may be ignored. This Windows version cannot confirm PC audio. Check both audio directions before continuing.".into()));
                }
            }
            action => {
                if !before.call_control {
                    return Err("This Windows version does not expose per-call audio transfer or call controls. Use the phone controls for this call.".into());
                }
                let id = match &action {
                    Action::Answer { call_id }
                    | Action::End { call_id }
                    | Action::TakeAudio { call_id }
                    | Action::Mute { call_id, .. } => call_id,
                    Action::Dial { .. } => return Err("Invalid call action.".into()),
                };
                // Never use PhoneCall::GetFromId on an untrusted id. Resolve
                // from live calls on the selected phone's verified lines only.
                let call = self
                    .lines()?
                    .iter()
                    .map(active_calls)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .find(|call| call.CallId().is_ok_and(|value| value == id))
                    .ok_or("That call has ended or belongs to another phone.")?;
                match action {
                    Action::Answer { .. } => {
                        operation(wait(
                            call.AcceptIncomingAsync().map_err(error)?,
                            IO_TIMEOUT,
                        )?)?;
                        if let Err(reason) = set_route(&call, PhoneCallAudioDevice::LocalDevice) {
                            return self.after_action(Some(format!(
                                "Call answered, but PC audio transfer failed: {reason}"
                            )));
                        }
                    }
                    Action::End { .. } => {
                        operation(wait(call.EndAsync().map_err(error)?, IO_TIMEOUT)?)?
                    }
                    Action::TakeAudio { .. } => {
                        set_route(&call, PhoneCallAudioDevice::LocalDevice)?
                    }
                    Action::Mute { muted, .. } => {
                        let op = if muted {
                            call.MuteAsync()
                        } else {
                            call.UnmuteAsync()
                        };
                        operation(wait(op.map_err(error)?, IO_TIMEOUT)?)?;
                    }
                    Action::Dial { .. } => return Err("Invalid call action.".into()),
                }
            }
        }
        self.after_action(None)
    }

    fn after_action(&self, notice: Option<String>) -> Result<Snapshot, String> {
        // A successful dial/answer must not become a retryable error just
        // because the subsequent read failed. Never automatically redial.
        match self.status() {
            Ok(mut status) => {
                status.notice = notice;
                Ok(status)
            }
            Err(reason) => Ok(Snapshot {
                available: true,
                packaged: true,
                device_id: self
                    .transport
                    .as_ref()
                    .and_then(|t| t.DeviceId().ok())
                    .map(|id| id.to_string()),
                notice: Some(format!(
                    "Call operation sent. Could not refresh its state: {reason} Check the existing call before trying another operation."
                )),
                ..Snapshot::default()
            }),
        }
    }

    fn release(&mut self) -> Result<(), String> {
        // Unregistering this app must never hang up the user's cellular call.
        // Windows releases the hands-free service back to the phone.
        if let Some(transport) = &self.transport {
            transport.UnregisterApp().map_err(error)?;
            if transport.IsRegistered().map_err(error)? {
                return Err("Windows has not released the call-audio registration. Check the current call and try Disconnect call audio again.".into());
            }
        }
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.Stop();
        }
        self.transport = None;
        self.ids = Arc::default();
        Ok(())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn devices() -> Result<Vec<Device>, String> {
    let selector = PhoneLineTransportDevice::GetDeviceSelectorForPhoneLineTransport(
        PhoneLineTransport::Bluetooth,
    )
    .map_err(error)?;
    let devices = wait(
        DeviceInformation::FindAllAsyncAqsFilter(&selector).map_err(error)?,
        ENUM_TIMEOUT,
    )?;
    devices
        .into_iter()
        .take(32)
        .map(|device| {
            Ok(Device {
                id: device.Id().map_err(error)?.to_string(),
                name: device.Name().map_err(error)?.to_string(),
            })
        })
        .collect()
}

fn endpoint_name(input: bool) -> Option<String> {
    let id = if input {
        MediaDevice::GetDefaultAudioCaptureId(AudioDeviceRole::Communications)
    } else {
        MediaDevice::GetDefaultAudioRenderId(AudioDeviceRole::Communications)
    }
    .ok()?;
    if id.is_empty() {
        return None;
    }
    wait(
        DeviceInformation::CreateFromIdAsync(&id).ok()?,
        ENUM_TIMEOUT,
    )
    .ok()?
    .Name()
    .ok()
    .map(|name| name.to_string())
}

fn active_calls(line: &PhoneLine) -> Result<Vec<PhoneCall>, String> {
    let result = wait(
        line.GetAllActivePhoneCallsAsync().map_err(error)?,
        ENUM_TIMEOUT,
    )?;
    let status = result.OperationStatus().map_err(error)?;
    if status != PhoneLineOperationStatus::Succeeded {
        return Err(format!(
            "Could not read this phone's live calls ({status:?})."
        ));
    }
    Ok(result
        .AllActivePhoneCalls()
        .map_err(error)?
        .into_iter()
        .take(16)
        .collect())
}

fn set_route(call: &PhoneCall, route: PhoneCallAudioDevice) -> Result<(), String> {
    operation(wait(
        call.ChangeAudioDeviceAsync(route).map_err(error)?,
        IO_TIMEOUT,
    )?)
}

fn operation(status: PhoneCallOperationStatus) -> Result<(), String> {
    if status == PhoneCallOperationStatus::Succeeded {
        Ok(())
    } else {
        Err(format!(
            "Windows call operation failed ({status:?}). Check the current call before retrying."
        ))
    }
}

fn call_state(state: PhoneCallStatus) -> &'static str {
    match state {
        PhoneCallStatus::Incoming => "incoming",
        PhoneCallStatus::Dialing => "dialing",
        PhoneCallStatus::Talking => "talking",
        PhoneCallStatus::Held => "held",
        PhoneCallStatus::Ended => "ended",
        _ => "unknown",
    }
}

fn error(error: impl std::fmt::Display) -> String {
    // The stage and HRESULT are useful; never include a phone number in errors.
    format!("Windows telephony error: {error}")
}

fn wait<T: RuntimeType>(op: IAsyncOperation<T>, timeout: Duration) -> Result<T, String> {
    let started = Instant::now();
    while op.Status().map_err(error)? == AsyncStatus::Started {
        if started.elapsed() >= timeout {
            let _ = op.Cancel();
            return Err(
                "Windows call operation timed out. Check the phone before repeating a call action."
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = op.GetResults().map_err(error);
    drop(op);
    result
}
