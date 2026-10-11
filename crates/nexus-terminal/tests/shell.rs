//! Real shells on a real PTY, driven through the in-memory channel.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use nexus_proto::v1::TermOpen;
use nexus_terminal::{
    HostConfig, TermCmd, TermEvent, TerminalHost, memory_pair, run_client, serve,
};
use tokio::sync::mpsc;

struct Term {
    cmds: mpsc::UnboundedSender<TermCmd>,
    events: mpsc::Receiver<TermEvent>,
    seen: String,
}

fn open(name: &str) -> TermOpen {
    TermOpen {
        cols: 100,
        rows: 30,
        shell: "/bin/sh".into(),
        name: name.into(),
        ..Default::default()
    }
}

fn host() -> Arc<TerminalHost> {
    TerminalHost::new(HostConfig {
        allowed_shells: vec!["/bin/sh".into()],
        default_shell: Some("/bin/sh".into()),
        ..Default::default()
    })
}

async fn attach(host: &Arc<TerminalHost>, req: TermOpen) -> Term {
    let (client_io, mut server_io) = memory_pair();
    let h = Arc::clone(host);
    tokio::spawn(async move {
        let _ = serve(&h, &mut server_io).await;
    });
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::channel(256);
    tokio::spawn(async move {
        let _ = run_client(client_io, req, cmd_rx, ev_tx).await;
    });
    Term {
        cmds: cmd_tx,
        events: ev_rx,
        seen: String::new(),
    }
}

impl Term {
    fn type_line(&self, line: &str) {
        self.cmds
            .send(TermCmd::Input(Bytes::from(format!("{line}\n"))))
            .unwrap();
    }

    /// Reads until the accumulated output contains `needle`.
    async fn until(&mut self, needle: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !self.seen.contains(needle) {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, self.events.recv()).await {
                Ok(Some(TermEvent::Output(b))) => self.seen.push_str(&String::from_utf8_lossy(&b)),
                Ok(Some(TermEvent::Opened { .. })) => {}
                Ok(Some(other)) => panic!(
                    "unexpected {other:?} while waiting for {needle:?}; saw {:?}",
                    self.seen
                ),
                Ok(None) | Err(_) => {
                    panic!("timed out waiting for {needle:?}; saw {:?}", self.seen)
                }
            }
        }
    }

    async fn event(&mut self) -> TermEvent {
        tokio::time::timeout(Duration::from_secs(10), self.events.recv())
            .await
            .expect("an event")
            .expect("open channel")
    }
}

#[tokio::test]
async fn a_shell_runs_commands_and_reports_its_exit_code() {
    let host = host();
    let mut t = attach(&host, open("")).await;
    t.type_line("echo hello-$((6*7))");
    t.until("hello-42").await;
    t.type_line("exit 3");
    loop {
        match t.event().await {
            TermEvent::Exited(code) => {
                assert_eq!(code, 3);
                break;
            }
            TermEvent::Output(_) | TermEvent::Opened { .. } => {}
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn the_window_size_reaches_the_shell() {
    let host = host();
    let mut t = attach(&host, open("")).await;
    t.type_line("stty size");
    t.until("30 100").await;
    t.cmds
        .send(TermCmd::Resize {
            cols: 132,
            rows: 43,
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    t.type_line("stty size");
    t.until("43 132").await;
}

#[tokio::test]
async fn ephemeral_shells_die_with_the_connection() {
    let host = host();
    let t = attach(&host, open("")).await;
    t.type_line("echo up");
    for _ in 0..100 {
        if host.session_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(host.session_count(), 1);
    t.cmds.send(TermCmd::Detach).unwrap();
    for _ in 0..100 {
        if host.session_count() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the shell outlived its connection");
}

#[tokio::test]
async fn a_named_shell_survives_a_disconnect_and_replays_its_output() {
    let host = host();
    let mut a = attach(&host, open("build")).await;
    a.type_line("echo started-the-long-job; export JOB=42");
    a.until("started-the-long-job").await;
    a.cmds.send(TermCmd::Detach).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(host.names(), ["build"]);
    assert_eq!(host.session_count(), 1);

    let mut b = attach(&host, open("build")).await;
    // Earlier output comes back, and the shell state (an exported variable) is still there.
    b.until("started-the-long-job").await;
    b.type_line("echo job=$JOB");
    b.until("job=42").await;
    match b.events.try_recv() {
        Ok(_) | Err(_) => {}
    }

    // Killing it ends the session for good.
    b.cmds.send(TermCmd::Kill).unwrap();
    loop {
        if let TermEvent::Exited(_) = b.event().await {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    host.reap();
    assert!(host.names().is_empty());
}

#[tokio::test]
async fn a_second_controller_takes_over_a_named_terminal() {
    let host = host();
    let mut first = attach(&host, open("shared")).await;
    first.type_line("echo mine");
    first.until("mine").await;
    let mut second = attach(&host, open("shared")).await;
    second.type_line("echo taken");
    second.until("taken").await;
    loop {
        match first.event().await {
            TermEvent::Failed(why) => {
                assert!(why.contains("taken over"), "{why}");
                break;
            }
            TermEvent::Output(_) => {}
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn policy_refusals_are_reported_not_crashed_on() {
    let host = host();
    let mut bad_shell = attach(
        &host,
        TermOpen {
            shell: "/bin/bash".into(),
            ..open("")
        },
    )
    .await;
    match bad_shell.event().await {
        TermEvent::Failed(why) => assert!(why.contains("not an allowed shell"), "{why}"),
        other => panic!("{other:?}"),
    }
    let mut missing = attach(
        &host,
        TermOpen {
            reattach_only: true,
            ..open("ghost")
        },
    )
    .await;
    match missing.event().await {
        TermEvent::Failed(why) => assert!(why.contains("no terminal named ghost"), "{why}"),
        other => panic!("{other:?}"),
    }
    let small = TerminalHost::new(HostConfig {
        max_sessions: 1,
        default_shell: Some("/bin/sh".into()),
        ..Default::default()
    });
    let _one = attach(&small, open("a")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut two = attach(&small, open("b")).await;
    match two.event().await {
        TermEvent::Failed(why) => assert!(why.contains("too many"), "{why}"),
        other => panic!("{other:?}"),
    }
}
