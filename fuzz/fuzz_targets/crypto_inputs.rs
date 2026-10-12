//! Untrusted bytes into the identity, sealed-envelope and pairing-code parsers.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_crypto::noise::{envelope, pairing};
use nexus_crypto::{DeviceIdentity, IdentitySeed, PublicIdentity};

fuzz_target!(|data: &[u8]| {
    let _ = PublicIdentity::from_bytes(data);
    let me = DeviceIdentity::from_seed(&IdentitySeed::from_bytes([7; 32]));
    let _ = envelope::open(&me, data);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = pairing::PairingCode::parse(text);
    }
});
