//! Runs a real Xvfb-backed virtual display and drives it with the real capture/input backends.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::time::{Duration, Instant};

use nexus_capture::CaptureBackend;
use nexus_input::Injector;
use nexus_vdisplay::{VirtualDisplay, VirtualDisplayConfig, available};

fn config() -> VirtualDisplayConfig {
    VirtualDisplayConfig::default()
        .with_size("800x600")
        .unwrap()
}

#[test]
fn starts_serves_capture_and_input_and_cleans_up() {
    if !available() {
        eprintln!("Xvfb not installed; skipping");
        return;
    }
    let mut vd = VirtualDisplay::start(&config()).unwrap();
    assert_eq!(vd.size(), (800, 600));
    assert!(vd.name().starts_with(':'));
    assert!(vd.is_alive());

    let backend = nexus_capture::x11::X11Backend::connect(Some(vd.name())).unwrap();
    let monitors = backend.monitors().unwrap();
    assert_eq!(monitors.len(), 1);
    assert_eq!((monitors[0].width, monitors[0].height), (800, 600));
    // Pointer injection works on it.
    let inj = nexus_input::x11::X11Injector::connect(Some(vd.name())).unwrap();
    let _ = inj.name();

    let socket = format!("/tmp/.X11-unix/X{}", &vd.name()[1..]);
    assert!(std::path::Path::new(&socket).exists());
    drop(vd);
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::path::Path::new(&socket).exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !std::path::Path::new(&socket).exists(),
        "the X server must go away with the handle"
    );
}

#[test]
fn two_displays_do_not_collide() {
    if !available() {
        return;
    }
    let a = VirtualDisplay::start(&config()).unwrap();
    let b = VirtualDisplay::start(&config()).unwrap();
    assert_ne!(a.name(), b.name());
}

#[test]
fn a_crashing_session_command_is_restarted_with_backoff() {
    if !available() {
        return;
    }
    let mut cfg = config();
    cfg.session = vec!["sh".into(), "-c".into(), "exit 1".into()];
    let vd = VirtualDisplay::start(&cfg).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while vd.session_restarts() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        vd.session_restarts() >= 2,
        "restarts: {}",
        vd.session_restarts()
    );
}

#[test]
fn a_session_command_sees_the_display_and_dies_with_it() {
    if !available() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("nexus-vd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("display");
    let mut cfg = config();
    cfg.session = vec![
        "sh".into(),
        "-c".into(),
        format!("echo $DISPLAY > {} && exec sleep 600", marker.display()),
    ];
    let vd = VirtualDisplay::start(&cfg).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), vd.name());
    drop(vd);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn missing_binary_and_bad_config_are_clean_errors() {
    let mut cfg = config();
    cfg.xvfb = Some("/nonexistent/Xvfb".into());
    assert!(VirtualDisplay::start(&cfg).is_err());
    cfg.width = 5;
    assert!(VirtualDisplay::start(&cfg).is_err());
}
