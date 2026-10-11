//! The whole update flow against a local HTTP server standing in for the release host.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;

use axum::Router;
use axum::routing::get;
use nexus_crypto::{DeviceIdentity, IdentitySeed};
use nexus_update::fetch::{download, fetch_manifest};
use nexus_update::manifest::{Artifact, Manifest, PublisherKeys, sign};
use nexus_update::{Decision, Device, UpdateError, decide};
use semver::Version;

const PAYLOAD: &[u8] = b"#!/bin/sh\necho the new nexus\n";

async fn host(manifest_body: String, artifact_body: Vec<u8>) -> SocketAddr {
    let app = Router::new()
        .route(
            "/manifest.json",
            get(move || async move { manifest_body.clone() }),
        )
        .route(
            "/nexus-agent",
            get(move || async move { artifact_body.clone() }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn publisher() -> DeviceIdentity {
    DeviceIdentity::from_seed(&IdentitySeed::from_bytes([42; 32]))
}

fn keys(id: &DeviceIdentity) -> PublisherKeys {
    let hex: String = id
        .public()
        .ed25519_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    PublisherKeys::from_hex(&[hex]).unwrap()
}

fn manifest_for(addr: SocketAddr, blake3: String, size: u64) -> Manifest {
    Manifest {
        channel: "stable".into(),
        version: "2.0.0".into(),
        released_ms: 1,
        valid_until_ms: u64::MAX,
        min_supported: None,
        rollout_percent: 100,
        notes: "new".into(),
        artifacts: vec![Artifact {
            os: "linux".into(),
            arch: "x86_64".into(),
            url: format!("http://{addr}/nexus-agent"),
            blake3,
            size,
            kind: "binary".into(),
        }],
    }
}

fn hash_hex(data: &[u8]) -> String {
    blake3::hash(data)
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn device<'a>(current: &'a Version) -> Device<'a> {
    Device {
        current,
        os: "linux",
        arch: "x86_64",
        device_id: b"this-device",
        now_ms: 1_000,
        channel: "stable",
    }
}

#[tokio::test]
async fn check_download_verify_and_apply() {
    let id = publisher();
    // The manifest has to name the real address, so start the host in two steps.
    let placeholder = host(String::new(), PAYLOAD.to_vec()).await;
    let m = manifest_for(placeholder, hash_hex(PAYLOAD), PAYLOAD.len() as u64);
    let signed = serde_json::to_string(&sign(&id, &m).unwrap()).unwrap();
    let addr = host(signed, PAYLOAD.to_vec()).await;
    // The artifact URL points at the first server, which serves the same bytes.

    let fetched = fetch_manifest(&format!("http://{addr}/manifest.json"))
        .await
        .unwrap();
    let verified = keys(&id).verify(&fetched).unwrap();
    let cur = Version::parse("1.0.0").unwrap();
    let Decision::Update {
        version, artifact, ..
    } = decide(&verified, &device(&cur)).unwrap()
    else {
        panic!("expected an update");
    };
    assert_eq!(version.to_string(), "2.0.0");

    let dir = tempfile::tempdir().unwrap();
    let staged = download(&artifact, dir.path()).await.unwrap();
    assert_eq!(std::fs::read(&staged).unwrap(), PAYLOAD);

    #[cfg(unix)]
    {
        let target = dir.path().join("nexus-agent-installed");
        std::fs::write(&target, b"old").unwrap();
        nexus_update::apply::replace_binary(&staged, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), PAYLOAD);
    }
}

#[tokio::test]
async fn a_tampered_download_is_refused_and_leaves_nothing_behind() {
    let addr = host(String::new(), b"EVIL EVIL EVIL EVIL EVIL EVIL EV".to_vec()).await;
    // The signed manifest promises different bytes (right size, wrong content).
    let promised = vec![b'a'; 32];
    let m = manifest_for(addr, hash_hex(&promised), 32);
    let dir = tempfile::tempdir().unwrap();
    let err = download(&m.artifacts[0], dir.path()).await.unwrap_err();
    assert!(matches!(err, UpdateError::HashMismatch), "{err}");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "no partial file left"
    );

    // Too much data for the signed size is stopped early too.
    let addr = host(String::new(), vec![b'a'; 1000]).await;
    let m = manifest_for(addr, hash_hex(&vec![b'a'; 1000]), 10);
    assert!(matches!(
        download(&m.artifacts[0], dir.path()).await,
        Err(UpdateError::HashMismatch)
    ));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn an_unsigned_or_foreign_manifest_never_gets_as_far_as_a_download() {
    let id = publisher();
    let m = manifest_for("127.0.0.1:9".parse().unwrap(), hash_hex(PAYLOAD), 1);
    let signed = sign(&id, &m).unwrap();
    let stranger = DeviceIdentity::from_seed(&IdentitySeed::from_bytes([7; 32]));
    assert!(matches!(
        keys(&stranger).verify(&signed),
        Err(UpdateError::Untrusted)
    ));
    // Server errors surface as download errors, not panics.
    assert!(matches!(
        fetch_manifest("http://127.0.0.1:9/manifest.json").await,
        Err(UpdateError::Download(_))
    ));
}
