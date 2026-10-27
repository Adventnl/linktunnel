//! What the person at this machine is told when someone connects, and the panic switch.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;

use crate::dto::{ActiveSession, AppEvent};
use crate::link::{self, AgentLink};

/// Ctrl/Cmd + Alt + Shift + L: lock everyone out, wherever the focus is.
pub fn panic_shortcut() -> Shortcut {
    Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::KeyL,
    )
}

/// Sessions in `now` that were not in `before`.
pub fn arrivals<'a>(before: &[ActiveSession], now: &'a [ActiveSession]) -> Vec<&'a ActiveSession> {
    now.iter()
        .filter(|s| !before.iter().any(|b| b.id == s.id))
        .collect()
}

/// "Alex's MacBook is now connected (screen, keyboard)".
pub fn arrival_text(s: &ActiveSession) -> String {
    if s.grants.is_empty() {
        format!("{} is now connected to this computer", s.peer_name)
    } else {
        format!(
            "{} is now connected to this computer ({})",
            s.peer_name,
            s.grants.join(", ")
        )
    }
}

/// Tells the person about every new connection. The tray and dashboard banner are the persistent
/// indicator; this is the nudge for someone who is not looking at either.
pub fn sessions_changed(app: &AppHandle, before: &[ActiveSession], now: &[ActiveSession]) {
    for s in arrivals(before, now) {
        let _ = app
            .notification()
            .builder()
            .title("linktunnel")
            .body(arrival_text(s))
            .show();
    }
}

/// Lockdown on and everyone out. Order matters: lockdown first, so nobody can reconnect between
/// the two steps.
pub async fn panic_now(app: &AppHandle, link: &Arc<AgentLink>) -> Result<(), String> {
    let client = link.client()?;
    client.set_lockdown(true).await.map_err(|e| e.to_string())?;
    client
        .disconnect_sessions(Vec::new())
        .await
        .map_err(|e| e.to_string())?;
    let snap = link.snapshot().await;
    crate::tray::refresh(app, link);
    let _ = app.emit(link::EVENT, AppEvent::Snapshot { snapshot: snap });
    Ok(())
}

/// Registers the panic shortcut. Failing (another app owns the keys, no display server) is not
/// fatal: the tray item and the CLI do the same thing.
pub fn register_panic_hotkey(app: &AppHandle) {
    let shortcut = panic_shortcut();
    let res = app
        .global_shortcut()
        .on_shortcut(shortcut, |app, _shortcut, event| {
            if event.state != ShortcutState::Pressed {
                return;
            }
            let app = app.clone();
            let link = app.state::<Arc<AgentLink>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                match panic_now(&app, &link).await {
                    Ok(()) => {
                        let _ = app
                            .notification()
                            .builder()
                            .title("linktunnel")
                            .body("Lockdown is on and everyone was disconnected.")
                            .show();
                    }
                    Err(e) => tracing::warn!("panic hotkey: {e}"),
                }
            });
        });
    if let Err(e) = res {
        tracing::warn!("could not register the panic hotkey: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: u64, name: &str, grants: &[&str]) -> ActiveSession {
        ActiveSession {
            id,
            device_id: "ab".into(),
            peer_name: name.into(),
            started_ms: 0,
            path: "lan",
            grants: grants.iter().map(|g| (*g).to_owned()).collect(),
        }
    }

    #[test]
    fn only_new_sessions_are_announced() {
        let before = vec![s(1, "A", &[])];
        let now = vec![s(1, "A", &[]), s(2, "B", &["screen"])];
        let new = arrivals(&before, &now);
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].peer_name, "B");
        assert!(arrivals(&now, &before).is_empty(), "leaving is silent");
        assert!(arrivals(&[], &[]).is_empty());
    }

    #[test]
    fn the_text_names_who_and_what() {
        assert_eq!(
            arrival_text(&s(1, "MacBook", &["screen", "terminal"])),
            "MacBook is now connected to this computer (screen, terminal)"
        );
        assert_eq!(arrival_text(&s(1, "X", &[])), "X is now connected to this computer");
    }
}
