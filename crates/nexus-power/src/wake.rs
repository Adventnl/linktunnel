//! Wake-on-LAN: the "magic packet" is six `0xFF` bytes followed by the target's MAC address
//! sixteen times, sent as a UDP broadcast (conventionally to port 9).

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

/// Why a wake packet could not be sent.
#[derive(Debug, thiserror::Error)]
pub enum WakeError {
    /// The MAC address text is malformed.
    #[error("invalid MAC address {0:?}")]
    BadMac(String),
    /// The socket failed.
    #[error("could not send the wake packet: {0}")]
    Io(String),
}

/// Parses `aa:bb:cc:dd:ee:ff`, `aa-bb-cc-dd-ee-ff` or `aabbccddeeff`.
pub fn parse_mac(text: &str) -> Result<[u8; 6], WakeError> {
    let bad = || WakeError::BadMac(text.to_owned());
    let hex: String = text
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.'))
        .collect();
    if hex.len() != 12 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let mut mac = [0u8; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| bad())?;
    }
    // All zeros and all ones are never a real NIC.
    if mac == [0; 6] || mac == [0xFF; 6] {
        return Err(bad());
    }
    Ok(mac)
}

/// The 102-byte magic packet for `mac`.
#[must_use]
pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut p = [0xFFu8; 102];
    for i in 0..16 {
        p[6 + i * 6..12 + i * 6].copy_from_slice(&mac);
    }
    p
}

/// Broadcasts a magic packet for `mac` to `broadcast:port` (use `255.255.255.255` and port 9 for
/// the local network; a directed subnet broadcast address for others).
pub fn send_magic_packet(mac: [u8; 6], broadcast: Ipv4Addr, port: u16) -> Result<(), WakeError> {
    let io = |e: std::io::Error| WakeError::Io(e.to_string());
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).map_err(io)?;
    socket.set_broadcast(true).map_err(io)?;
    socket
        .send_to(&magic_packet(mac), SocketAddr::from((broadcast, port)))
        .map_err(io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs_parse_in_common_notations_and_reject_junk() {
        let want = [0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e];
        for s in [
            "00:1a:2b:3c:4d:5e",
            "00-1A-2B-3C-4D-5E",
            "001a2b3c4d5e",
            "001a.2b3c.4d5e",
        ] {
            assert_eq!(parse_mac(s).unwrap(), want, "{s}");
        }
        for s in [
            "",
            "00:1a:2b",
            "zz:zz:zz:zz:zz:zz",
            "00:00:00:00:00:00",
            "ff:ff:ff:ff:ff:ff",
            "00:1a:2b:3c:4d:5e:6f",
        ] {
            assert!(parse_mac(s).is_err(), "{s}");
        }
    }

    #[test]
    fn the_packet_has_the_documented_layout() {
        let mac = [1, 2, 3, 4, 5, 6];
        let p = magic_packet(mac);
        assert_eq!(&p[..6], &[0xFF; 6]);
        for i in 0..16 {
            assert_eq!(&p[6 + i * 6..12 + i * 6], &mac);
        }
        assert_eq!(p.len(), 102);
    }

    #[test]
    fn a_packet_goes_out_on_the_wire() {
        // Receive on loopback: the same code path, minus the broadcast flag mattering.
        let rx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        rx.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let port = rx.local_addr().unwrap().port();
        let mac = [0xde, 0xad, 0xbe, 0xef, 0x00, 0x01];
        send_magic_packet(mac, Ipv4Addr::LOCALHOST, port).unwrap();
        let mut buf = [0u8; 200];
        let (n, _) = rx.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], &magic_packet(mac));
    }
}
