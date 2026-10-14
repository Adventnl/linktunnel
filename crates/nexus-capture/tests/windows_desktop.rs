//! Opt-in acceptance test against the physical Windows desktop, without writing screenshots.
#![cfg(target_os = "windows")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use nexus_capture::{CaptureBackend, FrameFormat, windows_capture::WindowsBackend};

#[test]
#[ignore = "requires an unlocked interactive Windows desktop with a physical monitor"]
fn captures_every_connected_monitor_and_samples_the_cursor() {
    let backend = WindowsBackend::new().expect("native Windows capture backend");
    let monitors = backend.monitors().expect("enumerate physical monitors");
    assert!(
        !monitors.is_empty(),
        "this acceptance host must have a monitor"
    );
    for monitor in monitors {
        let mut capturer = backend.open(monitor.index).expect("open monitor");
        let deadline = Instant::now() + Duration::from_secs(5);
        let frame = loop {
            if let Some(frame) = capturer
                .next_frame(Duration::from_millis(250))
                .expect("capture desktop frame")
            {
                break frame;
            }
            assert!(
                Instant::now() < deadline,
                "no initial frame on {}",
                monitor.name
            );
        };
        frame.validate().expect("valid pixel buffer");
        assert_eq!(frame.format, FrameFormat::Bgra8);
        assert_eq!((frame.width, frame.height), (monitor.width, monitor.height));
        let first = &frame.data[..3];
        assert!(
            frame.data.chunks_exact(4).any(|pixel| &pixel[..3] != first),
            "{} returned a solid-color desktop; verify it is unlocked",
            monitor.name
        );
        eprintln!(
            "captured {}: {}x{} via {}",
            monitor.name,
            frame.width,
            frame.height,
            capturer.name()
        );
    }
    let mut cursor = backend
        .cursor_source()
        .unwrap()
        .expect("native cursor source");
    let sample = cursor.poll().expect("sample physical cursor");
    if let Some(shape) = sample.new_shape {
        assert_eq!(
            shape.bgra.len(),
            shape.width as usize * shape.height as usize * 4
        );
    }
}
