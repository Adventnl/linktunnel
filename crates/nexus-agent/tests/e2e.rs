//! Whole-system tests: real agents, real signaling server, real QUIC, driven over IPC.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use nexus_agent::{Agent, AgentConfig, DiagnosticsHandler};
use nexus_core::Paths;
use nexus_crypto::{DeviceId, DeviceIdentity, IdentitySeed};
use nexus_ipc::{EventReceiver, IpcClient, IpcError};
use nexus_net::NodeConfig;
use nexus_proto::v1::{
    IpcErrorCode, IpcEvent, PathKind, PingDevice, PresenceState, ipc_event::Body as Ev,
};
use nexus_signal::store::{MemoryPairingStore, MemoryPresenceStore};
use nexus_signal::{Hub, SignalConfig, serve};
use nexus_trust::grants;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use url::Url;

// ---- harness ----------------------------------------------------------------------------------

struct Server {
    addr: SocketAddr,
    _stop: oneshot::Sender<()>,
}

impl Server {
    async fn start_on(addr: &str) -> Self {
        let listener = TcpListener::bind(addr).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hub = Hub::new(
            SignalConfig::new(addr.to_string()),
            Arc::new(MemoryPairingStore::new()),
            Arc::new(MemoryPresenceStore::new()),
        );
        let (stop, rx) = oneshot::channel::<()>();
        tokio::spawn(serve(listener, hub, async {
            let _ = rx.await;
        }));
        Self { addr, _stop: stop }
    }

    async fn start() -> Self {
        Self::start_on("127.0.0.1:0").await
    }

    fn url(&self) -> Url {
        format!("ws://{}/v1/ws", self.addr).parse().unwrap()
    }
}

struct Device {
    agent: Agent,
    client: IpcClient,
    events: EventReceiver,
    seed: u8,
    home: std::path::PathBuf,
    name: String,
}

fn identity(seed: u8) -> Arc<DeviceIdentity> {
    Arc::new(DeviceIdentity::from_seed(&IdentitySeed::from_bytes(
        [seed; 32],
    )))
}

fn config(name: &str, home: &std::path::Path, signal: Option<Url>) -> AgentConfig {
    AgentConfig {
        device_name: name.into(),
        paths: Paths::under(home),
        signal_url: signal,
        node: NodeConfig {
            bind: Some("127.0.0.1:0".parse().unwrap()),
            port_mapping: false,
            ..NodeConfig::default()
        },
        reconnect_max: Duration::from_millis(500),
        publish_loopback_hints: true,
        start_locked_down: false,
        // The scenarios below are about transport and policy, not prompts; the attended flow
        // has its own tests.
        default_consent: nexus_core::config::ConsentMode::Unattended,
        consent_timeout: Duration::from_secs(30),
    }
}

async fn start_device(name: &str, seed: u8, home: &std::path::Path, signal: Option<Url>) -> Device {
    start_device_with(name, seed, home, signal, Arc::new(DiagnosticsHandler)).await
}

async fn start_device_with(
    name: &str,
    seed: u8,
    home: &std::path::Path,
    signal: Option<Url>,
    handler: Arc<dyn nexus_agent::SessionHandler>,
) -> Device {
    start_device_cfg(name, seed, home, signal, handler, |_| {}).await
}

async fn start_device_cfg(
    name: &str,
    seed: u8,
    home: &std::path::Path,
    signal: Option<Url>,
    handler: Arc<dyn nexus_agent::SessionHandler>,
    tweak: impl FnOnce(&mut AgentConfig),
) -> Device {
    let mut cfg = config(name, home, signal);
    tweak(&mut cfg);
    let agent = Agent::start(cfg, identity(seed), handler).await.unwrap();
    let (client, events, _welcome) = IpcClient::connect(agent.ipc_paths(), "test").await.unwrap();
    Device {
        agent,
        client,
        events,
        seed,
        home: home.to_path_buf(),
        name: name.into(),
    }
}

impl Device {
    fn id(&self) -> Vec<u8> {
        self.agent.device_id().as_bytes().to_vec()
    }

    async fn restart(self, signal: Option<Url>) -> Device {
        let (seed, home, name) = (self.seed, self.home.clone(), self.name.clone());
        drop(self.client);
        self.agent.shutdown().await;
        start_device(&name, seed, &home, signal).await
    }
}

async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

async fn wait_signal(d: &Device) {
    eventually("signaling connection", || async {
        d.client.status().await.unwrap().signal_connected
    })
    .await;
}

async fn next_matching<T>(
    events: &mut EventReceiver,
    mut pick: impl FnMut(&IpcEvent) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let ev = tokio::time::timeout_at(deadline, events.next())
            .await
            .expect("timed out waiting for an IPC event")
            .expect("IPC event stream closed");
        if let Some(found) = pick(&ev) {
            return found;
        }
    }
}

/// Pairs `joiner` with `offerer` through the full flow, confirming the words on both sides.
async fn pair(offerer: &mut Device, joiner: &mut Device) {
    let offer = offerer.client.pairing_offer(grants::all()).await.unwrap();
    assert_eq!(offer.code.split(' ').count(), 6);
    let join = joiner
        .client
        .pairing_join(&offer.code, grants::all(), vec![grants::SCREEN.into()])
        .await
        .unwrap();

    let sas_of = |ev: &IpcEvent| match &ev.body {
        Some(Ev::PairingSas(s)) => Some(s.clone()),
        _ => None,
    };
    let (offerer_sas, joiner_sas) = tokio::join!(
        next_matching(&mut offerer.events, sas_of),
        next_matching(&mut joiner.events, sas_of),
    );
    assert_eq!(
        offerer_sas.words, joiner_sas.words,
        "both screens must show identical words"
    );
    assert_eq!(offerer_sas.words.len(), 4);
    assert_eq!(joiner_sas.peer_device_id.as_ref(), offerer.id().as_slice());
    assert_eq!(offerer_sas.peer_device_id.as_ref(), joiner.id().as_slice());

    offerer
        .client
        .pairing_confirm(offer.pairing_id, true)
        .await
        .unwrap();
    joiner
        .client
        .pairing_confirm(join.pairing_id, true)
        .await
        .unwrap();

    let done = |ev: &IpcEvent| match &ev.body {
        Some(Ev::PairingFinished(f)) => Some(f.clone()),
        _ => None,
    };
    let (a, b) = tokio::join!(
        next_matching(&mut offerer.events, done),
        next_matching(&mut joiner.events, done)
    );
    assert!(a.success && b.success, "pairing failed: {a:?} / {b:?}");
}

async fn wait_online(from: &Device, peer: &Device) {
    let peer_id = peer.id();
    eventually("peer to be online", || async {
        from.client
            .list_devices()
            .await
            .unwrap()
            .devices
            .iter()
            .any(|d| d.device_id.as_ref() == peer_id.as_slice() && d.online)
    })
    .await;
}

fn ping_request(peer: &Device) -> PingDevice {
    PingDevice {
        device_id: peer.id().into(),
        pings: 50,
        stream_echoes: 50,
        datagrams: 50,
        bulk_bytes: 4 * 1024 * 1024,
    }
}

// ---- tests ------------------------------------------------------------------------------------

#[tokio::test]
async fn two_agents_pair_see_each_others_presence_and_exchange_encrypted_traffic() {
    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut mac = start_device("MacBook", 1, &dir.path().join("a"), Some(server.url())).await;
    let mut pc = start_device("Windows PC", 2, &dir.path().join("b"), Some(server.url())).await;
    wait_signal(&mac).await;
    wait_signal(&pc).await;

    // Before pairing neither knows the other exists.
    assert!(mac.client.list_devices().await.unwrap().devices.is_empty());

    pair(&mut pc, &mut mac).await; // the PC shows the code, the Mac enters it

    // Presence: both dashboards show the other, online, with the name it chose.
    wait_online(&mac, &pc).await;
    wait_online(&pc, &mac).await;
    let view = mac.client.list_devices().await.unwrap().devices.remove(0);
    assert_eq!(view.name, "Windows PC");
    assert_eq!(view.state, PresenceState::Online as i32);
    assert_eq!(
        view.grants_on_peer.len(),
        grants::all().len(),
        "the Mac was granted full control"
    );
    let view = pc.client.list_devices().await.unwrap().devices.remove(0);
    assert_eq!(
        view.grants_on_peer,
        vec![grants::SCREEN.to_string()],
        "the PC was only granted view"
    );

    // An authenticated, end-to-end encrypted session: control RPC, input-sized streams,
    // datagrams and bulk data between the two real agents.
    let report = mac.client.ping(ping_request(&pc)).await.unwrap();
    println!(
        "control p50={}µs p95={}µs | stream p50={}µs | datagram p50={}µs loss={:.1}% | bulk {:.0} Mbit/s | path {:?} → {:?} ({})",
        report.control.as_ref().unwrap().p50_us,
        report.control.as_ref().unwrap().p95_us,
        report.stream.as_ref().unwrap().p50_us,
        report.datagram.as_ref().unwrap().p50_us,
        report.datagram_loss * 100.0,
        report.throughput_mbps,
        report.path_before(),
        report.path_after(),
        report.remote,
    );
    assert_eq!(report.control.unwrap().count, 50);
    assert_eq!(report.stream.unwrap().count, 50);
    assert!(report.datagram_loss < 0.1);
    assert!(report.throughput_mbps > 50.0);
    assert_eq!(report.path_after(), PathKind::Lan);

    // And the other direction works too.
    pc.client.ping(ping_request(&mac)).await.unwrap();
}

#[tokio::test]
async fn unpairing_ends_the_relationship_on_both_sides_immediately() {
    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut mac = start_device("MacBook", 1, &dir.path().join("a"), Some(server.url())).await;
    let mut pc = start_device("Windows PC", 2, &dir.path().join("b"), Some(server.url())).await;
    wait_signal(&mac).await;
    wait_signal(&pc).await;
    pair(&mut pc, &mut mac).await;
    wait_online(&mac, &pc).await;
    mac.client.ping(ping_request(&pc)).await.unwrap();

    // The PC's owner unpairs the Mac.
    pc.client.unpair(mac.id()).await.unwrap();
    assert!(pc.client.list_devices().await.unwrap().devices.is_empty());

    // The Mac learns of it through the server and drops the device from its dashboard...
    let gone = |ev: &IpcEvent| matches!(ev.body, Some(Ev::DeviceRemoved(_))).then_some(());
    next_matching(&mut mac.events, gone).await;
    assert!(mac.client.list_devices().await.unwrap().devices.is_empty());
    // ...and neither can open a session to the other any more.
    let err = mac.client.ping(ping_request(&pc)).await.unwrap_err();
    assert!(
        matches!(
            err,
            IpcError::Agent {
                code: IpcErrorCode::NotFound,
                ..
            }
        ),
        "{err:?}"
    );

    // The decision survives a restart of both agents.
    let mac = mac.restart(Some(server.url())).await;
    let pc = pc.restart(Some(server.url())).await;
    assert!(mac.client.list_devices().await.unwrap().devices.is_empty());
    assert!(pc.client.list_devices().await.unwrap().devices.is_empty());
}

#[tokio::test]
async fn pairings_survive_agent_restarts_and_a_signaling_server_restart() {
    let server = Server::start().await;
    let addr = server.addr;
    let dir = tempfile::tempdir().unwrap();
    let mut mac = start_device("MacBook", 1, &dir.path().join("a"), Some(server.url())).await;
    let mut pc = start_device("Windows PC", 2, &dir.path().join("b"), Some(server.url())).await;
    wait_signal(&mac).await;
    wait_signal(&pc).await;
    pair(&mut pc, &mut mac).await;
    wait_online(&mac, &pc).await;

    // The server loses all its state (in-memory stores) and comes back on the same address.
    drop(server);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let server = Server::start_on(&addr.to_string()).await;
    // Agents reconnect on their own and re-register the pairing they hold.
    wait_signal(&mac).await;
    wait_signal(&pc).await;
    wait_online(&mac, &pc).await;
    mac.client.ping(ping_request(&pc)).await.unwrap();

    // Now both agent processes restart.
    let mac = mac.restart(Some(server.url())).await;
    let pc = pc.restart(Some(server.url())).await;
    wait_signal(&mac).await;
    wait_online(&mac, &pc).await;
    mac.client.ping(ping_request(&pc)).await.unwrap();
}

#[tokio::test]
async fn lockdown_refuses_everything_at_once_and_persists() {
    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut mac = start_device("MacBook", 1, &dir.path().join("a"), Some(server.url())).await;
    let mut pc = start_device("Windows PC", 2, &dir.path().join("b"), Some(server.url())).await;
    wait_signal(&mac).await;
    wait_signal(&pc).await;
    pair(&mut pc, &mut mac).await;
    wait_online(&mac, &pc).await;
    mac.client.ping(ping_request(&pc)).await.unwrap();

    pc.client.set_lockdown(true).await.unwrap();
    assert!(pc.client.status().await.unwrap().lockdown);
    let err = mac.client.ping(ping_request(&pc)).await.unwrap_err();
    assert!(
        matches!(err, IpcError::Agent { .. }),
        "a locked-down device accepted a session: {err:?}"
    );
    // A locked-down device also refuses to dial out or to pair.
    assert!(pc.client.ping(ping_request(&mac)).await.is_err());
    assert!(pc.client.pairing_offer(vec![]).await.is_err());

    // Lockdown is sticky across restarts.
    let pc = pc.restart(Some(server.url())).await;
    assert!(pc.client.status().await.unwrap().lockdown);
    wait_signal(&pc).await;
    assert!(mac.client.ping(ping_request(&pc)).await.is_err());

    pc.client.set_lockdown(false).await.unwrap();
    wait_online(&mac, &pc).await;
    mac.client.ping(ping_request(&pc)).await.unwrap();
}

#[tokio::test]
async fn a_mismatch_reported_by_either_human_cancels_the_pairing_for_both() {
    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut mac = start_device("MacBook", 1, &dir.path().join("a"), Some(server.url())).await;
    let mut pc = start_device("Windows PC", 2, &dir.path().join("b"), Some(server.url())).await;
    wait_signal(&mac).await;
    wait_signal(&pc).await;

    let offer = pc.client.pairing_offer(grants::all()).await.unwrap();
    let join = mac
        .client
        .pairing_join(&offer.code, grants::all(), vec![])
        .await
        .unwrap();
    let sas = |ev: &IpcEvent| matches!(ev.body, Some(Ev::PairingSas(_))).then_some(());
    next_matching(&mut pc.events, sas).await;
    next_matching(&mut mac.events, sas).await;

    // The Mac's user sees different words and says so.
    mac.client
        .pairing_confirm(join.pairing_id, false)
        .await
        .unwrap();
    let done = |ev: &IpcEvent| match &ev.body {
        Some(Ev::PairingFinished(f)) => Some(f.clone()),
        _ => None,
    };
    let (m, p) = tokio::join!(
        next_matching(&mut mac.events, done),
        next_matching(&mut pc.events, done)
    );
    assert!(!m.success && !p.success);
    assert!(mac.client.list_devices().await.unwrap().devices.is_empty());
    assert!(pc.client.list_devices().await.unwrap().devices.is_empty());
    // The slot is free again.
    pc.client.pairing_offer(vec![]).await.unwrap();
}

#[tokio::test]
async fn pairing_guards_busy_cancel_and_missing_signaling() {
    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let pc = start_device("Windows PC", 2, &dir.path().join("b"), Some(server.url())).await;
    wait_signal(&pc).await;

    let first = pc.client.pairing_offer(vec![]).await.unwrap();
    let err = pc.client.pairing_offer(vec![]).await.unwrap_err();
    assert!(
        matches!(
            err,
            IpcError::Agent {
                code: IpcErrorCode::Busy,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(
        pc.client
            .pairing_join("not six words", vec![], vec![])
            .await
            .is_err()
    );
    pc.client.pairing_cancel(first.pairing_id).await.unwrap();
    eventually("slot to free up", || async {
        pc.client.pairing_offer(vec![]).await.is_ok()
    })
    .await;
    assert!(pc.client.pairing_cancel(9999).await.is_err());
    assert!(pc.client.pairing_confirm(9999, true).await.is_err());

    // Without a signaling server there is nowhere to meet.
    let offline = start_device("Offline", 3, &dir.path().join("c"), None).await;
    let err = offline.client.pairing_offer(vec![]).await.unwrap_err();
    assert!(
        matches!(
            err,
            IpcError::Agent {
                code: IpcErrorCode::Unavailable,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(!offline.client.status().await.unwrap().signal_connected);
}

#[tokio::test]
async fn status_reports_identity_and_a_second_agent_on_the_same_home_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("a");
    let d = start_device("MacBook", 1, &home, None).await;
    let status = d.client.status().await.unwrap();
    assert_eq!(status.device_id.as_ref(), d.id().as_slice());
    assert_eq!(status.device_name, "MacBook");
    assert_eq!(
        status.fingerprint,
        identity(1).public().fingerprint().to_string()
    );
    assert!(!status.direct_addresses.is_empty());
    assert_eq!(
        DeviceId::from_slice(&status.device_id).unwrap(),
        identity(1).device_id()
    );
    let second = Agent::start(
        config("Again", &home, None),
        identity(1),
        Arc::new(DiagnosticsHandler),
    )
    .await;
    assert!(second.is_err(), "two agents shared one runtime directory");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paired_device_can_watch_and_control_another_through_the_agents() {
    use nexus_capture::synthetic::{PROBE_ON, SyntheticBackend, SyntheticConfig};
    use nexus_input::recording::RecordingInjector;
    use nexus_input::{InputEvent, KeyState, Modifiers};
    use nexus_media::viewer::VideoDecoder;
    use nexus_proto::wire::input::{InputPacket, TimedInput};
    use nexus_proto::wire::video::{FIXED_LEN, VideoFrameHeader};

    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (backend, probe) = SyntheticBackend::new(SyntheticConfig {
        width: 320,
        height: 240,
        ..Default::default()
    });
    let (injector, recorded) = RecordingInjector::new();
    let injector = std::sync::Mutex::new(Some(injector));
    let handler = nexus_agent::RemoteHandler::new(
        Some(Arc::new(backend)),
        Some(Arc::new(move || {
            let i = injector
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| nexus_input::InputError::Failed("used".into()))?;
            Ok(Box::new(i) as Box<dyn nexus_input::Injector>)
        })),
        60,
    );
    let mut mac = start_device("MacBook", 1, &dir.path().join("a"), Some(server.url())).await;
    let mut pc = start_device_with(
        "Windows PC",
        2,
        &dir.path().join("b"),
        Some(server.url()),
        Arc::new(handler),
    )
    .await;
    wait_signal(&mac).await;
    wait_signal(&pc).await;
    pair(&mut pc, &mut mac).await;
    wait_online(&mac, &pc).await;

    let opened = mac
        .client
        .open_session(nexus_proto::v1::OpenSession {
            device_id: pc.id().into(),
            want_video: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(opened.peer_name, "Windows PC");
    let video = opened.video.clone().unwrap();
    assert_eq!((video.width, video.height), (320, 240));
    assert_eq!(opened.monitors.len(), 1);
    assert_eq!(opened.path(), PathKind::Lan);

    // Decode what arrives over IPC, exactly as a viewer process would.
    let mut decoder = VideoDecoder::new();
    let mut saw_first = false;
    probe.set_flash(true);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut flash_seen = false;
    while tokio::time::Instant::now() < deadline && !flash_seen {
        let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(500), mac.events.next()).await
        else {
            continue;
        };
        let Some(Ev::SessionVideo(v)) = ev.body else {
            continue;
        };
        assert_eq!(v.session_id, opened.session_id);
        let fixed: [u8; FIXED_LEN] = v.frame[..FIXED_LEN].try_into().unwrap();
        let head_len = FIXED_LEN + VideoFrameHeader::extra_len(&fixed).unwrap();
        let header = VideoFrameHeader::decode(&v.frame[..head_len]).unwrap();
        let frame = nexus_media::controller::VideoFrame {
            header,
            payload: v.frame.slice(head_len..),
            recv_us: 0,
        };
        match decoder.decode(&frame) {
            Ok(Some(p)) => {
                saw_first = true;
                let i = (32 * p.width as usize + 32) * 4;
                flash_seen = [p.bgra[i], p.bgra[i + 1], p.bgra[i + 2]]
                    .iter()
                    .zip(PROBE_ON)
                    .all(|(a, b)| a.abs_diff(b) < 40);
            }
            Ok(None) => {}
            Err(_) => {
                mac.client
                    .session_keyframe(opened.session_id)
                    .await
                    .unwrap();
            }
        }
    }
    assert!(
        saw_first && flash_seen,
        "the controller must see the target's screen, including a change made after connecting"
    );

    // Input travels IPC → agent → QUIC → target agent → injector.
    let sent = vec![
        InputEvent::MouseMoveAbs {
            monitor: 0,
            x: 5,
            y: 6,
        },
        InputEvent::Key {
            page: 7,
            usage: 4,
            state: KeyState::Down,
            modifiers: Modifiers::default(),
        },
        InputEvent::Key {
            page: 7,
            usage: 4,
            state: KeyState::Up,
            modifiers: Modifiers::default(),
        },
    ];
    let packet = InputPacket {
        seq: 0,
        base_us: 0,
        events: sent
            .iter()
            .cloned()
            .map(|event| TimedInput { dt_us: 0, event })
            .collect(),
        skipped: 0,
    };
    let mut bytes = bytes::BytesMut::new();
    packet.encode(&mut bytes).unwrap();
    mac.client
        .session_input(opened.session_id, bytes.to_vec())
        .await
        .unwrap();
    eventually("input to reach the target's injector", || async {
        recorded.events() == sent
    })
    .await;

    // Other clients cannot touch the session; unknown sessions are errors.
    let (other, _ev, _) = IpcClient::connect(mac.agent.ipc_paths(), "other")
        .await
        .unwrap();
    assert!(matches!(
        other.close_session(opened.session_id).await,
        Err(IpcError::Agent {
            code: IpcErrorCode::PermissionDenied,
            ..
        })
    ));
    assert!(matches!(
        other.session_keyframe(9999).await,
        Err(IpcError::Agent {
            code: IpcErrorCode::NotFound,
            ..
        })
    ));

    // Diagnostics still work against a remote-desktop target.
    mac.client.ping(ping_request(&pc)).await.unwrap();

    // Disconnecting the viewer's IPC connection ends the session on both agents.
    mac.client.close_session(opened.session_id).await.unwrap();
    eventually("the target to see no active sessions", || async {
        pc.client.status().await.unwrap().active_sessions == 0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_move_between_paired_devices_through_the_agents_even_when_the_target_has_no_screen() {
    use nexus_files::{FileRoots, SharedRoot};

    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let share = dir.path().join("share");
    std::fs::create_dir_all(&share).unwrap();
    let handler = nexus_agent::RemoteHandler::new(None, None, 60).with_files(
        FileRoots::new(vec![SharedRoot {
            path: share.clone(),
            writable: true,
        }]),
        1 << 30,
    );
    let mut mac = start_device("MacBook", 1, &dir.path().join("a"), Some(server.url())).await;
    let mut pc = start_device_with(
        "Windows PC",
        2,
        &dir.path().join("b"),
        Some(server.url()),
        Arc::new(handler),
    )
    .await;
    wait_signal(&mac).await;
    wait_signal(&pc).await;
    pair(&mut pc, &mut mac).await;
    wait_online(&mac, &pc).await;

    let opened = mac
        .client
        .open_session(nexus_proto::v1::OpenSession {
            device_id: pc.id().into(),
            want_video: false,
            ..Default::default()
        })
        .await
        .unwrap();
    let sid = opened.session_id;

    let roots = mac.client.file_list(sid, "").await.unwrap();
    assert_eq!(roots.entries.len(), 1);
    let remote_dir = roots.entries[0].name.clone();

    // Upload ~3 MiB, watching progress events.
    let payload: Vec<u8> = (0..3_000_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let src = dir.path().join("src.bin");
    std::fs::write(&src, &payload).unwrap();
    let remote_file = format!("{remote_dir}/up.bin");
    let started = mac
        .client
        .file_transfer(nexus_proto::v1::FileTransfer {
            session_id: sid,
            remote_path: remote_file.clone(),
            local_path: src.to_string_lossy().into_owned(),
            upload: true,
            overwrite: false,
        })
        .await
        .unwrap();
    let finished = loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), mac.events.next())
            .await
            .expect("transfer events")
            .expect("event stream");
        if let Some(Ev::TransferFinished(f)) = ev.body
            && f.transfer_id == started.transfer_id
        {
            break f;
        }
    };
    assert!(finished.ok, "{}", finished.error_message);
    assert_eq!(finished.blake3.as_ref(), blake3::hash(&payload).as_bytes());
    assert_eq!(std::fs::read(share.join("up.bin")).unwrap(), payload);

    // Download it back to a different place.
    let dst = dir.path().join("down.bin");
    let started = mac
        .client
        .file_transfer(nexus_proto::v1::FileTransfer {
            session_id: sid,
            remote_path: remote_file,
            local_path: dst.to_string_lossy().into_owned(),
            upload: false,
            overwrite: false,
        })
        .await
        .unwrap();
    let finished = loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), mac.events.next())
            .await
            .expect("transfer events")
            .expect("event stream");
        if let Some(Ev::TransferFinished(f)) = ev.body
            && f.transfer_id == started.transfer_id
        {
            break f;
        }
    };
    assert!(finished.ok, "{}", finished.error_message);
    assert_eq!(std::fs::read(&dst).unwrap(), payload);

    // Outside the share: refused, nothing leaks.
    std::fs::write(dir.path().join("secret"), b"s").unwrap();
    let err = mac
        .client
        .file_stat(sid, dir.path().join("secret").to_str().unwrap())
        .await;
    assert!(matches!(
        err,
        Err(IpcError::Agent {
            code: IpcErrorCode::PermissionDenied,
            ..
        })
    ));
    mac.client.close_session(sid).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_can_switch_monitors_through_the_agent() {
    use nexus_capture::synthetic::{SyntheticBackend, SyntheticConfig};

    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (backend, _probe) = SyntheticBackend::new(SyntheticConfig {
        monitors: 2,
        width: 320,
        height: 200,
        ..Default::default()
    });
    let handler = nexus_agent::RemoteHandler::new(Some(Arc::new(backend)), None, 30);
    let mut mac = start_device("MacBook", 1, &dir.path().join("a"), Some(server.url())).await;
    let mut pc = start_device_with(
        "Windows PC",
        2,
        &dir.path().join("b"),
        Some(server.url()),
        Arc::new(handler),
    )
    .await;
    wait_signal(&mac).await;
    wait_signal(&pc).await;
    pair(&mut pc, &mut mac).await;
    wait_online(&mac, &pc).await;

    let opened = mac
        .client
        .open_session(nexus_proto::v1::OpenSession {
            device_id: pc.id().into(),
            want_video: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(opened.monitors.len(), 2);
    mac.client
        .session_monitor(opened.session_id, 1)
        .await
        .unwrap();
    let changed = next_matching(&mut mac.events, |ev| match &ev.body {
        Some(Ev::SessionMonitorChanged(m)) if m.monitor == 1 => Some(*m),
        _ => None,
    })
    .await;
    assert_eq!((changed.native_width, changed.native_height), (320, 200));
    assert!(matches!(
        mac.client.session_monitor(opened.session_id, 999).await,
        Err(IpcError::Agent {
            code: IpcErrorCode::InvalidRequest,
            ..
        })
    ));
    mac.client.close_session(opened.session_id).await.unwrap();
}

/// A paired MacBook (controller) and a headless PC (target) that shares a folder and asks first.
async fn attended_pair(
    dir: &std::path::Path,
    timeout: Duration,
) -> (Server, Device, Device, std::path::PathBuf) {
    use nexus_files::{FileRoots, SharedRoot};
    let server = Server::start().await;
    let share = dir.join("share");
    std::fs::create_dir_all(&share).unwrap();
    std::fs::write(share.join("a.txt"), b"hello").unwrap();
    let handler = nexus_agent::RemoteHandler::new(None, None, 60).with_files(
        FileRoots::new(vec![SharedRoot {
            path: share.clone(),
            writable: true,
        }]),
        1 << 30,
    );
    let mac = start_device("MacBook", 1, &dir.join("a"), Some(server.url())).await;
    let pc = start_device_cfg(
        "Windows PC",
        2,
        &dir.join("b"),
        Some(server.url()),
        Arc::new(handler),
        |c| {
            c.default_consent = nexus_core::config::ConsentMode::Attended;
            c.consent_timeout = timeout;
        },
    )
    .await;
    wait_signal(&mac).await;
    wait_signal(&pc).await;
    let (mut mac, mut pc) = (mac, pc);
    pair(&mut pc, &mut mac).await;
    wait_online(&mac, &pc).await;
    (server, mac, pc, share)
}

async fn open_files_session(mac: &Device, pc: &Device) -> u64 {
    mac.client
        .open_session(nexus_proto::v1::OpenSession {
            device_id: pc.id().into(),
            want_video: false,
            ..Default::default()
        })
        .await
        .unwrap()
        .session_id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attended_access_waits_for_a_person_and_honours_yes_and_no() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, mac, mut pc, share) = attended_pair(dir.path(), Duration::from_secs(30)).await;
    let sid = open_files_session(&mac, &pc).await;
    let want = share.to_str().unwrap().to_owned();

    // Connecting and pinging need no approval.
    mac.client.ping(ping_request(&pc)).await.unwrap();

    // Listing files does. The PC's local client sees the prompt, says yes.
    let list = {
        let (client, want) = (mac.client.clone(), want.clone());
        tokio::spawn(async move { client.file_list(sid, &want).await })
    };
    let req = next_matching(&mut pc.events, |ev| match &ev.body {
        Some(Ev::ConsentRequest(r)) => Some(r.clone()),
        _ => None,
    })
    .await;
    assert_eq!(req.peer_name, "MacBook");
    assert!(req.grants.iter().any(|g| g == "files"));
    assert!(!list.is_finished(), "the request must wait for the person");
    pc.client
        .consent_decision(req.request_id, true, false)
        .await
        .unwrap();
    let listing = list.await.unwrap().unwrap();
    assert_eq!(listing.entries.len(), 1);
    // Answering twice is an error, not a second approval.
    assert!(
        pc.client
            .consent_decision(req.request_id, true, false)
            .await
            .is_err()
    );

    // The answer holds for the rest of that session without asking again.
    mac.client.file_list(sid, &want).await.unwrap();
    mac.client.close_session(sid).await.unwrap();

    // A new session asks again, and "no" is final for it.
    let sid = open_files_session(&mac, &pc).await;
    let list = {
        let (client, want) = (mac.client.clone(), want.clone());
        tokio::spawn(async move { client.file_list(sid, &want).await })
    };
    let req = next_matching(&mut pc.events, |ev| match &ev.body {
        Some(Ev::ConsentRequest(r)) => Some(r.clone()),
        _ => None,
    })
    .await;
    pc.client
        .consent_decision(req.request_id, false, false)
        .await
        .unwrap();
    assert!(list.await.unwrap().is_err(), "a refused request must fail");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_allow_pre_authorises_the_device_until_it_is_switched_back() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, mac, mut pc, share) = attended_pair(dir.path(), Duration::from_secs(30)).await;
    let want = share.to_str().unwrap().to_owned();
    let sid = open_files_session(&mac, &pc).await;
    let list = {
        let (client, want) = (mac.client.clone(), want.clone());
        tokio::spawn(async move { client.file_list(sid, &want).await })
    };
    let req = next_matching(&mut pc.events, |ev| match &ev.body {
        Some(Ev::ConsentRequest(r)) => Some(r.clone()),
        _ => None,
    })
    .await;
    pc.client
        .consent_decision(req.request_id, true, true)
        .await
        .unwrap();
    list.await.unwrap().unwrap();
    mac.client.close_session(sid).await.unwrap();

    let devices = pc.client.list_devices().await.unwrap().devices;
    assert!(devices[0].unattended);

    // No prompt any more: nobody answers anything and it still works.
    let sid = open_files_session(&mac, &pc).await;
    tokio::time::timeout(Duration::from_secs(5), mac.client.file_list(sid, &want))
        .await
        .expect("no prompt expected")
        .unwrap();
    mac.client.close_session(sid).await.unwrap();

    // Switching back restores the prompt (and the choice survives in the list view).
    pc.client.set_unattended(mac.id(), false).await.unwrap();
    assert!(!pc.client.list_devices().await.unwrap().devices[0].unattended);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unanswered_prompt_expires_and_an_unwatched_machine_refuses_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, mac, mut pc, share) = attended_pair(dir.path(), Duration::from_secs(1)).await;
    let want = share.to_str().unwrap().to_owned();

    // Nobody answers: refused when the timeout passes, and the prompt is withdrawn.
    let sid = open_files_session(&mac, &pc).await;
    let started = std::time::Instant::now();
    let r = mac.client.file_list(sid, &want).await;
    assert!(r.is_err());
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
    let resolved = next_matching(&mut pc.events, |ev| match &ev.body {
        Some(Ev::ConsentResolved(r)) => Some(*r),
        _ => None,
    })
    .await;
    assert!(resolved.timed_out && !resolved.allowed);
    mac.client.close_session(sid).await.ok();

    // With no client connected at the target nobody *can* approve: refuse immediately.
    let pc_id = pc.id();
    let Device { client, events, .. } = pc;
    drop(client);
    drop(events);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let opened = mac
        .client
        .open_session(nexus_proto::v1::OpenSession {
            device_id: pc_id.into(),
            want_video: false,
            ..Default::default()
        })
        .await
        .unwrap();
    let started = std::time::Instant::now();
    assert!(
        mac.client
            .file_list(opened.session_id, &want)
            .await
            .is_err()
    );
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn power_requests_reach_the_other_agent_and_wake_needs_a_known_address() {
    use nexus_power::{PowerAction, RecordingPower};

    let server = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let rec = RecordingPower::new();
    let handler = nexus_agent::RemoteHandler::new(None, None, 60)
        .with_power(Arc::new(rec.clone()))
        .with_wake_relay(true);
    let mut mac = start_device("MacBook", 1, &dir.path().join("a"), Some(server.url())).await;
    let mut pc = start_device_with(
        "Windows PC",
        2,
        &dir.path().join("b"),
        Some(server.url()),
        Arc::new(handler),
    )
    .await;
    wait_signal(&mac).await;
    wait_signal(&pc).await;
    pair(&mut pc, &mut mac).await;
    wait_online(&mac, &pc).await;

    let opened = mac
        .client
        .open_session(nexus_proto::v1::OpenSession {
            device_id: pc.id().into(),
            want_video: false,
            ..Default::default()
        })
        .await
        .unwrap();
    mac.client
        .power(opened.session_id, nexus_proto::v1::PowerActionKind::Lock)
        .await
        .unwrap();
    mac.client
        .power(
            opened.session_id,
            nexus_proto::v1::PowerActionKind::Unspecified,
        )
        .await
        .unwrap_err();
    assert_eq!(rec.actions(), [PowerAction::Lock]);
    mac.client.close_session(opened.session_id).await.unwrap();

    // The MAC addresses of this test machine may or may not be known; a made-up device id is
    // always an error, and so is waking a device nobody has a hardware address for.
    assert!(mac.client.wake_device(vec![1; 32], vec![]).await.is_err());
    let known = mac
        .client
        .list_devices()
        .await
        .unwrap()
        .devices
        .into_iter()
        .any(|v| v.os.is_some_and(|o| !o.mac_addresses.is_empty()));
    if !known {
        assert!(mac.client.wake_device(pc.id(), vec![]).await.is_err());
    }
}
