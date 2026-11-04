//! Exercise the built CLI through authenticated IPC, with deterministic agent replies.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::process::Stdio;
use std::time::Duration;

use nexus_core::Paths;
use nexus_ipc::{IpcConnection, IpcPaths, IpcServer};
use nexus_proto::v1::{
    DeviceList, DeviceView, IpcAck, IpcEvent, IpcFrame, IpcResponse, IpcWelcome, SessionOpened,
    TerminalEnded, TerminalOutput, TerminalStarted, ipc_event::Body as Ev,
    ipc_frame::Body as Frame, ipc_request::Body as Req, ipc_response::Body as Resp,
};
use tokio::io::AsyncWriteExt;

async fn event(conn: &mut IpcConnection, body: Ev) {
    conn.send(&IpcFrame {
        body: Some(Frame::Event(IpcEvent { body: Some(body) })),
    })
    .await
    .unwrap();
}

async fn exercise(mode: &str, args: &[&str], input: &[u8]) -> std::process::Output {
    // Short socket path: macOS limits sockaddr_un to 104 bytes.
    let home = tempfile::Builder::new()
        .prefix("nt-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let paths = Paths::under(home.path());
    let mut server = IpcServer::bind(IpcPaths::in_dir(&paths.runtime_dir))
        .await
        .unwrap();
    let mode = mode.to_owned();
    let agent = tokio::spawn(async move {
        let (mut conn, _) = server
            .accept()
            .await
            .unwrap()
            .authenticate(IpcWelcome::default())
            .await
            .unwrap();
        let mut input = Vec::new();
        let mut closed = false;
        while let Some(frame) = conn.recv().await.unwrap() {
            let Some(Frame::Request(req)) = frame.body else {
                continue;
            };
            let mut after = false;
            let body = match req.body.unwrap() {
                Req::ListDevices(_) => Resp::Devices(DeviceList {
                    devices: vec![DeviceView {
                        name: "Henrik server".into(),
                        device_id: vec![1; 32].into(),
                        online: true,
                        ..Default::default()
                    }],
                }),
                Req::OpenSession(req) => {
                    assert!(!req.want_video);
                    assert_eq!(req.device_id.as_ref(), &[1; 32]);
                    Resp::SessionOpened(SessionOpened {
                        session_id: 42,
                        ..Default::default()
                    })
                }
                Req::OpenTerminal(req) => {
                    assert_eq!(req.session_id, 42);
                    if mode == "command" {
                        assert_eq!(req.args, ["-c", "printf result; exit 7"]);
                        assert_eq!(req.cwd, "/work/repo");
                        after = true;
                    }
                    if mode == "drop" {
                        break;
                    }
                    Resp::TerminalStarted(TerminalStarted { terminal_id: 9 })
                }
                Req::TerminalInput(req) => {
                    assert_eq!(req.terminal_id, 9);
                    input.extend_from_slice(&req.data);
                    if mode == "pipe" && input.ends_with(b"\x04\x04") {
                        // Escape bytes must survive a pipe, and EOF must not truncate output.
                        assert_eq!(input, b"hello\x1d.\n\x04\x04");
                        after = true;
                    }
                    if mode == "fast" {
                        after = true;
                        Resp::Error(nexus_proto::v1::IpcError {
                            code: nexus_proto::v1::IpcErrorCode::NotFound.into(),
                            message: "terminal already ended".into(),
                        })
                    } else {
                        Resp::Ack(IpcAck {})
                    }
                }
                Req::CloseTerminal(req) => {
                    assert_eq!(req.terminal_id, 9);
                    closed = true;
                    Resp::Ack(IpcAck {})
                }
                Req::CloseSession(req) => {
                    assert_eq!(req.session_id, 42);
                    assert!(closed);
                    Resp::Ack(IpcAck {})
                }
                other => panic!("unexpected request: {other:?}"),
            };
            conn.send(&IpcFrame {
                body: Some(Frame::Response(IpcResponse {
                    in_reply_to: req.id,
                    body: Some(body),
                })),
            })
            .await
            .unwrap();
            if after {
                event(
                    &mut conn,
                    Ev::TerminalOutput(TerminalOutput {
                        terminal_id: 9,
                        data: b"final output\r\n".to_vec().into(),
                    }),
                )
                .await;
                event(
                    &mut conn,
                    Ev::TerminalEnded(TerminalEnded {
                        terminal_id: 9,
                        exited: true,
                        exit_code: 7,
                        ..Default::default()
                    }),
                )
                .await;
            }
        }
    });
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_nexus"))
        .args([
            "--home",
            home.path().to_str().unwrap(),
            "remote",
            "terminal",
            "Henrik server",
        ])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).await.unwrap();
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), agent)
        .await
        .unwrap()
        .unwrap();
    output
}

#[tokio::test]
async fn remote_command_forwards_cwd_arguments_output_and_exit_status() {
    let output = exercise(
        "command",
        &["--command", "printf result; exit 7", "--cwd", "/work/repo"],
        b"",
    )
    .await;
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(output.stdout, b"final output\r\n");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[tokio::test]
async fn pipes_preserve_escape_bytes_and_drain_output_after_eof() {
    let output = exercise("pipe", &[], b"hello\x1d.\n").await;
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(output.stdout, b"final output\r\n");
}

#[tokio::test]
async fn timeout_closes_the_remote_attachment_and_reports_failure() {
    let output = exercise("timeout", &["--timeout", "1"], b"").await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("timed out"));
}

#[tokio::test]
async fn lost_agent_is_failure_not_a_successful_remote_exit() {
    let output = exercise("drop", &[], b"").await;
    assert!(!output.status.success());
}

#[tokio::test]
async fn remote_exit_wins_when_stdin_eof_races_terminal_cleanup() {
    let output = exercise("fast", &["--command", "exit 7"], b"").await;
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(output.stdout, b"final output\r\n");
}
