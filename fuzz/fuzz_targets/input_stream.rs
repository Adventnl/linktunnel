//! The input stream a controller sends: packets in arbitrary chunks. The target applies these to
//! the machine, so the framing and event decoder must be airtight.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_media::input_wire::PacketReader;

fuzz_target!(|data: &[u8]| {
    let Some((&chunk, rest)) = data.split_first() else { return };
    let mut r = PacketReader::new();
    for piece in rest.chunks(usize::from(chunk).max(1)) {
        r.extend(piece);
        loop {
            match r.next_packet() {
                Ok(Some(p)) => assert!(p.events.len() <= 4096),
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
});
