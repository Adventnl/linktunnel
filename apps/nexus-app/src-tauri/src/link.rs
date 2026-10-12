//! The connection to the local agent: reconnects forever, forwards its events to the UI.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nexus_core::{Paths, Scope};
use nexus_ipc::{IpcClient, IpcPaths};
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, Manager};

use crate::dto::{self, ActiveSession, AgentStatus, AppEvent, Device, Snapshot};

/// The one event channel the UI listens on.
pub const EVENT: &str = "nexus://event";

/// Seconds between attempts to reach an agent that is not running.
const RETRY: Duration = Duration::from_millis(1500);

#[derive(Default)]
struct Cache {
    devices: Vec<Device>,
    sessions: Vec<ActiveSession>,
    lockdown: bool,
}

pub struct AgentLink {
    paths: IpcPaths,
    client: Mutex<Option<IpcClient>>,
    /// Terminal id → the control-only session carrying it (closed with the terminal).
    pub terminals: Mutex<HashMap<u64, u64>>,
    cache: Mutex<Cache>,
    /// Whether closing the window should only hide it (a tray icon exists to bring it back).
    pub tray_ok: Mutex<bool>,
}

pub fn agent_paths() -> IpcPaths {
    let paths = match std::env::var_os("NEXUS_HOME") {
        Some(home) => Paths::under(&PathBuf::from(home)),
        None => Paths::resolve(Scope::User)
            .unwrap_or_else(|_| Paths::under(&std::env::temp_dir().join("nexus"))),
    };
    IpcPaths::in_dir(&paths.runtime_dir)
}

impl AgentLink {
    pub fn new(paths: IpcPaths) -> Arc<Self> {
        Arc::new(Self {
            paths,
            client: Mutex::new(None),
            terminals: Mutex::new(HashMap::new()),
            cache: Mutex::new(Cache::default()),
            tray_ok: Mutex::new(false),
        })
    }

    /// Starts the background task that keeps the connection alive.
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        let link = Arc::clone(self);
        tauri::async_runtime::spawn(async move { link.run(app).await });
    }

    /// The live client, or why there is none.
    pub fn client(&self) -> Result<IpcClient, String> {
        self.client
            .lock()
            .clone()
            .ok_or_else(|| "the Nexus agent is not running".to_owned())
    }

    /// Devices as last seen (for the tray menu).
    pub fn devices(&self) -> Vec<Device> {
        self.cache.lock().devices.clone()
    }

    pub fn lockdown(&self) -> bool {
        self.cache.lock().lockdown
    }

    /// Who is connected to this machine, as last reported.
    pub fn sessions(&self) -> Vec<ActiveSession> {
        self.cache.lock().sessions.clone()
    }

    pub async fn snapshot(&self) -> Snapshot {
        let client = match self.client() {
            Ok(c) => c,
            Err(e) => return Snapshot::unreachable(e),
        };
        match tokio::try_join!(client.status(), client.list_devices(), client.list_sessions()) {
            Ok((status, list, sessions)) => {
                let devices: Vec<Device> = list.devices.iter().map(Device::from).collect();
                let sessions: Vec<ActiveSession> =
                    sessions.sessions.iter().map(ActiveSession::from).collect();
                let status = AgentStatus::from(&status);
                let mut cache = self.cache.lock();
                cache.devices.clone_from(&devices);
                cache.sessions.clone_from(&sessions);
                cache.lockdown = status.lockdown;
                Snapshot {
                    agent: "connected",
                    status: Some(status),
                    devices,
                    sessions,
                    error: None,
                }
            }
            Err(e) => Snapshot::unreachable(e.to_string()),
        }
    }

    async fn run(self: Arc<Self>, app: AppHandle) {
        let mut was_connected = false;
        loop {
            match IpcClient::connect(&self.paths, "nexus-app").await {
                Ok((client, mut events, _welcome)) => {
                    *self.client.lock() = Some(client);
                    was_connected = true;
                    let snap = self.snapshot().await;
                    crate::tray::refresh(&app, &self);
                    let _ = app.emit(EVENT, AppEvent::Snapshot { snapshot: snap });
                    while let Some(ev) = events.next().await {
                        let Some(app_ev) = dto::translate(&ev, now_ms()) else {
                            continue;
                        };
                        let before = self.sessions();
                        self.apply(&app_ev);
                        let _ = app.emit(EVENT, &app_ev);
                        if let AppEvent::SessionsChanged { sessions } = &app_ev {
                            crate::alert::sessions_changed(&app, &before, sessions);
                        }
                        if matches!(
                            app_ev,
                            AppEvent::DeviceChanged { .. }
                                | AppEvent::DeviceRemoved { .. }
                                | AppEvent::SessionsChanged { .. }
                        ) {
                            crate::tray::refresh(&app, &self);
                        }
                    }
                    *self.client.lock() = None;
                    self.terminals.lock().clear();
                    {
                        let mut cache = self.cache.lock();
                        cache.devices.clear();
                        cache.sessions.clear();
                    }
                    crate::tray::refresh(&app, &self);
                    let _ = app.emit(
                        EVENT,
                        AppEvent::Agent {
                            agent: "unreachable",
                            error: Some("the agent stopped".into()),
                        },
                    );
                }
                Err(e) => {
                    if was_connected || !self.cache.lock().devices.is_empty() {
                        was_connected = false;
                    }
                    let _ = app.emit(
                        EVENT,
                        AppEvent::Agent {
                            agent: "unreachable",
                            error: Some(e.to_string()),
                        },
                    );
                }
            }
            tokio::time::sleep(RETRY).await;
        }
    }

    fn apply(&self, ev: &AppEvent) {
        let mut cache = self.cache.lock();
        match ev {
            AppEvent::DeviceChanged { device } => {
                match cache.devices.iter_mut().find(|d| d.id == device.id) {
                    Some(slot) => *slot = device.clone(),
                    None => cache.devices.push(device.clone()),
                }
            }
            AppEvent::DeviceRemoved { id } => cache.devices.retain(|d| &d.id != id),
            AppEvent::SessionsChanged { sessions } => cache.sessions.clone_from(sessions),
            _ => {}
        }
    }

    /// Shows or focuses the main window.
    pub fn show_window(app: &AppHandle) {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The `nexus` CLI next to this app (installed together), else whatever is on `PATH`.
pub fn nexus_cli() -> PathBuf {
    let name = if cfg!(windows) { "nexus.exe" } else { "nexus" };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(name)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}
