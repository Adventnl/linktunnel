//! Update manifests are downloaded from a URL: parsing and verification must reject, not panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_update::manifest::{SignedManifest, PublisherKeys};

fuzz_target!(|data: &[u8]| {
    let Ok(signed) = serde_json::from_slice::<SignedManifest>(data) else { return };
    let keys = PublisherKeys::from_hex(&["5866666666666666666666666666666666666666666666666666666666666666"]);
    if let Ok(keys) = keys {
        // The key is the curve's base point, whose private key nobody holds: nothing may verify.
        assert!(keys.verify(&signed).is_err());
    }
});
