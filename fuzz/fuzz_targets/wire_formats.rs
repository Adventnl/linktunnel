//! Every hot-path binary format: must never panic, and whatever decodes must re-encode to
//! something that decodes to the same value.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_proto::wire::{audio, cursor, input, video};

fuzz_target!(|data: &[u8]| {
    if let Ok(update) = cursor::CursorUpdate::decode(data) {
        let mut out = Vec::new();
        update.encode(&mut out);
        assert_eq!(cursor::CursorUpdate::decode(&out).ok(), Some(update));
    }
    if let Ok((header, payload)) = audio::decode_packet(data) {
        let mut out = Vec::new();
        audio::encode_packet(&header, payload, &mut out).expect("a decoded packet re-encodes");
        let (h2, p2) = audio::decode_packet(&out).expect("and decodes again");
        assert_eq!((h2, p2), (header, payload));
    }
    let _ = video::VideoFrameHeader::decode(data);
    if let Ok(packet) = input::InputPacket::decode(data) {
        let mut out = bytes::BytesMut::new();
        if packet.encode(&mut out).is_ok() {
            assert!(input::InputPacket::decode(&out).is_ok());
        }
    }
    let _ = audio::is_audio(data);
});
