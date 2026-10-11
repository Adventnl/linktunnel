//! What this machine is, for presence and capability announcements.

use nexus_proto::v1::{DeviceClass, OsFamily, OsInfo};

/// Describes the host OS.
#[must_use]
pub fn os_info() -> OsInfo {
    let info = os_info::get();
    let family = match info.os_type() {
        os_info::Type::Windows => OsFamily::Windows,
        os_info::Type::Macos => OsFamily::Macos,
        _ => OsFamily::Linux,
    };
    OsInfo {
        family: family as i32,
        version: format!("{} {}", info.os_type(), info.version()),
        arch: std::env::consts::ARCH.to_owned(),
        hostname: hostname(),
        class: DeviceClass::Desktop as i32,
    }
}

/// The machine's host name, or `unknown`.
#[must_use]
pub fn hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_this_machine() {
        let os = os_info();
        assert_ne!(os.family, OsFamily::Unspecified as i32);
        assert!(!os.hostname.is_empty());
        assert!(!os.arch.is_empty());
        assert!(!os.version.is_empty());
    }
}
