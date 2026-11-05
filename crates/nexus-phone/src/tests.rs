#![allow(clippy::print_stdout, clippy::print_stderr)]
use super::*;
use std::io::Write;

fn options() -> Options {
    Options {
        serial: "abc123".into(),
        screen_off: true,
        media_audio: false,
        max_size: 1600,
        max_fps: 60,
    }
}

#[test]
fn options_target_one_phone_and_keep_media_distinct_from_calls() {
    let args = options().args().unwrap();
    assert!(args.contains(&"--serial=abc123".into()));
    assert!(args.contains(&"--turn-screen-off".into()));
    assert!(args.contains(&"--no-audio".into()));
    assert!(
        !args
            .iter()
            .any(|s| s.contains("mic") || s.contains("voice-call") || s.contains("tcpip"))
    );
    let mut opt = options();
    opt.screen_off = false;
    opt.media_audio = true;
    let args = opt.args().unwrap();
    assert!(!args.contains(&"--turn-screen-off".into()));
    assert!(!args.contains(&"--no-audio".into()));
    opt.max_fps = 0;
    assert!(opt.args().is_err());
    opt.max_fps = 60;
    opt.max_size = u16::MAX;
    assert!(opt.args().is_err());
}

#[test]
fn rejects_flags_commands_and_ambiguous_network_addresses() {
    for address in [
        "--help",
        "192.168.1.2",
        "phone:5555",
        "1.2.3.4:0",
        "0.0.0.0:5555",
        "224.0.0.1:5555",
        "1.2.3.4:5555;whoami",
        "1.2.3.4:99999",
    ] {
        assert!(validate_address(address).is_err(), "{address}");
    }
    for address in ["192.168.1.20:37123", "[2001:db8::1]:37123"] {
        assert!(validate_address(address).is_ok());
    }
    for serial in ["", "--tcpip=evil", "abc\nxyz", "abc;whoami", "$(whoami)"] {
        let mut opt = options();
        opt.serial = serial.into();
        assert!(opt.args().is_err());
    }
}

#[test]
fn lists_all_transports_without_treating_unauthorised_as_ready() {
    let devices = parse_devices("* daemon started successfully *\nList of devices attached\nabc device product:x model:Redmi_Note_14_Pro transport_id:1\n192.168.1.20:37123 offline\nxyz unauthorized usb:1\n\n").unwrap();
    assert_eq!(devices.len(), 3);
    assert_eq!(devices[0].model, "Redmi Note 14 Pro");
    assert_eq!(devices[1].serial, "192.168.1.20:37123");
    assert_eq!(devices[2].state, "unauthorized");
    assert!(parse_devices(&"a".repeat((1 << 20) + 1)).is_err());
}

#[test]
fn requires_a_supported_scrcpy_version() {
    assert!(
        check_version("scrcpy 3.3 <https://github.com/Genymobile/scrcpy>\nDependencies:").is_ok()
    );
    assert!(check_version("scrcpy 4.0").is_ok());
    for version in [
        "scrcpy 1.25",
        "scrcpy 3.2",
        "random executable",
        "scrcpy nope",
    ] {
        assert!(check_version(version).is_err());
    }
}

#[test]
fn recognises_both_scrcpy_startup_formats_but_not_idle_or_setup_logs() {
    for line in [
        "INFO: Texture: 1080x2400",
        "INFO: Texture (D3D11VA): 720x1600",
        "INFO: 1 fps",
        "INFO: 60 fps (+2 frames skipped)\r",
    ] {
        assert!(process::frame_received(line.as_bytes()));
    }
    for line in [
        "INFO: 0 fps",
        "INFO: FPS counter started",
        "INFO: Renderer: direct3d",
        "[server] INFO: Device: Redmi",
        "INFO: fps",
        "INFO: -1 fps",
    ] {
        assert!(!process::frame_received(line.as_bytes()));
    }
}

#[tokio::test]
async fn stop_cancels_a_start_waiting_for_the_session_lock() {
    let phone = Phone::default();
    let guard = phone.managed.lock().await;
    let mut start = Box::pin(phone.start(options()));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut start)
            .await
            .is_err()
    );
    drop(guard);
    let (stopped, started) = tokio::join!(phone.stop(), start);
    stopped.unwrap();
    assert!(started.unwrap_err().to_string().contains("cancelled"));
}

#[tokio::test]
async fn rejects_bad_pairing_codes_before_launching_tools() {
    let phone = Phone::default();
    for code in ["", "12345", "1234567", "12345x", "１２３４５６", "12\n456"] {
        assert!(matches!(
            phone.pair("192.168.1.20:37123", code).await,
            Err(Error::Invalid(_))
        ));
    }
}

proptest::proptest! {
    #[test]
    fn parsers_never_panic(text in ".{0,2000}") {
        let _ = parse_devices(&text);
        let _ = validate_address(&text);
        let _ = validate_serial(&text);
        let _ = check_version(&text);
    }
}

fn fixture(kind: &str) -> Command {
    let mut command = process::command(&std::env::current_exe().unwrap());
    command
        .args(["--exact", "tests::tool_fixture", "--nocapture"])
        .env("NEXUS_PHONE_TEST_FIXTURE", kind);
    command
}

/// Runs as a subprocess only when NEXUS_PHONE_TEST_FIXTURE is set. No shell or
/// external runtime dependency; exercises real pipes on Windows, macOS and Linux.
#[test]
fn tool_fixture() {
    let Ok(kind) = std::env::var("NEXUS_PHONE_TEST_FIXTURE") else {
        return;
    };
    match kind.as_str() {
        "ready" => {
            eprint!("INFO: Tex");
            std::io::stderr().flush().unwrap();
            std::thread::sleep(Duration::from_millis(20));
            eprintln!("ture: 1080x2400");
        }
        "flood" => {
            std::io::stdout()
                .write_all(&vec![b'a'; (1 << 20) + 100])
                .unwrap();
        }
        "logs" => {
            std::io::stderr().write_all(&vec![b'x'; 20000]).unwrap();
            eprintln!("\nINFO: Texture: 1080x2400");
        }
        "fail" => {
            eprintln!("ERROR: Device is unauthorized");
            std::process::exit(1);
        }
        "server-fail-hang" => eprintln!("ERROR: Server connection failed"),
        "silent-fail" => std::process::exit(1),
        "empty" => return,
        _ => {}
    }
    std::thread::sleep(Duration::from_secs(20));
}

#[tokio::test]
async fn startup_requires_a_picture_not_a_spawned_process() {
    for kind in ["fail", "empty"] {
        let error = process::viewer(&mut fixture(kind), Duration::from_secs(5))
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("before receiving a picture"));
    }
    let (mut child, _) = process::viewer(&mut fixture("ready"), Duration::from_secs(5))
        .await
        .unwrap();
    assert!(child.try_wait().unwrap().is_none());
    child.kill().await.unwrap();
}

#[tokio::test]
async fn startup_times_out_and_bounds_diagnostics() {
    let error = process::viewer(&mut fixture("server-fail-hang"), Duration::from_secs(5))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("Could not start the phone viewer"));
    let error = process::viewer(&mut fixture("hang"), Duration::from_millis(250))
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("Timed out"));
    let (mut child, logs) = process::viewer(&mut fixture("logs"), Duration::from_secs(5))
        .await
        .unwrap();
    assert!(logs.tail().len() <= 8192);
    child.kill().await.unwrap();
}

#[cfg(windows)]
#[test]
fn bundled_tools_strip_verbatim_paths_before_passing_them_to_c_tools() {
    let phone = Phone::with_tools(Some(Path::new(
        r"\\?\C:\Program Files\Link Tunnel\phone-tools",
    )));
    assert_eq!(
        phone.scrcpy,
        Path::new(r"C:\Program Files\Link Tunnel\phone-tools\scrcpy.exe")
    );
    let command = phone.scrcpy();
    assert!(
        command
            .as_std()
            .get_envs()
            .all(|(_, value)| value.is_none_or(|v| !v.to_string_lossy().starts_with(r"\\?\")))
    );
}

#[tokio::test]
async fn finite_commands_fail_on_hangs_floods_and_nonzero_exit() {
    assert!(
        process::output(&mut fixture("silent-fail"), Duration::from_secs(5))
            .await
            .unwrap_err()
            .to_string()
            .contains("Phone tool exited")
    );
    assert!(
        process::output(&mut fixture("hang"), Duration::from_millis(250))
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    assert!(
        process::output(&mut fixture("flood"), Duration::from_secs(5))
            .await
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
    assert!(
        process::output(&mut fixture("fail"), Duration::from_secs(5))
            .await
            .unwrap_err()
            .to_string()
            .contains("unauthorized")
    );
}

#[tokio::test]
async fn closed_or_failed_viewers_clear_active_session_and_stop_is_idempotent() {
    let mut child = process::spawn(&mut fixture("fail")).unwrap();
    child.wait().await.unwrap();
    let mut managed = Managed {
        child: Some(child),
        logs: None,
        session: Session {
            serial: Some("abc123".into()),
            error: None,
            ..Session::default()
        },
        ..Managed::default()
    };
    managed.refresh().unwrap();
    assert!(managed.child.is_none());
    assert!(managed.session.serial.is_none());
    assert!(managed.session.error.is_some());
    let phone = Phone::default();
    phone.stop().await.unwrap();
    phone.stop().await.unwrap();
    assert!(phone.session().await.serial.is_none());
}

#[tokio::test]
async fn session_queries_do_not_wait_behind_startup() {
    let phone = Phone::default();
    let _startup = phone.managed.lock().await;
    let session = tokio::time::timeout(Duration::from_millis(100), phone.session())
        .await
        .unwrap();
    assert!(session.starting);
    assert!(session.serial.is_none());
}

/// Opt-in hardware check: NEXUS_TEST_PHONE names one already authorised transport.
#[tokio::test]
#[ignore = "requires an explicitly selected, authorised Android phone"]
async fn real_phone_reports_first_picture() {
    let serial = std::env::var("NEXUS_TEST_PHONE").expect("set NEXUS_TEST_PHONE");
    let phone = if let Ok(root) = std::env::var("NEXUS_TEST_PHONE_TOOLS") {
        // Exercise the exact canonicalized resource path Tauri supplies on Windows.
        Phone::bundled(std::fs::canonicalize(root).unwrap())
    } else {
        Phone::default()
    };
    let started = std::time::Instant::now();
    phone
        .start(Options {
            serial: serial.clone(),
            screen_off: false,
            ..options()
        })
        .await
        .unwrap();
    let session = phone.session().await;
    assert_eq!(session.serial.as_deref(), Some(serial.as_str()));
    assert!(started.elapsed() < Duration::from_secs(40));
    #[cfg(windows)]
    assert!(session.width > 0 && session.height > 0);
    println!(
        "First phone picture confirmed in {:?} ({}x{})",
        started.elapsed(),
        session.width,
        session.height
    );
    phone.stop().await.unwrap();
}
