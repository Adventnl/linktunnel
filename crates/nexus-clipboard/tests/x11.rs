//! The real OS-clipboard backend against a private X server.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use nexus_clipboard::{ArboardBackend, ClipContent, ClipboardBackend, encode_png};
use nexus_testkit::Xvfb;

#[test]
fn text_and_images_roundtrip_through_the_x_clipboard() {
    let Some(x) = Xvfb::start(320, 200) else {
        return;
    };
    // One test, one process-wide DISPLAY: no other test in this binary touches it.
    #[allow(unsafe_code)]
    // SAFETY: nothing else in this test binary reads the environment concurrently.
    unsafe {
        std::env::set_var("DISPLAY", x.display());
    }
    let mut writer = ArboardBackend::new().unwrap();
    let mut reader = ArboardBackend::new().unwrap();

    writer.write(&ClipContent::text("héllo — 你好")).unwrap();
    let got = reader.read(true).unwrap();
    assert_eq!(got.text.as_deref(), Some("héllo — 你好"));

    let rgba: Vec<u8> = (0..8 * 8)
        .flat_map(|i| [i as u8, 255 - i as u8, 7, 255])
        .collect();
    let png = encode_png(8, 8, &rgba).unwrap();
    writer
        .write(&ClipContent {
            text: None,
            image_png: Some(png),
        })
        .unwrap();
    let got = reader.read(true).unwrap();
    let (w, h, back) = nexus_clipboard::decode_png(got.image_png.as_deref().unwrap()).unwrap();
    assert_eq!((w, h), (8, 8));
    assert_eq!(back, rgba);
}
