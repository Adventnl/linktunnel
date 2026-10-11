//! Authentication and framing over a real local socket.
#![cfg(unix)]

use std::time::Duration;

use nexus_proto::v1::{
    IpcFrame, IpcHello, IpcResponse, IpcWelcome, ipc_frame::Body as F, ipc_request::Body as Req,
    ipc_response::Body as Resp,
};
use tokio::io::AsyncWriteExt;

use super::*;

fn welcome() -> IpcWelcome {
    IpcWelcome {
        agent_version: "test".into(),
        ..Default::default()
    }
}

/// Serves requests: `Status` → status, anything else → NotFound error. Returns the paths.
async fn start(dir: &tempfile::TempDir) -> (IpcPaths, tokio::task::JoinHandle<()>) {
    let paths = IpcPaths::in_dir(&dir.path().join("run"));
    let mut server = IpcServer::bind(paths.clone()).await.unwrap();
    let handle = tokio::spawn(async move {
        loop {
            let Ok(pending) = server.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let Ok((mut conn, _name)) = pending.authenticate(welcome()).await else {
                    return;
                };
                while let Ok(Some(frame)) = conn.recv().await {
                    if let Some(F::Request(r)) = frame.body {
                        let body = match r.body {
                            Some(Req::Status(_)) => Resp::Status(nexus_proto::v1::Status {
                                device_name: "mac".into(),
                                ..Default::default()
                            }),
                            _ => Resp::Error(nexus_proto::v1::IpcError {
                                code: nexus_proto::v1::IpcErrorCode::NotFound as i32,
                                message: "nope".into(),
                            }),
                        };
                        let reply = IpcFrame {
                            body: Some(F::Response(IpcResponse {
                                in_reply_to: r.id,
                                body: Some(body),
                            })),
                        };
                        if conn.send(&reply).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    (paths, handle)
}

#[tokio::test]
async fn authenticated_client_gets_responses_and_typed_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _server) = start(&dir).await;
    let (client, _events, welcome) = IpcClient::connect(&paths, "test").await.unwrap();
    assert_eq!(welcome.agent_version, "test");
    assert_eq!(client.status().await.unwrap().device_name, "mac");
    let err = client.list_devices().await.unwrap_err();
    assert!(
        matches!(
            err,
            IpcError::Agent {
                code: nexus_proto::v1::IpcErrorCode::NotFound,
                ..
            }
        ),
        "{err:?}"
    );
    // Many concurrent requests are correlated correctly.
    let results = futures_join(&client).await;
    assert!(results.iter().all(|r| r.as_deref() == Ok("mac")));
}

async fn futures_join(client: &IpcClient) -> Vec<Result<String, String>> {
    let mut handles = Vec::new();
    for _ in 0..50 {
        let c = client.clone();
        handles.push(tokio::spawn(async move {
            c.status()
                .await
                .map(|s| s.device_name)
                .map_err(|e| e.to_string())
        }));
    }
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

#[tokio::test]
async fn a_wrong_token_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _server) = start(&dir).await;
    // Overwrite the client's view of the token.
    let bogus = dir.path().join("bogus.token");
    std::fs::write(&bogus, "0".repeat(64)).unwrap();
    let wrong = IpcPaths {
        token_file: bogus,
        ..paths
    };
    let err = IpcClient::connect(&wrong, "attacker").await.unwrap_err();
    assert!(
        matches!(err, IpcError::Unauthorized | IpcError::Closed),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_client_that_sends_requests_before_authenticating_gets_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _server) = start(&dir).await;
    let IpcEndpoint::Unix(sock) = &paths.endpoint else {
        panic!()
    };
    let stream = tokio::net::UnixStream::connect(sock).await.unwrap();
    let mut conn = IpcConnection::new(Box::new(stream));
    conn.send(&IpcFrame {
        body: Some(F::Request(nexus_proto::v1::IpcRequest {
            id: 1,
            body: Some(Req::Status(nexus_proto::v1::GetStatus {})),
        })),
    })
    .await
    .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2), conn.recv())
        .await
        .unwrap();
    assert!(
        matches!(reply, Ok(None) | Err(_)),
        "server answered an unauthenticated request: {reply:?}"
    );
}

#[tokio::test]
async fn garbage_and_slow_clients_do_not_wedge_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _server) = start(&dir).await;
    let IpcEndpoint::Unix(sock) = &paths.endpoint else {
        panic!()
    };
    // 1. Garbage bytes.
    let mut s = tokio::net::UnixStream::connect(sock).await.unwrap();
    s.write_all(&[0xff; 64]).await.unwrap();
    drop(s);
    // 2. A connection that never says anything (held open).
    let _idle = tokio::net::UnixStream::connect(sock).await.unwrap();
    // 3. An oversize declared frame.
    let mut s = tokio::net::UnixStream::connect(sock).await.unwrap();
    s.write_all(&[0x80, 0x80, 0x80, 0x80, 0x08]).await.unwrap();
    // A legitimate client still works.
    let (client, _e, _w) = IpcClient::connect(&paths, "good").await.unwrap();
    assert!(client.status().await.is_ok());
}

#[tokio::test]
async fn token_and_socket_are_private_and_a_second_agent_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (paths, _server) = start(&dir).await;
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&paths.token_file), 0o600);
    let IpcEndpoint::Unix(sock) = &paths.endpoint else {
        panic!()
    };
    assert_eq!(mode(sock), 0o600);
    assert_eq!(mode(paths.token_file.parent().unwrap()), 0o700);
    assert!(matches!(
        IpcServer::bind(paths.clone()).await,
        Err(IpcError::AlreadyRunning(_))
    ));
}

#[tokio::test]
async fn stale_sockets_from_a_crashed_agent_are_replaced_and_cleanup_happens_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let paths = IpcPaths::in_dir(&dir.path().join("run"));
    let IpcEndpoint::Unix(sock) = paths.endpoint.clone() else {
        panic!()
    };
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    // Simulate a crash: a socket file with nobody listening.
    drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
    assert!(sock.exists());
    let server = IpcServer::bind(paths.clone()).await.unwrap();
    drop(server);
    assert!(
        !sock.exists() && !paths.token_file.exists(),
        "files left behind after shutdown"
    );
}

#[tokio::test]
async fn connecting_without_an_agent_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let paths = IpcPaths::in_dir(&dir.path().join("run"));
    let err = IpcClient::connect(&paths, "x").await.unwrap_err();
    assert!(matches!(err, IpcError::Unreachable(_)), "{err:?}");
    let _ = IpcHello::default();
}
