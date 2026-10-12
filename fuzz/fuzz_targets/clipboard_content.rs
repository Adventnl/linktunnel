//! Clipboard items from the peer: PNG decoding and update validation.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_clipboard::{ClipContent, decode_png};
use nexus_proto::v1::ClipboardUpdate;
use prost::Message;

fuzz_target!(|data: &[u8]| {
    let _ = decode_png(data);
    if let Ok(update) = ClipboardUpdate::decode(data) {
        let _ = ClipContent::from_update(&update);
    }
});
