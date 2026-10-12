//! Arbitrary bytes into the H.264 decoder (OpenH264, C code): the single most exposed parser,
//! since the controller decodes whatever the target sends.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_proto::v1::VideoCodec;

fuzz_target!(|data: &[u8]| {
    let Ok(mut dec) = nexus_codec::create_decoder(VideoCodec::H264) else { return };
    // Several access units, split at the 0x00 0x00 0x00 0x01 start codes the fuzzer produces or
    // at fixed points, to exercise state carried between calls.
    for piece in data.chunks(1500) {
        if let Ok(Some(frame)) = dec.decode(piece) {
            assert!(frame.width > 0 && frame.height > 0);
        }
    }
});
