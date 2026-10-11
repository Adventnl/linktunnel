//! Clipboard sync behaviour over the in-memory transport.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::time::Duration;

use nexus_clipboard::{
    ClipContent, ClipIo, ClipboardPolicy, MemoryClipboard, encode_png, memory_pair, run,
};
use nexus_proto::v1::{ClipboardItem, ClipboardUpdate};

fn fast() -> ClipboardPolicy {
    ClipboardPolicy {
        poll: Duration::from_millis(10),
        ..ClipboardPolicy::default()
    }
}

/// Lets the sync tasks take their "what was already there" snapshot before the test copies.
async fn started() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

async fn until<F: Fn() -> bool>(what: &str, f: F) {
    for _ in 0..300 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn copies_flow_both_ways_and_settle_without_echo() {
    let (a, b) = (MemoryClipboard::new(), MemoryClipboard::new());
    let (ia, ib) = memory_pair();
    let ta = tokio::spawn(run(a.clone(), ia, fast()));
    let tb = tokio::spawn(run(b.clone(), ib, fast()));

    started().await;
    a.set(ClipContent::text("from a"));
    until("b to receive", || b.get() == ClipContent::text("from a")).await;
    b.set(ClipContent::text("from b"));
    until("a to receive", || a.get() == ClipContent::text("from b")).await;

    // Let it settle: each side wrote exactly once for the other's single copy, no ping-pong.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(b.sync_writes(), 1, "b was written once");
    assert_eq!(a.sync_writes(), 1, "a was written once");
    ta.abort();
    tb.abort();
}

#[tokio::test]
async fn existing_clipboard_contents_are_not_sent_on_connect() {
    let (a, b) = (MemoryClipboard::new(), MemoryClipboard::new());
    a.set(ClipContent::text("private, copied before connecting"));
    let (ia, ib) = memory_pair();
    let ta = tokio::spawn(run(a.clone(), ia, fast()));
    let tb = tokio::spawn(run(b.clone(), ib, fast()));
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(b.get().is_empty(), "nothing leaks until the user copies");
    ta.abort();
    tb.abort();
}

#[tokio::test]
async fn each_direction_is_an_independent_opt_in() {
    let (a, b) = (MemoryClipboard::new(), MemoryClipboard::new());
    let (ia, ib) = memory_pair();
    // a may only receive; b may only send.
    let ta = tokio::spawn(run(
        a.clone(),
        ia,
        ClipboardPolicy {
            send: false,
            receive: true,
            ..fast()
        },
    ));
    let tb = tokio::spawn(run(
        b.clone(),
        ib,
        ClipboardPolicy {
            send: true,
            receive: false,
            ..fast()
        },
    ));
    started().await;
    a.set(ClipContent::text("a stays private"));
    b.set(ClipContent::text("b shares"));
    until("a to receive b's copy", || {
        a.get() == ClipContent::text("b shares")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(b.sync_writes(), 0, "b never applies anything");
    assert!(b.get() != ClipContent::text("a stays private"));
    ta.abort();
    tb.abort();
}

#[tokio::test]
async fn images_sync_and_can_be_switched_off() {
    let rgba: Vec<u8> = (0..16 * 16 * 4).map(|i| (i % 251) as u8).collect();
    let png = encode_png(16, 16, &rgba).unwrap();
    let img = ClipContent {
        text: None,
        image_png: Some(png),
    };

    let (a, b) = (MemoryClipboard::new(), MemoryClipboard::new());
    let (ia, ib) = memory_pair();
    let ta = tokio::spawn(run(a.clone(), ia, fast()));
    let tb = tokio::spawn(run(b.clone(), ib, fast()));
    started().await;
    a.set(img.clone());
    until("the image to arrive", || b.get() == img).await;
    ta.abort();
    tb.abort();

    let (c, d) = (MemoryClipboard::new(), MemoryClipboard::new());
    let (ic, id) = memory_pair();
    let no_images = ClipboardPolicy {
        images: false,
        ..fast()
    };
    let tc = tokio::spawn(run(c.clone(), ic, no_images));
    let td = tokio::spawn(run(d.clone(), id, no_images));
    started().await;
    c.set(img);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(d.get().is_empty());
    tc.abort();
    td.abort();
}

#[tokio::test]
async fn malformed_updates_from_the_peer_are_dropped_and_the_session_survives() {
    let a = MemoryClipboard::new();
    let (ia, mut peer) = memory_pair();
    let ta = tokio::spawn(run(a.clone(), ia, fast()));
    peer.send(ClipboardUpdate {
        seq: 1,
        items: vec![ClipboardItem {
            mime: "text/plain".into(),
            data: vec![0xff, 0xff].into(),
        }],
    })
    .await
    .unwrap();
    peer.send(ClipboardUpdate {
        seq: 2,
        items: vec![ClipboardItem {
            mime: "image/png".into(),
            data: b"not a png".to_vec().into(),
        }],
    })
    .await
    .unwrap();
    assert!(a.get().is_empty());
    peer.send(ClipContent::text("fine").to_update(3))
        .await
        .unwrap();
    until("a good update after bad ones", || {
        a.get() == ClipContent::text("fine")
    })
    .await;
    drop(peer);
    assert!(
        ta.await.unwrap().is_ok(),
        "ends cleanly when the peer leaves"
    );
}
