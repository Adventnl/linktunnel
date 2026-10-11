//! Classifying network paths for the UI and telemetry.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use nexus_proto::v1::PathKind;

/// True for addresses that are only reachable on the local network (or this very machine).
#[must_use]
pub fn is_local_network(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_local_v4(v4),
        IpAddr::V6(v6) => is_local_v6(v6),
    }
}

fn is_local_v4(ip: Ipv4Addr) -> bool {
    // Carrier-grade NAT space (100.64.0.0/10) is *not* the user's LAN: peers there are generally
    // other subscribers, so it is classified as a direct internet path.
    ip.is_loopback() || ip.is_private() || ip.is_link_local()
}

fn is_local_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_loopback()
        || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (first & 0xffc0) == 0xfe80 // link local fe80::/10
}

/// Kind of path for a direct (IP) address.
#[must_use]
pub fn classify_ip(ip: IpAddr) -> PathKind {
    if is_local_network(ip) {
        PathKind::Lan
    } else {
        PathKind::Direct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> PathKind {
        classify_ip(s.parse().unwrap())
    }

    #[test]
    fn local_addresses_are_lan() {
        for a in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.50",
            "169.254.9.9",
            "::1",
            "fd12:3456::1",
            "fe80::1",
        ] {
            assert_eq!(k(a), PathKind::Lan, "{a}");
        }
    }

    #[test]
    fn public_and_cgnat_addresses_are_direct() {
        for a in [
            "8.8.8.8",
            "203.0.113.5",
            "100.64.0.1",
            "100.127.255.254",
            "172.32.0.1",
            "2001:db8::1",
            "2606:4700::1111",
        ] {
            assert_eq!(k(a), PathKind::Direct, "{a}");
        }
    }
}
