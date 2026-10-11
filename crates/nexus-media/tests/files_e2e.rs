//! The file panel over a real QUIC session on loopback, with the grant check in the path.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use nexus_capture::synthetic::{SyntheticBackend, SyntheticConfig};
use nexus_files::{FileRoots, FileServer, SharedRoot};
use nexus_media::controller::{ControllerOptions, ControllerSession};
use nexus_media::target::{self, TargetConfig, TargetStats};
use nexus_trust::grants;

async fn rig(grant_list: &[&str], root: &std::path::Path) -> ControllerSession {
    let (ia, ib) = (identity(1), identity(2));
    let a = loopback_node(&ia, &[key(&ib)]).await;
    let b = loopback_node(&ib, &[key(&ia)]).await;
    let (a_session, b_session) = connect_pair(&a, &b, &loopback_peer(&b)).await;
    let (ha, hb) = (hello(1), hello(2));
    let (ea, eb) = tokio::join!(
        a_session.establish(&ha, CONTROLLER, &[nexus_proto::v1::VideoCodec::H264]),
        b_session.establish(&hb, TARGET, &[nexus_proto::v1::VideoCodec::H264])
    );
    let (ea, eb) = (ea.unwrap(), eb.unwrap());
    let (backend, _probe) = SyntheticBackend::new(SyntheticConfig::default());
    let roots = FileRoots::new(vec![SharedRoot {
        path: root.to_owned(),
        writable: true,
    }]);
    let config = TargetConfig {
        grants: grant_list
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>(),
        capture: Arc::new(backend),
        injector: None,
        max_fps: 30,
        max_inflight: 8,
        egress: None,
        files: Some(Arc::new(FileServer::new(roots, 1 << 30))),
        clipboard: None,
        consent: None,
        power: None,
        wake_relay: false,
        terminal: None,
    };
    tokio::spawn(target::serve(
        b_session,
        eb.control,
        config,
        Arc::new(TargetStats::default()),
    ));
    let (controller, streams) =
        ControllerSession::start(a_session, ea.control, ControllerOptions::default())
            .await
            .unwrap();
    // The media events are not used here; keep the receiver alive so the controller task runs.
    std::mem::forget(streams);
    std::mem::forget(a);
    std::mem::forget(b);
    controller
}

fn data(len: usize) -> Vec<u8> {
    let mut x = 12345u32;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

#[tokio::test]
async fn browse_download_and_upload_over_quic() {
    let dir = tempfile::tempdir().unwrap();
    let share = dir.path().join("share");
    std::fs::create_dir(&share).unwrap();
    let bytes = data(9 * 1024 * 1024 + 5);
    std::fs::write(share.join("big.bin"), &bytes).unwrap();
    let controller = rig(&[grants::FILES, grants::SCREEN], &share).await;
    let files = controller.files();

    let roots = files.list("").await.unwrap();
    assert_eq!(roots.entries.len(), 1);
    let listing = files.list(&roots.entries[0].name).await.unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].size, bytes.len() as u64);

    let dest = dir.path().join("got.bin");
    let t = files
        .download(
            share.join("big.bin").to_str().unwrap(),
            &dest,
            false,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), bytes);
    assert_eq!(t.blake3, *blake3::hash(&bytes).as_bytes());

    // Several operations at once do not interfere (one stream each).
    let up_src = dir.path().join("up.bin");
    std::fs::write(&up_src, &bytes).unwrap();
    let (p1, p2) = (share.join("up1.bin"), share.join("up2.bin"));
    let (a, b) = tokio::join!(
        files.upload(&up_src, p1.to_str().unwrap(), false, |_| {}),
        files.upload(&up_src, p2.to_str().unwrap(), false, |_| {}),
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        files
            .list(share.to_str().unwrap())
            .await
            .unwrap()
            .entries
            .len(),
        3
    );
    assert_eq!(std::fs::read(share.join("up1.bin")).unwrap(), bytes);
    assert_eq!(std::fs::read(share.join("up2.bin")).unwrap(), bytes);
}

#[tokio::test]
async fn without_the_files_grant_nothing_is_served() {
    let dir = tempfile::tempdir().unwrap();
    let share = dir.path().join("share");
    std::fs::create_dir(&share).unwrap();
    let controller = rig(&[grants::SCREEN], &share).await;
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        controller.files().list(share.to_str().unwrap()),
    )
    .await
    .expect("the refusal must be prompt, not a hang");
    assert!(r.is_err(), "{r:?}");
}
