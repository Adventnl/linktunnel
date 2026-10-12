//! A screen-less Linux machine gets a virtual screen and advertises video for it.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use nexus_agent::RemoteHandler;
use nexus_agent::SessionHandler;
use nexus_core::config::{DisplayConfig, VirtualScreen};

fn offers_video(h: &RemoteHandler) -> bool {
    !h.capabilities().video_encode.is_empty()
}

#[test]
fn headless_machines_get_a_virtual_screen_and_never_means_never() {
    if !nexus_vdisplay::available() {
        eprintln!("Xvfb not installed; skipping");
        return;
    }
    if std::env::var_os("DISPLAY").is_some() {
        eprintln!("a real DISPLAY is set; this test needs a screen-less environment; skipping");
        return;
    }
    let mut cfg = DisplayConfig {
        size: "1024x768".into(),
        ..DisplayConfig::default()
    };
    // Auto: nothing to capture → virtual screen.
    let h = RemoteHandler::detect_with_display(30, &cfg);
    assert!(offers_video(&h), "auto should create a virtual screen");
    drop(h);
    // Never: stays screen-less.
    cfg.virtual_screen = VirtualScreen::Never;
    let h = RemoteHandler::detect_with_display(30, &cfg);
    assert!(!offers_video(&h));
    // Always: virtual even though detection was skipped.
    cfg.virtual_screen = VirtualScreen::Always;
    assert!(offers_video(&RemoteHandler::detect_with_display(30, &cfg)));
    // A bad size is a logged failure, not a crash; the machine just has no screen.
    cfg.size = "oops".into();
    assert!(!offers_video(&RemoteHandler::detect_with_display(30, &cfg)));
}
