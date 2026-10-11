//! Sealed envelopes: one-way messages between *paired* devices that travel through the
//! untrusted signaling server.
//!
//! The server sees only `to`, `from` and an opaque blob. The blob is a single-message Noise `X`
//! handshake: the sender's static key is sent encrypted, the whole payload is encrypted to the
//! recipient's static key, and both statics are mixed in, so the recipient learns **who sent it**
//! (and that they hold the matching private key) and no one else can read or alter it.
//!
//! Noise `X` gives no replay protection by itself, so the payload carries a strictly increasing
//! counter and a timestamp: [`open_from_peers`] enforces a clock window and feeds the counter to a
//! [`ReplayGuard`].
//!
//! Known limit: `X` is vulnerable to key-compromise impersonation, i.e. someone who steals the
//! *recipient's* private key can forge messages to it. That attacker already controls the
//! recipient, so nothing further is lost.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use snow::Builder;

use super::NoiseError;
use super::replay::ReplayGuard;
use crate::error::CryptoError;
use crate::identity::{DeviceIdentity, PublicIdentity};

/// The exact Noise protocol name.
pub const ENVELOPE_PROTOCOL: &str = "Noise_X_25519_ChaChaPoly_BLAKE2s";
/// Largest application payload in one envelope.
pub const MAX_BODY: usize = 16 * 1024;
/// Largest sealed blob accepted (payload + Noise overhead + headroom).
pub const MAX_SEALED: usize = MAX_BODY + 512;
/// Accepted difference between an envelope's timestamp and the receiver's clock.
pub const MAX_CLOCK_SKEW_MS: u64 = 120_000;

const PROLOGUE_PREFIX: &[u8] = b"nexus-remote/v1/envelope\0";
const PAYLOAD_VERSION: u8 = 1;
const HEADER_LEN: usize = 1 + 2 + 8 + 8 + 4;

/// An application message and its anti-replay metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// Application-defined message type (e.g. wake request, session request).
    pub kind: u16,
    /// Strictly increasing per sender (see [`CounterSource`]).
    pub counter: u64,
    /// Sender's wall clock, milliseconds since the Unix epoch.
    pub unix_ms: u64,
    /// Opaque application payload.
    pub body: Vec<u8>,
}

/// An envelope after decryption, with the sender's authenticated Noise static key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedEnvelope {
    /// The sender's X25519 static key (authenticated by the handshake).
    pub sender_x25519: [u8; 32],
    /// The decrypted message.
    pub envelope: Envelope,
}

/// Produces strictly increasing counters that survive process restarts: each value is at least
/// the current wall-clock time in microseconds, so a restart cannot reuse an old counter unless
/// the clock moves backwards.
#[derive(Debug, Default)]
pub struct CounterSource {
    last: AtomicU64,
}

impl CounterSource {
    /// A new source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The next counter value.
    pub fn next(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX));
        let mut prev = self.last.load(Ordering::Relaxed);
        loop {
            let candidate = now.max(prev.saturating_add(1));
            match self.last.compare_exchange_weak(
                prev,
                candidate,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return candidate,
                Err(actual) => prev = actual,
            }
        }
    }
}

fn prologue(recipient: &PublicIdentity) -> Vec<u8> {
    let mut p = Vec::with_capacity(PROLOGUE_PREFIX.len() + 16);
    p.extend_from_slice(PROLOGUE_PREFIX);
    p.extend_from_slice(recipient.device_id().as_bytes());
    p
}

fn encode(envelope: &Envelope) -> Result<Vec<u8>, CryptoError> {
    if envelope.body.len() > MAX_BODY {
        return Err(NoiseError::TooLarge {
            len: envelope.body.len(),
            max: MAX_BODY,
        }
        .into());
    }
    let body_len = u32::try_from(envelope.body.len()).map_err(|_| NoiseError::BadPayload)?;
    let mut out = Vec::with_capacity(HEADER_LEN + envelope.body.len());
    out.push(PAYLOAD_VERSION);
    out.extend_from_slice(&envelope.kind.to_le_bytes());
    out.extend_from_slice(&envelope.counter.to_le_bytes());
    out.extend_from_slice(&envelope.unix_ms.to_le_bytes());
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&envelope.body);
    Ok(out)
}

fn decode(payload: &[u8]) -> Result<Envelope, CryptoError> {
    if payload.len() < HEADER_LEN || payload[0] != PAYLOAD_VERSION {
        return Err(NoiseError::BadPayload.into());
    }
    let u16_at = |i: usize| u16::from_le_bytes([payload[i], payload[i + 1]]);
    let u64_at = |i: usize| {
        let mut b = [0u8; 8];
        b.copy_from_slice(&payload[i..i + 8]);
        u64::from_le_bytes(b)
    };
    let kind = u16_at(1);
    let counter = u64_at(3);
    let unix_ms = u64_at(11);
    let body_len = usize::try_from(u32::from_le_bytes([
        payload[19],
        payload[20],
        payload[21],
        payload[22],
    ]))
    .map_err(|_| NoiseError::BadPayload)?;
    let body = &payload[HEADER_LEN..];
    if body_len > MAX_BODY || body.len() != body_len {
        return Err(NoiseError::BadPayload.into());
    }
    Ok(Envelope {
        kind,
        counter,
        unix_ms,
        body: body.to_vec(),
    })
}

/// Encrypts and authenticates `envelope` from `sender` to `recipient`.
pub fn seal(
    sender: &DeviceIdentity,
    recipient: &PublicIdentity,
    envelope: &Envelope,
) -> Result<Vec<u8>, CryptoError> {
    let payload = encode(envelope)?;
    let params = ENVELOPE_PROTOCOL.parse().map_err(NoiseError::from)?;
    let mut state = Builder::new(params)
        .local_private_key(sender.noise_secret())
        .map_err(NoiseError::from)?
        .remote_public_key(recipient.x25519())
        .map_err(NoiseError::from)?
        .prologue(&prologue(recipient))
        .map_err(NoiseError::from)?
        .build_initiator()
        .map_err(NoiseError::from)?;
    // e (32) + encrypted s (32 + 16) + encrypted payload (+16) = payload + 96.
    let mut out = vec![0u8; payload.len() + 128];
    let n = state
        .write_message(&payload, &mut out)
        .map_err(NoiseError::from)?;
    out.truncate(n);
    Ok(out)
}

/// Decrypts `sealed` addressed to `recipient`, returning the sender's authenticated Noise key.
///
/// This does **not** check who the sender is allowed to be, nor freshness; use
/// [`open_from_peers`] for that.
pub fn open(recipient: &DeviceIdentity, sealed: &[u8]) -> Result<OpenedEnvelope, CryptoError> {
    if sealed.len() > MAX_SEALED {
        return Err(NoiseError::TooLarge {
            len: sealed.len(),
            max: MAX_SEALED,
        }
        .into());
    }
    let params = ENVELOPE_PROTOCOL.parse().map_err(NoiseError::from)?;
    let mut state = Builder::new(params)
        .local_private_key(recipient.noise_secret())
        .map_err(NoiseError::from)?
        .prologue(&prologue(recipient.public()))
        .map_err(NoiseError::from)?
        .build_responder()
        .map_err(NoiseError::from)?;
    let mut buf = vec![0u8; sealed.len()];
    let n = state
        .read_message(sealed, &mut buf)
        .map_err(NoiseError::from)?;
    if !state.is_handshake_finished() {
        return Err(NoiseError::OutOfOrder.into());
    }
    let remote = state
        .get_remote_static()
        .ok_or(NoiseError::IdentityMismatch)?;
    let mut sender_x25519 = [0u8; 32];
    if remote.len() != 32 {
        return Err(NoiseError::IdentityMismatch.into());
    }
    sender_x25519.copy_from_slice(remote);
    Ok(OpenedEnvelope {
        sender_x25519,
        envelope: decode(&buf[..n])?,
    })
}

/// Opens `sealed`, requires the sender to be one of `peers`, enforces the clock window and
/// records the counter in `guard`. Returns the matching peer.
pub fn open_from_peers<'p>(
    recipient: &DeviceIdentity,
    peers: &'p [PublicIdentity],
    guard: &mut ReplayGuard,
    sealed: &[u8],
    now_ms: u64,
) -> Result<(&'p PublicIdentity, Envelope), CryptoError> {
    let opened = open(recipient, sealed)?;
    let peer = peers
        .iter()
        .find(|p| p.x25519() == &opened.sender_x25519)
        .ok_or(NoiseError::UnknownSender)?;
    if opened.envelope.unix_ms.abs_diff(now_ms) > MAX_CLOCK_SKEW_MS {
        return Err(NoiseError::Stale.into());
    }
    guard.check_and_record(&opened.sender_x25519, opened.envelope.counter)?;
    Ok((peer, opened.envelope))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::identity::IdentitySeed;

    fn identity(byte: u8) -> DeviceIdentity {
        DeviceIdentity::from_seed(&IdentitySeed::from_bytes([byte; 32]))
    }

    fn env(counter: u64, unix_ms: u64, body: &[u8]) -> Envelope {
        Envelope {
            kind: 7,
            counter,
            unix_ms,
            body: body.to_vec(),
        }
    }

    const NOW: u64 = 1_800_000_000_000;

    #[test]
    fn roundtrip_identifies_the_sender() {
        let (alice, bob) = (identity(1), identity(2));
        let message = env(1, NOW, b"wake aa:bb:cc:dd:ee:ff");
        let sealed = seal(&alice, bob.public(), &message).unwrap();
        let opened = open(&bob, &sealed).unwrap();
        assert_eq!(opened.envelope, message);
        assert_eq!(&opened.sender_x25519, alice.public().x25519());
    }

    #[test]
    fn the_relay_cannot_read_the_message() {
        let (alice, bob) = (identity(1), identity(2));
        let secret = b"super secret wake target 00:11:22:33:44:55";
        let sealed = seal(&alice, bob.public(), &env(1, NOW, secret)).unwrap();
        assert!(!sealed.windows(secret.len()).any(|w| w == secret));
        assert!(!sealed.windows(8).any(|w| w == b"wake tar"));
        // Nor does it reveal the sender's static key in the clear.
        assert!(!sealed.windows(32).any(|w| w == alice.public().x25519()));
    }

    #[test]
    fn only_the_addressed_recipient_can_open() {
        let (alice, bob, carol) = (identity(1), identity(2), identity(3));
        let sealed = seal(&alice, bob.public(), &env(1, NOW, b"x")).unwrap();
        assert!(open(&carol, &sealed).is_err());
    }

    #[test]
    fn tampering_is_detected_everywhere() {
        let (alice, bob) = (identity(1), identity(2));
        let sealed = seal(&alice, bob.public(), &env(1, NOW, b"payload")).unwrap();
        for i in 0..sealed.len() {
            let mut bad = sealed.clone();
            bad[i] ^= 1;
            assert!(open(&bob, &bad).is_err(), "bit flip at byte {i} accepted");
        }
        assert!(open(&bob, &sealed[..sealed.len() - 1]).is_err());
        assert!(open(&bob, &[]).is_err());
    }

    #[test]
    fn two_seals_of_the_same_message_differ() {
        let (alice, bob) = (identity(1), identity(2));
        let m = env(1, NOW, b"same");
        assert_ne!(
            seal(&alice, bob.public(), &m).unwrap(),
            seal(&alice, bob.public(), &m).unwrap()
        );
    }

    #[test]
    fn oversize_bodies_are_refused() {
        let (alice, bob) = (identity(1), identity(2));
        assert!(seal(&alice, bob.public(), &env(1, NOW, &vec![0; MAX_BODY + 1])).is_err());
        assert!(seal(&alice, bob.public(), &env(1, NOW, &vec![0; MAX_BODY])).is_ok());
        assert!(open(&bob, &vec![0u8; MAX_SEALED + 1]).is_err());
    }

    #[test]
    fn open_from_peers_enforces_sender_freshness_and_replay() {
        let (alice, bob, mallory) = (identity(1), identity(2), identity(3));
        let peers = [alice.public().clone()];
        let mut guard = ReplayGuard::default();

        // Authorised sender, fresh: accepted exactly once.
        let sealed = seal(&alice, bob.public(), &env(10, NOW, b"hi")).unwrap();
        let (peer, got) = open_from_peers(&bob, &peers, &mut guard, &sealed, NOW + 1_000).unwrap();
        assert_eq!(peer, alice.public());
        assert_eq!(got.body, b"hi");
        assert!(matches!(
            open_from_peers(&bob, &peers, &mut guard, &sealed, NOW + 1_000),
            Err(CryptoError::Noise(NoiseError::Replay))
        ));

        // A device we never paired with.
        let from_mallory = seal(&mallory, bob.public(), &env(1, NOW, b"hi")).unwrap();
        assert!(matches!(
            open_from_peers(&bob, &peers, &mut guard, &from_mallory, NOW),
            Err(CryptoError::Noise(NoiseError::UnknownSender))
        ));

        // Stale (an old capture replayed after a receiver restart) and from-the-future messages.
        let old = seal(&alice, bob.public(), &env(11, NOW, b"old")).unwrap();
        assert!(matches!(
            open_from_peers(&bob, &peers, &mut guard, &old, NOW + MAX_CLOCK_SKEW_MS + 1),
            Err(CryptoError::Noise(NoiseError::Stale))
        ));
        let future = seal(
            &alice,
            bob.public(),
            &env(12, NOW + MAX_CLOCK_SKEW_MS + 1, b"f"),
        )
        .unwrap();
        assert!(matches!(
            open_from_peers(&bob, &peers, &mut guard, &future, NOW),
            Err(CryptoError::Noise(NoiseError::Stale))
        ));
        // A rejected-as-stale message must not burn its counter.
        let (_, ok) = open_from_peers(&bob, &peers, &mut guard, &old, NOW + 10).unwrap();
        assert_eq!(ok.counter, 11);
    }

    #[test]
    fn counters_are_strictly_increasing() {
        let src = CounterSource::new();
        let mut prev = src.next();
        for _ in 0..10_000 {
            let n = src.next();
            assert!(n > prev);
            prev = n;
        }
    }

    #[test]
    fn counters_are_unique_across_threads() {
        let src = std::sync::Arc::new(CounterSource::new());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let s = src.clone();
                std::thread::spawn(move || (0..2_000).map(|_| s.next()).collect::<Vec<_>>())
            })
            .collect();
        let mut all: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total, "duplicate counter issued");
    }

    proptest! {
        #[test]
        fn open_never_panics(data in proptest::collection::vec(any::<u8>(), 0..2000)) {
            let bob = identity(2);
            let _ = open(&bob, &data);
        }

        #[test]
        fn payload_decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..200)) {
            let _ = decode(&data);
        }

        #[test]
        fn roundtrip_arbitrary(kind in any::<u16>(), counter in any::<u64>(), ms in any::<u64>(),
                               body in proptest::collection::vec(any::<u8>(), 0..512)) {
            let (alice, bob) = (identity(1), identity(2));
            let m = Envelope { kind, counter, unix_ms: ms, body };
            let sealed = seal(&alice, bob.public(), &m).unwrap();
            prop_assert_eq!(open(&bob, &sealed).unwrap().envelope, m);
        }
    }
}
