//! Pairing records and revocations as the signaling server and a peer would present them.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_proto::v1::{SignedPairingRecord, SignedRevocation};
use nexus_trust::{record, revocation};
use prost::Message;

fuzz_target!(|data: &[u8]| {
    let _ = record::parse_unsigned(data);
    if let Ok(signed) = SignedPairingRecord::decode(data) {
        let _ = record::verify(&signed);
    }
    if let Ok(signed) = SignedRevocation::decode(data) {
        let _ = revocation::verify(&signed, 1_700_000_000_000);
    }
});
