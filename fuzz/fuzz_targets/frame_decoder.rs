//! Length-delimited stream framing fed in arbitrary chunks: no panic, no unbounded allocation
//! (the decoder caps the declared length before reserving memory).
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_proto::framing::FrameDecoder;
use nexus_proto::v1::ControlMessage;

fuzz_target!(|data: &[u8]| {
    let Some((&chunk, rest)) = data.split_first() else { return };
    let chunk = usize::from(chunk).max(1);
    let mut d = FrameDecoder::new(64 * 1024);
    for piece in rest.chunks(chunk) {
        d.extend(piece);
        loop {
            match d.next_message::<ControlMessage>() {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
    assert!(d.buffered() <= rest.len());
});
