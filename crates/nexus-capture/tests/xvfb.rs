//! Capture tests against a real (virtual) X server.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use nexus_capture::x11::X11Backend;
use nexus_capture::{CaptureBackend, Rect};
use nexus_testkit::{Painter, Xvfb};

fn px(f: &nexus_capture::Frame, x: usize, y: usize) -> [u8; 3] {
    let i = y * f.stride + x * 4;
    [f.data[i + 2], f.data[i + 1], f.data[i]] // BGRA → RGB
}

#[test]
fn sees_what_is_drawn_and_only_the_damaged_part() {
    let Some(xvfb) = Xvfb::start(800, 600) else {
        return;
    };
    let painter = Painter::connect(xvfb.display());
    painter.fill(0, 0, 800, 600, 0x102030);

    let backend = X11Backend::connect(Some(xvfb.display())).unwrap();
    let monitors = backend.monitors().unwrap();
    assert_eq!((monitors[0].width, monitors[0].height), (800, 600));
    let mut cap = backend.open(0).unwrap();

    let first = cap.next_frame(Duration::from_secs(1)).unwrap().unwrap();
    first.validate().unwrap();
    assert!(first.dirty.is_empty());
    assert_eq!(px(&first, 5, 5), [0x10, 0x20, 0x30]);

    // Nothing changes → no frame.
    let spurious = cap.next_frame(Duration::from_millis(100)).unwrap();
    assert!(
        spurious.is_none(),
        "unexpected damage: {:?}",
        spurious.map(|f| f.dirty)
    );

    painter.fill(100, 200, 50, 40, 0xff0000);
    let started = Instant::now();
    let frame = cap
        .next_frame(Duration::from_secs(2))
        .unwrap()
        .expect("damage wakes the capturer");
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(!frame.dirty.is_empty());
    let covered = frame
        .dirty
        .iter()
        .any(|r: &Rect| r.x <= 100 && r.y <= 200 && r.x + r.w >= 150 && r.y + r.h >= 240);
    assert!(
        covered,
        "dirty rects {:?} must cover the painted area",
        frame.dirty
    );
    assert_eq!(px(&frame, 120, 220), [0xff, 0x00, 0x00]);
    assert_eq!(
        px(&frame, 5, 5),
        [0x10, 0x20, 0x30],
        "untouched pixels survive the partial update"
    );
}

#[test]
fn cursor_position_and_shape() {
    let Some(xvfb) = Xvfb::start(640, 480) else {
        return;
    };
    let painter = Painter::connect(xvfb.display());
    let backend = X11Backend::connect(Some(xvfb.display())).unwrap();
    let mut cursor = backend.cursor_source().unwrap().expect("XFixes available");
    painter.warp_pointer(123, 321);
    let s = cursor.poll().unwrap();
    assert_eq!((s.x, s.y), (123, 321));
    let shape = s.new_shape.expect("first sample carries the bitmap");
    assert_eq!(shape.bgra.len(), (shape.width * shape.height * 4) as usize);
    assert!(cursor.poll().unwrap().new_shape.is_none());
}

#[test]
fn no_display_is_a_clear_error() {
    let err = X11Backend::connect(Some(":9999")).unwrap_err();
    assert!(err.to_string().contains("cannot connect"), "{err}");
}
