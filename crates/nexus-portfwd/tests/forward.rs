//! End to end over in-memory pipes: a fake "network" stream between controller and target.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use nexus_portfwd::{
    AllowRule, ForwardError, ForwardPolicy, listen_and_forward, open_forward, serve_stream,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A TCP echo server on loopback.
async fn echo_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

/// Connects a controller-side stream to a target-side `serve_stream` running `policy`.
fn pipe(policy: ForwardPolicy) -> tokio::io::DuplexStream {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let active = Arc::default();
    tokio::spawn(async move {
        serve_stream(server, &policy, &active, &|_, _, _| {}).await;
    });
    client
}

#[tokio::test]
async fn bytes_flow_both_ways_to_a_loopback_service() {
    let port = echo_server().await;
    let mut s = pipe(ForwardPolicy::default());
    open_forward(&mut s, "localhost", port).await.unwrap();
    s.write_all(b"hello through the tunnel").await.unwrap();
    let mut buf = [0u8; 24];
    s.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"hello through the tunnel");
    // A large transfer in both directions at once.
    let data: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
    let (mut r, mut w) = tokio::io::split(s);
    let send = data.clone();
    let writer = tokio::spawn(async move {
        w.write_all(&send).await.unwrap();
        w
    });
    let mut got = vec![0u8; data.len()];
    r.read_exact(&mut got).await.unwrap();
    assert!(got == data);
    drop(writer.await.unwrap());
}

#[tokio::test]
async fn policy_and_failures_are_reported_distinctly() {
    let port = echo_server().await;
    // Not allowed: an address off the machine, and anything at all under deny-all.
    let mut s = pipe(ForwardPolicy::default());
    assert!(matches!(
        open_forward(&mut s, "10.1.2.3", 80).await,
        Err(ForwardError::Denied)
    ));
    let mut s = pipe(ForwardPolicy::deny_all());
    assert!(matches!(
        open_forward(&mut s, "localhost", port).await,
        Err(ForwardError::Denied)
    ));
    // Allowed but nothing listening.
    let free = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let mut s = pipe(ForwardPolicy::default());
    assert!(matches!(
        open_forward(&mut s, "127.0.0.1", free).await,
        Err(ForwardError::Refused)
    ));
    // Malformed request.
    let mut s = pipe(ForwardPolicy::default());
    assert!(matches!(
        open_forward(&mut s, "", 80).await,
        Err(ForwardError::Invalid(_))
    ));
    let (mut c, server) = tokio::io::duplex(1024);
    let active = Arc::default();
    tokio::spawn(async move {
        serve_stream(server, &ForwardPolicy::default(), &active, &|_, _, _| {}).await
    });
    c.write_all(b"XXXXXXXX").await.unwrap();
    assert_eq!(c.read_u8().await.unwrap(), 4, "bad request status");
}

#[tokio::test]
async fn explicit_allow_rules_work_and_the_connection_limit_holds() {
    let port = echo_server().await;
    let mut policy = ForwardPolicy {
        loopback: false,
        ..ForwardPolicy::default()
    };
    policy.allow.push(AllowRule::new("127.0.0.1", port));
    policy.max_connections = 2;
    let policy = Arc::new(policy);
    let active: Arc<nexus_portfwd::Active> = Arc::default();
    let connect = |host: &'static str| {
        let (policy, active) = (Arc::clone(&policy), Arc::clone(&active));
        async move {
            let (mut client, server) = tokio::io::duplex(1024);
            tokio::spawn(
                async move { serve_stream(server, &policy, &active, &|_, _, _| {}).await },
            );
            open_forward(&mut client, host, port).await.map(|()| client)
        }
    };
    // "localhost" is not allowed (loopback rule off, only the literal address is listed).
    assert!(matches!(
        connect("localhost").await,
        Err(ForwardError::Denied)
    ));
    let a = connect("127.0.0.1").await.unwrap();
    let b = connect("127.0.0.1").await.unwrap();
    assert!(
        matches!(connect("127.0.0.1").await, Err(ForwardError::Denied)),
        "third connection exceeds the limit"
    );
    drop(a);
    // The slot frees once the target notices the close.
    let mut freed = false;
    for _ in 0..100 {
        if connect("127.0.0.1").await.is_ok() {
            freed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(freed, "a closed forward must release its slot");
    drop(b);
}

#[tokio::test]
async fn a_local_listener_forwards_each_connection() {
    let port = echo_server().await;
    let policy = Arc::new(ForwardPolicy::default());
    let active: Arc<nexus_portfwd::Active> = Arc::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap();
    let open = move || {
        let (policy, active) = (Arc::clone(&policy), Arc::clone(&active));
        async move {
            let (client, server) = tokio::io::duplex(64 * 1024);
            tokio::spawn(
                async move { serve_stream(server, &policy, &active, &|_, _, _| {}).await },
            );
            Ok::<_, String>(client)
        }
    };
    let task = tokio::spawn(listen_and_forward(listener, "localhost".into(), port, open));
    // Three simultaneous connections, each its own tunnel.
    let mut handles = Vec::new();
    for i in 0..3u8 {
        handles.push(tokio::spawn(async move {
            let mut c = TcpStream::connect(local).await.unwrap();
            let msg = vec![i; 10_000];
            c.write_all(&msg).await.unwrap();
            let mut back = vec![0u8; msg.len()];
            tokio::time::timeout(Duration::from_secs(5), c.read_exact(&mut back))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(back, msg);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    task.abort();
}
