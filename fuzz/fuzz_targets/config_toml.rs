//! A user-edited config file: any text must either load and validate, or be refused cleanly.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus_core::config::ConfigLoader;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    if let Ok(cfg) = ConfigLoader::new().with_toml_str(text).load() {
        // `load` validates; a config it accepted must pass validation again.
        assert!(cfg.validate().is_ok());
    }
});
