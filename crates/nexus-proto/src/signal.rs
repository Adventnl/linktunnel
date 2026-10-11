//! Shared constants and helpers for the signaling protocol.

/// Largest WebSocket message either side accepts: a sealed envelope plus framing fits easily,
/// a stray multi-megabyte frame does not.
pub const MAX_SIGNAL_MESSAGE: usize = 64 * 1024;

/// Path of the WebSocket endpoint.
pub const WS_PATH: &str = "/v1/ws";

/// Length of the server nonce in `ServerHello`.
pub const SERVER_NONCE_LEN: usize = 32;
/// Length of the client nonce in `ClientAuth`.
pub const CLIENT_NONCE_LEN: usize = 16;
/// Length of a rendezvous room id.
pub const ROOM_ID_LEN: usize = 16;

/// The bytes a client signs (with context `SignalAuth`) to log in:
/// `len(audience) ‖ audience ‖ server_nonce ‖ client_nonce`.
///
/// Including the audience binds the proof to one server; including both nonces means every proof
/// is single-use and cannot be pre-computed or replayed.
#[must_use]
pub fn auth_message(audience: &str, server_nonce: &[u8], client_nonce: &[u8]) -> Vec<u8> {
    let audience = audience.as_bytes();
    let len = u16::try_from(audience.len()).unwrap_or(u16::MAX);
    let mut out = Vec::with_capacity(2 + audience.len() + server_nonce.len() + client_nonce.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(audience);
    out.extend_from_slice(server_nonce);
    out.extend_from_slice(client_nonce);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_message_is_unambiguous() {
        // Moving a byte from the audience into the nonce must change the message.
        let a = auth_message("ab", b"cd", b"ef");
        let b = auth_message("abc", b"d", b"ef");
        assert_ne!(a, b);
        assert_eq!(a, [2, 0, b'a', b'b', b'c', b'd', b'e', b'f']);
    }

    #[test]
    fn different_audiences_never_collide() {
        assert_ne!(
            auth_message("signal.example.com", &[1; 32], &[2; 16]),
            auth_message("signal.example.org", &[1; 32], &[2; 16])
        );
    }
}
