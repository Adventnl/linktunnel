//! The agent binary must start in hostile-but-realistic environments.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A LAN-only agent (no relay configured) needs no TLS roots: an unreadable `SSL_CERT_FILE`, as on
/// a minimal container or under a service account, must not stop it starting.
#[test]
fn the_agent_starts_without_readable_ca_roots_when_no_relay_is_configured() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_nexus-agent"))
        .args(["--home"])
        .arg(home.path())
        .args(["--file-store", "--no-screen"])
        .env("SSL_CERT_FILE", "/nonexistent/ca-bundle.pem")
        .env("SSL_CERT_DIR", "/nonexistent")
        .env_remove("NEXUS_SIGNAL_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The IPC token appears once the agent is fully up.
    let token = home.path().join("run").join("agent.token");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut up = false;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if token.exists()
            || std::fs::read_dir(home.path()).is_ok_and(|_| find(home.path(), "agent.token"))
        {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let exited = child.try_wait().unwrap();
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();
    assert!(
        up && exited.is_none(),
        "the agent did not start: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn find(dir: &std::path::Path, name: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let p = e.path();
            p.file_name().is_some_and(|n| n == name) || (p.is_dir() && find(&p, name))
        })
    })
}
