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
    }
}

async fn start_device(name: &str, seed: u8, home: &std::path::Path, signal: Option<Url>) -> Device {
    let agent = Agent::start(
        config(name, home, signal),
        identity(seed),
        Arc::new(DiagnosticsHandler),
    )
    .await
    .unwrap();
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
