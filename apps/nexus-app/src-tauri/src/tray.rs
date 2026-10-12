//! The system-tray / menu-bar icon: live status and one-click connect without opening the window.

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

use crate::dto::{ActiveSession, Device};
use crate::link::AgentLink;

const TRAY_ID: &str = "nexus-tray";

/// "🟢 Windows PC — online": the same dots as the dashboard.
pub fn label(d: &Device) -> String {
    let dot = match d.state {
        "online" | "idle" => "🟢",
        "in_use" => "🔵",
        "locked" => "🟡",
        "sleeping" | "hibernating" => "🟠",
        "offline" => "🔴",
        _ => "⚪",
    };
    format!("{dot} {} — {}", d.name, d.state.replace('_', " "))
}

/// Online devices can be connected to from the menu; the rest are shown but disabled.
pub fn connectable(d: &Device) -> bool {
    d.online && !matches!(d.state, "sleeping" | "hibernating")
}

/// "● Connected now: MacBook, Phone" — shown at the top of the menu while anyone is connected.
pub fn connected_label(sessions: &[ActiveSession]) -> Option<String> {
    if sessions.is_empty() {
        return None;
    }
    let names: Vec<&str> = sessions.iter().map(|s| s.peer_name.as_str()).collect();
    Some(format!("● Connected now: {}", names.join(", ")))
}

fn build_menu(
    app: &AppHandle,
    devices: &[Device],
    sessions: &[ActiveSession],
    lockdown: bool,
) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    if let Some(text) = connected_label(sessions) {
        menu.append(&MenuItem::with_id(app, "who", text, false, None::<&str>)?)?;
        menu.append(&MenuItem::with_id(
            app,
            "disconnect",
            "Disconnect everyone",
            true,
            None::<&str>,
        )?)?;
        menu.append(&PredefinedMenuItem::separator(app)?)?;
    }
    if devices.is_empty() {
        menu.append(&MenuItem::with_id(
            app,
            "none",
            "No paired devices",
            false,
            None::<&str>,
        )?)?;
    }
    let mut sorted: Vec<&Device> = devices.iter().collect();
    sorted.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    for d in sorted {
        menu.append(&MenuItem::with_id(
            app,
            format!("dev:{}", d.id),
            label(d),
            connectable(d),
            None::<&str>,
        )?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "open",
        "Open Nexus Remote",
        true,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "lockdown",
        if lockdown {
            "Turn lockdown off"
        } else {
            "Lockdown: block all remote access"
        },
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)?;
    Ok(menu)
}

/// Creates the icon; returns false if the platform has no tray (the window then must not hide on close).
pub fn build(app: &AppHandle, link: &Arc<AgentLink>) -> bool {
    let Ok(menu) = build_menu(app, &link.devices(), &link.sessions(), link.lockdown()) else {
        return false;
    };
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Nexus Remote")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref().to_owned();
            let link = app.state::<Arc<AgentLink>>().inner().clone();
            match id.as_str() {
                "open" => AgentLink::show_window(app),
                "quit" => app.exit(0),
                "disconnect" => {
                    tauri::async_runtime::spawn(async move {
                        if let Ok(c) = link.client() {
                            let _ = c.disconnect_sessions(Vec::new()).await;
                        }
                    });
                }
                "lockdown" => {
                    let on = !link.lockdown();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Ok(c) = link.client() {
                            let _ = c.set_lockdown(on).await;
                            let _ = link.snapshot().await;
                            refresh(&app, &link);
                        }
                    });
                }
                other => {
                    if let Some(dev) = other.strip_prefix("dev:")
                        && let Some(d) = link.devices().into_iter().find(|d| d.id == dev)
                    {
                        let _ = crate::commands::spawn_viewer(&d.name);
                    }
                }
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app).is_ok()
}

/// Rebuilds the menu from the latest device list.
pub fn refresh(app: &AppHandle, link: &Arc<AgentLink>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let sessions = link.sessions();
    let tip = if sessions.is_empty() {
        "Nexus Remote".to_owned()
    } else {
        format!("Nexus Remote — {} connected", sessions.len())
    };
    let _ = tray.set_tooltip(Some(tip));
    if let Ok(menu) = build_menu(app, &link.devices(), &link.sessions(), link.lockdown()) {
        let _ = tray.set_menu(Some(menu));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(state: &'static str, online: bool) -> Device {
        Device {
            id: "01".into(),
            name: "PC".into(),
            state,
            online,
            last_seen_ms: 0,
            os_family: "linux",
            os_version: String::new(),
            path: "lan",
            active_user: String::new(),
            battery_percent: None,
            fingerprint: String::new(),
            unattended: false,
            grants_on_peer: vec![],
        }
    }

    #[test]
    fn the_menu_names_whoever_is_connected() {
        assert_eq!(connected_label(&[]), None);
        let s = |n: &str| ActiveSession {
            id: 1,
            device_id: String::new(),
            peer_name: n.into(),
            started_ms: 0,
            path: "lan",
            grants: vec![],
        };
        assert_eq!(
            connected_label(&[s("MacBook"), s("Phone")]).as_deref(),
            Some("● Connected now: MacBook, Phone")
        );
    }

    #[test]
    fn dots_and_connectability() {
        assert_eq!(label(&d("in_use", true)), "🔵 PC — in use");
        assert_eq!(label(&d("sleeping", false)), "🟠 PC — sleeping");
        assert_eq!(label(&d("weird", false)), "⚪ PC — weird");
        assert!(connectable(&d("idle", true)));
        assert!(!connectable(&d("sleeping", true)));
        assert!(!connectable(&d("offline", false)));
    }
}
