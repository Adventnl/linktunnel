//! Device pairing: a Noise handshake between two devices that have just met.
//!
//! ```text
//!  Initiator (scans the code)                      Responder (shows the code)
//!      │  psk, e                                          │
//!      │ ───────────────────────────────────────────────► │   message 1: empty payload
//!      │       e, ee, s, es   [responder identity]        │
//!      │ ◄─────────────────────────────────────────────── │   message 2
//!      │  s, se               [initiator identity]        │
//!      │ ───────────────────────────────────────────────► │   message 3
//! ```
//!
//! Security properties and where each comes from:
//!
//! * **Only a holder of the one-time code can complete the handshake.** The code is mixed into the
//!   key schedule *first* (`psk0`), so an eavesdropper or the rendezvous server cannot even decrypt
//!   the responder's identity, and cannot mount an offline guessing attack on the code (every
//!   guess needs a live handshake).
//! * **Mutual authentication of long-term keys.** Each side proves possession of its static
//!   X25519 key; the identity payload then binds that key to the device's Ed25519 identity via
//!   the signed key binding. Both are checked in [`PairingSession::finish`].
//! * **Man-in-the-middle detection.** Even if an attacker learned the code (shoulder-surfing the
//!   QR), they would have to run two separate handshakes whose transcripts differ, so the two
//!   humans would see *different* short authentication strings. Pairing is only complete when
//!   both humans confirm the strings match ([`ShortAuthString`]).
//! * **Forward secrecy** from the ephemeral keys; the transcript hash is bound into the SAS.
//!
//! The code has 66 bits of entropy (six words). It is valid for minutes and each guess costs a
//! live round trip rate-limited by the rendezvous server, which makes online guessing
//! impractical.

use std::fmt;

use snow::{Builder, HandshakeState};
use zeroize::Zeroizing;

use super::{NoiseError, words};
use crate::error::CryptoError;
use crate::identity::{DeviceIdentity, PUBLIC_IDENTITY_LEN, PublicIdentity};

/// The exact Noise protocol name.
pub const PAIRING_PROTOCOL: &str = "Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s";

/// Number of words in a pairing code.
pub const CODE_WORDS: usize = 6;
/// Number of words in the short authentication string.
pub const SAS_WORDS: usize = 4;
/// Longest accepted device display name, in bytes.
pub const MAX_DEVICE_NAME: usize = 64;

const KDF_PSK: &str = "nexus-remote 2026-10-11 pairing noise psk";
const KDF_RENDEZVOUS: &str = "nexus-remote 2026-10-11 pairing rendezvous id";
const KDF_SAS: &str = "nexus-remote 2026-10-11 pairing short authentication string";
const PROLOGUE_PREFIX: &[u8] = b"nexus-remote/v1/pairing\0";
const PAYLOAD_VERSION: u8 = 1;
/// Noise messages are limited to 65535 bytes; ours are a few hundred.
const MAX_MESSAGE: usize = 1024;
/// Messages in an `XX` handshake.
const HANDSHAKE_MESSAGES: u8 = 3;

/// The one-time secret shown by the device that wants to be paired.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode {
    indices: [u16; CODE_WORDS],
}

impl PairingCode {
    /// Draws a fresh random code.
    pub fn generate() -> Result<Self, CryptoError> {
        let mut bytes = Zeroizing::new([0u8; 9]);
        getrandom::fill(&mut bytes[..]).map_err(|e| CryptoError::Rng(e.to_string()))?;
        let mut indices = [0u16; CODE_WORDS];
        let drawn = words::indices_from_bytes(&bytes[..], CODE_WORDS)
            .ok_or(CryptoError::Invalid("pairing code entropy"))?;
        indices.copy_from_slice(&drawn);
        Ok(Self { indices })
    }

    /// Parses a code typed or scanned as words separated by spaces, hyphens, commas or newlines.
    pub fn parse(text: &str) -> Result<Self, CryptoError> {
        let parts: Vec<&str> = text
            .split(|c: char| c.is_whitespace() || matches!(c, '-' | ',' | '.' | '_' | '+'))
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() != CODE_WORDS {
            return Err(NoiseError::BadCode("expected exactly six words").into());
        }
        let mut indices = [0u16; CODE_WORDS];
        for (slot, part) in indices.iter_mut().zip(parts) {
            *slot = words::index_of(part)
                .ok_or(CryptoError::Noise(NoiseError::BadCode("unknown word")))?;
        }
        Ok(Self { indices })
    }

    /// The six words, in order.
    #[must_use]
    pub fn words(&self) -> [&'static str; CODE_WORDS] {
        self.indices.map(words::word)
    }

    fn secret(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(words::pack_indices(&self.indices))
    }

    /// Identifier both devices use to meet at the rendezvous server. It is a one-way hash of the
    /// code, so the server can match the two parties without learning the code.
    #[must_use]
    pub fn rendezvous_id(&self) -> [u8; 16] {
        let digest = blake3::derive_key(KDF_RENDEZVOUS, &self.secret());
        let mut id = [0u8; 16];
        id.copy_from_slice(&digest[..16]);
        id
    }

    fn psk(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(blake3::derive_key(KDF_PSK, &self.secret()))
    }
}

impl fmt::Display for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.words().join(" "))
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode([redacted])")
    }
}

/// Four words (44 bits) that both humans must see identically before pairing is accepted.
#[derive(Clone, Copy)]
pub struct ShortAuthString([u8; 6]);

impl ShortAuthString {
    /// The four words.
    #[must_use]
    pub fn words(&self) -> [&'static str; SAS_WORDS] {
        let indices = words::indices_from_bytes(&self.0, SAS_WORDS).unwrap_or_default();
        let mut out = [words::word(0); SAS_WORDS];
        for (slot, index) in out.iter_mut().zip(indices) {
            *slot = words::word(index);
        }
        out
    }

    /// A six-digit alternative for UIs that prefer numbers.
    #[must_use]
    pub fn digits(&self) -> String {
        let n = u32::from_be_bytes([0, self.0[0], self.0[1], self.0[2]]) >> 4;
        format!("{:06}", n % 1_000_000)
    }
}

impl PartialEq for ShortAuthString {
    fn eq(&self, other: &Self) -> bool {
        // Not secret, but there is no reason to leak timing either.
        subtle::ConstantTimeEq::ct_eq(&self.0[..], &other.0[..]).into()
    }
}

impl Eq for ShortAuthString {}

impl fmt::Display for ShortAuthString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.words().join(" "))
    }
}

impl fmt::Debug for ShortAuthString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ShortAuthString({self})")
    }
}

/// Which end of the handshake this device plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Starts the handshake (the device that scanned / typed the code).
    Initiator,
    /// Answers (the device that displayed the code).
    Responder,
}

/// What a successful handshake yields. **Not trusted yet**: the caller must obtain human
/// confirmation of [`sas`](Self::sas) before recording the pairing.
#[derive(Debug, Clone)]
pub struct PairingOutcome {
    /// The peer's verified public identity.
    pub peer: PublicIdentity,
    /// The peer's self-declared display name (untrusted text; sanitise before display).
    pub peer_name: String,
    /// Compare this with the other device.
    pub sas: ShortAuthString,
}

/// A pairing handshake in progress.
pub struct PairingSession {
    role: Role,
    state: HandshakeState,
    local_payload: Vec<u8>,
    peer_payload: Option<Vec<u8>>,
    /// Handshake messages processed so far (sent or received): 0..=3.
    step: u8,
}

impl fmt::Debug for PairingSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingSession")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

fn encode_payload(identity: &PublicIdentity, name: &str) -> Result<Vec<u8>, CryptoError> {
    if name.len() > MAX_DEVICE_NAME {
        return Err(NoiseError::TooLarge {
            len: name.len(),
            max: MAX_DEVICE_NAME,
        }
        .into());
    }
    let name_len = u8::try_from(name.len()).map_err(|_| NoiseError::BadPayload)?;
    let mut out = Vec::with_capacity(2 + PUBLIC_IDENTITY_LEN + name.len());
    out.push(PAYLOAD_VERSION);
    out.extend_from_slice(&identity.to_bytes());
    out.push(name_len);
    out.extend_from_slice(name.as_bytes());
    Ok(out)
}

fn decode_payload(payload: &[u8]) -> Result<(PublicIdentity, String), CryptoError> {
    let min = 2 + PUBLIC_IDENTITY_LEN;
    if payload.len() < min || payload[0] != PAYLOAD_VERSION {
        return Err(NoiseError::BadPayload.into());
    }
    let identity = PublicIdentity::from_bytes(&payload[1..=PUBLIC_IDENTITY_LEN])?;
    let name_len = usize::from(payload[1 + PUBLIC_IDENTITY_LEN]);
    let name_bytes = &payload[min..];
    if name_len > MAX_DEVICE_NAME || name_bytes.len() != name_len {
        return Err(NoiseError::BadPayload.into());
    }
    let name = std::str::from_utf8(name_bytes).map_err(|_| NoiseError::BadPayload)?;
    Ok((identity, name.to_owned()))
}

impl PairingSession {
    /// Starts a handshake as `role`, presenting `identity` under the display name `device_name`.
    pub fn new(
        role: Role,
        identity: &DeviceIdentity,
        device_name: &str,
        code: &PairingCode,
    ) -> Result<Self, CryptoError> {
        let local_payload = encode_payload(identity.public(), device_name)?;

        let mut prologue = Vec::with_capacity(PROLOGUE_PREFIX.len() + 16);
        prologue.extend_from_slice(PROLOGUE_PREFIX);
        prologue.extend_from_slice(&code.rendezvous_id());

        let params = PAIRING_PROTOCOL.parse().map_err(NoiseError::from)?;
        let psk = code.psk();
        let builder = Builder::new(params)
            .local_private_key(identity.noise_secret())
            .map_err(NoiseError::from)?
            .psk(0, &psk)
            .map_err(NoiseError::from)?
            .prologue(&prologue)
            .map_err(NoiseError::from)?;
        let state = match role {
            Role::Initiator => builder.build_initiator(),
            Role::Responder => builder.build_responder(),
        }
        .map_err(NoiseError::from)?;

        Ok(Self {
            role,
            state,
            local_payload,
            peer_payload: None,
            step: 0,
        })
    }

    /// True while it is this side's turn to send.
    ///
    /// `XX` has three messages: the initiator sends the 1st and 3rd, the responder the 2nd.
    #[must_use]
    pub fn my_turn(&self) -> bool {
        matches!(
            (self.role, self.step),
            (Role::Initiator, 0 | 2) | (Role::Responder, 1)
        )
    }

    /// True once all three messages have been exchanged.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.step == HANDSHAKE_MESSAGES && self.state.is_handshake_finished()
    }

    /// Produces this side's next handshake message. Fails if it is not our turn.
    pub fn next_message(&mut self) -> Result<Vec<u8>, CryptoError> {
        if !self.my_turn() {
            return Err(NoiseError::OutOfOrder.into());
        }
        // Message 1 carries no identity: nothing is encrypted yet except by the PSK, and an
        // empty first payload keeps the initiator's identity private until the responder has
        // proven it knows the code.
        let payload: &[u8] = if self.step == 0 {
            &[]
        } else {
            &self.local_payload
        };
        let mut buf = vec![0u8; MAX_MESSAGE];
        let n = self
            .state
            .write_message(payload, &mut buf)
            .map_err(NoiseError::from)?;
        buf.truncate(n);
        self.step += 1;
        Ok(buf)
    }

    /// Consumes a message from the peer.
    pub fn handle_message(&mut self, message: &[u8]) -> Result<(), CryptoError> {
        if self.my_turn() || self.step >= HANDSHAKE_MESSAGES {
            return Err(NoiseError::OutOfOrder.into());
        }
        if message.len() > MAX_MESSAGE {
            return Err(NoiseError::TooLarge {
                len: message.len(),
                max: MAX_MESSAGE,
            }
            .into());
        }
        let mut buf = vec![0u8; MAX_MESSAGE];
        let n = self
            .state
            .read_message(message, &mut buf)
            .map_err(NoiseError::from)?;
        // Only messages 2 and 3 carry the peer's identity.
        if self.step >= 1 {
            if n == 0 {
                return Err(NoiseError::BadPayload.into());
            }
            self.peer_payload = Some(buf[..n].to_vec());
        }
        self.step += 1;
        Ok(())
    }

    /// Completes the handshake, verifying the peer's identity and deriving the SAS.
    pub fn finish(self) -> Result<PairingOutcome, CryptoError> {
        self.finish_with_channel()
            .map(|(outcome, _channel)| outcome)
    }

    /// Like [`finish`](Self::finish) but also returns the encrypted channel the two devices use for
    /// the rest of the pairing exchange (SAS confirmation, record signing).
    pub fn finish_with_channel(self) -> Result<(PairingOutcome, PairingChannel), CryptoError> {
        if !self.is_complete() {
            return Err(NoiseError::OutOfOrder.into());
        }
        let payload = self.peer_payload.as_deref().ok_or(NoiseError::BadPayload)?;
        let (peer, peer_name) = decode_payload(payload)?;

        // The Noise-authenticated static key must be the one the identity binds to its Ed25519
        // key; otherwise a peer could present someone else's identity blob.
        let remote_static = self
            .state
            .get_remote_static()
            .ok_or(NoiseError::IdentityMismatch)?;
        if remote_static != peer.x25519() {
            return Err(NoiseError::IdentityMismatch.into());
        }

        let digest = blake3::derive_key(KDF_SAS, self.state.get_handshake_hash());
        let mut sas = [0u8; 6];
        sas.copy_from_slice(&digest[..6]);
        let transport = self.state.into_transport_mode().map_err(NoiseError::from)?;
        Ok((
            PairingOutcome {
                peer,
                peer_name,
                sas: ShortAuthString(sas),
            },
            PairingChannel { state: transport },
        ))
    }
}

/// Largest plaintext accepted on the pairing channel.
pub const MAX_CHANNEL_PLAINTEXT: usize = 8 * 1024;

/// The authenticated, encrypted channel that follows a completed pairing handshake.
///
/// Messages must be decrypted in the order they were encrypted (the Noise nonce is an implicit
/// counter), which the rendezvous relay preserves; a dropped, duplicated or reordered message
/// makes decryption fail, which the pairing flow treats as fatal.
pub struct PairingChannel {
    state: snow::TransportState,
}

impl fmt::Debug for PairingChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingChannel")
    }
}

impl PairingChannel {
    /// Encrypts `plaintext` for the peer.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if plaintext.len() > MAX_CHANNEL_PLAINTEXT {
            return Err(NoiseError::TooLarge {
                len: plaintext.len(),
                max: MAX_CHANNEL_PLAINTEXT,
            }
            .into());
        }
        let mut out = vec![0u8; plaintext.len() + 16];
        let n = self
            .state
            .write_message(plaintext, &mut out)
            .map_err(NoiseError::from)?;
        out.truncate(n);
        Ok(out)
    }

    /// Decrypts and authenticates a message from the peer.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let max = MAX_CHANNEL_PLAINTEXT + 16;
        if ciphertext.len() > max {
            return Err(NoiseError::TooLarge {
                len: ciphertext.len(),
                max,
            }
            .into());
        }
        let mut out = vec![0u8; ciphertext.len()];
        let n = self
            .state
            .read_message(ciphertext, &mut out)
            .map_err(NoiseError::from)?;
        out.truncate(n);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::identity::{DeviceIdentity, IdentitySeed};

    fn identity(byte: u8) -> DeviceIdentity {
        DeviceIdentity::from_seed(&IdentitySeed::from_bytes([byte; 32]))
    }

    /// Drives a complete honest handshake and returns both outcomes.
    fn run(
        a: &DeviceIdentity,
        a_code: &PairingCode,
        b: &DeviceIdentity,
        b_code: &PairingCode,
    ) -> Result<(PairingOutcome, PairingOutcome), CryptoError> {
        let mut initiator = PairingSession::new(Role::Initiator, a, "MacBook", a_code)?;
        let mut responder = PairingSession::new(Role::Responder, b, "Windows PC", b_code)?;
        let m1 = initiator.next_message()?;
        responder.handle_message(&m1)?;
        let m2 = responder.next_message()?;
        initiator.handle_message(&m2)?;
        let m3 = initiator.next_message()?;
        responder.handle_message(&m3)?;
        assert!(initiator.is_complete() && responder.is_complete());
        Ok((initiator.finish()?, responder.finish()?))
    }

    #[test]
    fn honest_pairing_authenticates_both_sides_and_sas_matches() {
        let (a, b) = (identity(1), identity(2));
        let code = PairingCode::generate().unwrap();
        let (a_out, b_out) = run(&a, &code, &b, &code).unwrap();

        assert_eq!(a_out.peer, *b.public());
        assert_eq!(b_out.peer, *a.public());
        assert_eq!(a_out.peer_name, "Windows PC");
        assert_eq!(b_out.peer_name, "MacBook");
        assert_eq!(a_out.sas, b_out.sas, "both humans must see the same SAS");
        assert_eq!(a_out.sas.words(), b_out.sas.words());
        assert_eq!(a_out.sas.digits().len(), 6);
        assert!(a_out.sas.digits().chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn channel_carries_authenticated_messages_after_pairing() {
        let (a, b) = (identity(1), identity(2));
        let code = PairingCode::generate().unwrap();
        let mut i = PairingSession::new(Role::Initiator, &a, "i", &code).unwrap();
        let mut r = PairingSession::new(Role::Responder, &b, "r", &code).unwrap();
        let m1 = i.next_message().unwrap();
        r.handle_message(&m1).unwrap();
        let m2 = r.next_message().unwrap();
        i.handle_message(&m2).unwrap();
        let m3 = i.next_message().unwrap();
        r.handle_message(&m3).unwrap();
        let (_, mut ic) = i.finish_with_channel().unwrap();
        let (_, mut rc) = r.finish_with_channel().unwrap();

        let c1 = ic.encrypt(b"hello responder").unwrap();
        let c2 = ic.encrypt(b"second").unwrap();
        assert_eq!(rc.decrypt(&c1).unwrap(), b"hello responder");
        assert_eq!(rc.decrypt(&c2).unwrap(), b"second");
        assert_eq!(
            ic.decrypt(&rc.encrypt(b"reply").unwrap()).unwrap(),
            b"reply"
        );

        // Replay and reordering are fatal, not silently accepted.
        assert!(rc.decrypt(&c1).is_err(), "replayed message accepted");
        let c3 = ic.encrypt(b"third").unwrap();
        let c4 = ic.encrypt(b"fourth").unwrap();
        assert!(rc.decrypt(&c4).is_err(), "out-of-order message accepted");
        // A rejected message must not desynchronise the channel for the legitimate sequence.
        assert_eq!(rc.decrypt(&c3).unwrap(), b"third");
        assert!(ic.encrypt(&vec![0; MAX_CHANNEL_PLAINTEXT + 1]).is_err());
    }

    #[test]
    fn different_sessions_have_different_sas() {
        let (a, b) = (identity(1), identity(2));
        let code = PairingCode::generate().unwrap();
        let (x, _) = run(&a, &code, &b, &code).unwrap();
        let (y, _) = run(&a, &code, &b, &code).unwrap();
        assert_ne!(
            x.sas, y.sas,
            "fresh ephemeral keys must change the transcript"
        );
    }

    #[test]
    fn wrong_code_fails_at_the_first_message() {
        let (a, b) = (identity(1), identity(2));
        let right = PairingCode::generate().unwrap();
        let mut wrong = PairingCode::generate().unwrap();
        while wrong == right {
            wrong = PairingCode::generate().unwrap();
        }
        // The responder cannot even decrypt message 1 without the code: an attacker (or the
        // rendezvous server) learns nothing and gets no offline-guessing oracle.
        assert!(run(&a, &right, &b, &wrong).is_err());
    }

    #[test]
    fn man_in_the_middle_who_knows_the_code_produces_different_sas() {
        // The attacker saw the QR code, so the Noise handshake itself cannot stop them; the
        // human SAS comparison must.
        let (alice, bob, mallory) = (identity(1), identity(2), identity(3));
        let code = PairingCode::generate().unwrap();
        let (alice_view, _) = run(&alice, &code, &mallory, &code).unwrap(); // Alice ↔ Mallory
        let (_, bob_view) = run(&mallory, &code, &bob, &code).unwrap(); // Mallory ↔ Bob
        assert_ne!(
            alice_view.sas, bob_view.sas,
            "MITM must be visible to the humans"
        );
        // And Alice would pair with *Mallory's* key, not Bob's: confirming the SAS is what
        // protects her.
        assert_eq!(alice_view.peer, *mallory.public());
    }

    #[test]
    fn tampering_with_any_message_is_detected() {
        let (a, b) = (identity(1), identity(2));
        let code = PairingCode::generate().unwrap();
        for target in 0..3 {
            let mut initiator = PairingSession::new(Role::Initiator, &a, "a", &code).unwrap();
            let mut responder = PairingSession::new(Role::Responder, &b, "b", &code).unwrap();
            let mut result = Ok(());
            let mut m = initiator.next_message().unwrap();
            for step in 0..3 {
                if step == target {
                    let last = m.len() - 1;
                    m[last] ^= 0x80;
                }
                let (sender, receiver) = if step % 2 == 0 {
                    (&mut initiator, &mut responder)
                } else {
                    (&mut responder, &mut initiator)
                };
                let _ = sender;
                result = receiver.handle_message(&m);
                if result.is_err() {
                    break;
                }
                if step < 2 {
                    m = receiver.next_message().unwrap();
                }
            }
            assert!(
                result.is_err(),
                "tampering with message {} went unnoticed",
                target + 1
            );
        }
    }

    #[test]
    fn out_of_order_use_is_rejected() {
        let a = identity(1);
        let code = PairingCode::generate().unwrap();
        let mut responder = PairingSession::new(Role::Responder, &a, "r", &code).unwrap();
        assert!(
            responder.next_message().is_err(),
            "responder cannot speak first"
        );
        let mut initiator = PairingSession::new(Role::Initiator, &a, "i", &code).unwrap();
        assert!(
            initiator.handle_message(&[0; 48]).is_err(),
            "initiator cannot listen first"
        );
        assert!(
            initiator.finish().is_err(),
            "cannot finish before the handshake completes"
        );
    }

    #[test]
    fn identity_blob_not_matching_the_noise_key_is_rejected() {
        // A malicious responder presents someone else's identity while using its own Noise key.
        let (a, b, victim) = (identity(1), identity(2), identity(9));
        let code = PairingCode::generate().unwrap();
        let mut initiator = PairingSession::new(Role::Initiator, &a, "a", &code).unwrap();
        let mut responder = PairingSession::new(Role::Responder, &b, "b", &code).unwrap();
        responder.local_payload = encode_payload(victim.public(), "victim").unwrap();

        let m1 = initiator.next_message().unwrap();
        responder.handle_message(&m1).unwrap();
        let m2 = responder.next_message().unwrap();
        initiator.handle_message(&m2).unwrap();
        let m3 = initiator.next_message().unwrap();
        responder.handle_message(&m3).unwrap();
        assert!(matches!(
            initiator.finish(),
            Err(CryptoError::Noise(NoiseError::IdentityMismatch))
        ));
    }

    #[test]
    fn payload_codec_rejects_malformed_input() {
        let id = identity(1);
        let good = encode_payload(id.public(), "name").unwrap();
        assert!(decode_payload(&good).is_ok());
        assert!(
            decode_payload(&good[..good.len() - 1]).is_err(),
            "truncated name"
        );
        assert!(
            decode_payload(&[good.as_slice(), &[0]].concat()).is_err(),
            "trailing byte"
        );
        assert!(decode_payload(&[]).is_err());
        let mut bad_version = good.clone();
        bad_version[0] = 9;
        assert!(decode_payload(&bad_version).is_err());
        let mut bad_utf8 = good.clone();
        let last = bad_utf8.len() - 1;
        bad_utf8[last] = 0xff;
        assert!(decode_payload(&bad_utf8).is_err());
        assert!(encode_payload(id.public(), &"x".repeat(MAX_DEVICE_NAME + 1)).is_err());
    }

    #[test]
    fn code_text_roundtrip_and_leniency() {
        let code = PairingCode::generate().unwrap();
        let text = code.to_string();
        assert_eq!(text.split(' ').count(), CODE_WORDS);
        assert_eq!(PairingCode::parse(&text).unwrap(), code);
        assert_eq!(PairingCode::parse(&text.to_uppercase()).unwrap(), code);
        assert_eq!(PairingCode::parse(&text.replace(' ', "-")).unwrap(), code);
        assert_eq!(
            PairingCode::parse(&format!("  {}\n", text.replace(' ', ",  "))).unwrap(),
            code
        );
    }

    #[test]
    fn code_text_rejects_bad_input() {
        for bad in [
            "",
            "abandon",
            "abandon ability able about above",
            "a b c d e f",
            "abandon ability able about above absent absorb",
        ] {
            assert!(PairingCode::parse(bad).is_err(), "`{bad}` accepted");
        }
        assert!(PairingCode::parse("abandon ability able about above nexus").is_err());
    }

    /// Known-answer test with values computed independently in Python (`blake3`).
    #[test]
    fn code_derivations_known_answer() {
        let code = PairingCode::parse("abandon ability able about above absent").unwrap();
        assert_eq!(
            data_encoding::HEXLOWER.encode(&code.rendezvous_id()),
            "9c2ad9ff1b0abfdc4a10165c82fbb92e"
        );
        assert_eq!(
            data_encoding::HEXLOWER.encode(&*code.psk()),
            "b161d011cf525bc1393176b28a68d29cdfaa407f1f1334ee3358ba2c56408e54"
        );
    }

    #[test]
    fn distinct_codes_have_distinct_rendezvous_ids() {
        let a = PairingCode::generate().unwrap();
        let b = PairingCode::generate().unwrap();
        assert_ne!(a.rendezvous_id(), b.rendezvous_id());
    }

    #[test]
    fn debug_output_hides_the_code() {
        let code = PairingCode::generate().unwrap();
        let dbg = format!("{code:?}");
        assert!(dbg.contains("redacted"));
        for w in code.words() {
            assert!(!dbg.contains(w));
        }
    }

    proptest! {
        #[test]
        fn handle_message_never_panics(data in proptest::collection::vec(any::<u8>(), 0..1100)) {
            let a = identity(1);
            let code = PairingCode::parse("abandon ability able about above absent").unwrap();
            let mut s = PairingSession::new(Role::Responder, &a, "x", &code).unwrap();
            let _ = s.handle_message(&data);
        }

        #[test]
        fn code_parse_never_panics(text in "\\PC{0,120}") {
            let _ = PairingCode::parse(&text);
        }
    }
}
