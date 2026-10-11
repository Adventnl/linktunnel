//! Dual-signed pairing records.
//!
//! A pairing is valid only if **both** devices signed the exact same bytes. The signaling server
//! checks this before storing a record, and each agent checks it again before trusting a peer, so
//! neither the server nor a network attacker can invent a trust relationship.

use nexus_crypto::{DeviceIdentity, PublicIdentity, SignContext, Signature, verify_with_key};
use nexus_proto::v1::{PairingRecord, SignedPairingRecord};
use prost::Message;

use crate::error::TrustError;
use crate::grants;

/// Longest accepted display name.
pub const MAX_NAME_LEN: usize = 64;
/// Most grants per direction.
pub const MAX_GRANTS: usize = 32;
/// How far in the future a record's timestamp may be (clock skew allowance).
pub const MAX_FUTURE_SKEW_MS: u64 = 10 * 60 * 1000;
/// A record must be registered within this long of being created. Bounds the damage of a signed
/// record that one party holds back and registers much later.
pub const MAX_REGISTRATION_AGE_MS: u64 = 15 * 60 * 1000;
/// Largest serialised record we will parse.
pub const MAX_RECORD_BYTES: usize = 4096;

/// A record whose identities and signatures have all been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPairing {
    /// The device that sorts first by Ed25519 key.
    pub a: PublicIdentity,
    /// The other device.
    pub b: PublicIdentity,
    /// Display name `a` gave itself.
    pub name_a: String,
    /// Display name `b` gave itself.
    pub name_b: String,
    /// When the record was created (the signers' clocks).
    pub created_unix_ms: u64,
    /// What `a` may do on `b`.
    pub grants_a_to_b: Vec<String>,
    /// What `b` may do on `a`.
    pub grants_b_to_a: Vec<String>,
    /// The signed container, unchanged, for storage and forwarding.
    pub signed: SignedPairingRecord,
}

impl VerifiedPairing {
    /// The peer of `me`, or `None` if `me` is neither party.
    #[must_use]
    pub fn peer_of(&self, me: &[u8; 32]) -> Option<&PublicIdentity> {
        if &self.a.ed25519_bytes() == me {
            Some(&self.b)
        } else if &self.b.ed25519_bytes() == me {
            Some(&self.a)
        } else {
            None
        }
    }

    /// `(my_name_for_peer, peer_name, grants I have on the peer, grants the peer has on me)`
    /// from the point of view of `me`.
    #[must_use]
    pub fn view_for(&self, me: &[u8; 32]) -> Option<PairingView> {
        if &self.a.ed25519_bytes() == me {
            Some(PairingView {
                peer: self.b.clone(),
                peer_name: self.name_b.clone(),
                grants_on_peer: self.grants_a_to_b.clone(),
                grants_from_peer: self.grants_b_to_a.clone(),
            })
        } else if &self.b.ed25519_bytes() == me {
            Some(PairingView {
                peer: self.a.clone(),
                peer_name: self.name_a.clone(),
                grants_on_peer: self.grants_b_to_a.clone(),
                grants_from_peer: self.grants_a_to_b.clone(),
            })
        } else {
            None
        }
    }
}

/// A [`VerifiedPairing`] seen from one of its two parties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingView {
    /// The other device's identity.
    pub peer: PublicIdentity,
    /// The other device's display name.
    pub peer_name: String,
    /// What the viewing device may do on the peer.
    pub grants_on_peer: Vec<String>,
    /// What the peer may do on the viewing device.
    pub grants_from_peer: Vec<String>,
}

/// Builds the canonical record bytes for a pairing between `local` and `peer`.
///
/// Both devices must produce/accept *byte-identical* output, so the party that sorts first is
/// always `a`, and callers pass the record around as bytes rather than re-encoding it.
pub fn build_record(
    local: &PublicIdentity,
    local_name: &str,
    peer: &PublicIdentity,
    peer_name: &str,
    grants_local_on_peer: &[String],
    grants_peer_on_local: &[String],
    created_unix_ms: u64,
) -> Result<Vec<u8>, TrustError> {
    let local_first = local.ed25519_bytes() < peer.ed25519_bytes();
    let (a, b, name_a, name_b, a_to_b, b_to_a) = if local_first {
        (
            local,
            peer,
            local_name,
            peer_name,
            grants_local_on_peer,
            grants_peer_on_local,
        )
    } else {
        (
            peer,
            local,
            peer_name,
            local_name,
            grants_peer_on_local,
            grants_local_on_peer,
        )
    };
    let record = PairingRecord {
        identity_a: a.to_bytes().to_vec().into(),
        identity_b: b.to_bytes().to_vec().into(),
        name_a: name_a.to_owned(),
        name_b: name_b.to_owned(),
        created_unix_ms,
        grants_a_to_b: a_to_b.to_vec(),
        grants_b_to_a: b_to_a.to_vec(),
    };
    validate_fields(&record)?;
    Ok(record.encode_to_vec())
}

/// Signs `record_bytes` as `identity` (context `PairingRecord`).
#[must_use]
pub fn sign_record(identity: &DeviceIdentity, record_bytes: &[u8]) -> Signature {
    identity.sign(SignContext::PairingRecord, record_bytes)
}

/// Puts the two signatures into the right slots. `record_bytes` must decode and verify; the
/// signature order follows the record's canonical `a`/`b` ordering.
pub fn assemble(
    record_bytes: &[u8],
    local: &PublicIdentity,
    local_signature: &Signature,
    peer_signature: &Signature,
) -> Result<SignedPairingRecord, TrustError> {
    let record = decode_record(record_bytes)?;
    let local_is_a = record.identity_a.as_ref() == local.to_bytes().as_slice();
    let (sig_a, sig_b) = if local_is_a {
        (local_signature, peer_signature)
    } else {
        (peer_signature, local_signature)
    };
    Ok(SignedPairingRecord {
        record: record_bytes.to_vec().into(),
        signature_a: sig_a.to_bytes().to_vec().into(),
        signature_b: sig_b.to_bytes().to_vec().into(),
    })
}

fn decode_record(bytes: &[u8]) -> Result<PairingRecord, TrustError> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(TrustError::Invalid("record size"));
    }
    PairingRecord::decode(bytes).map_err(|_| TrustError::Malformed("pairing record"))
}

fn validate_fields(record: &PairingRecord) -> Result<(), TrustError> {
    for name in [&record.name_a, &record.name_b] {
        if name.len() > MAX_NAME_LEN || name.chars().any(char::is_control) {
            return Err(TrustError::Invalid("device name"));
        }
    }
    for list in [&record.grants_a_to_b, &record.grants_b_to_a] {
        if list.len() > MAX_GRANTS || !list.iter().all(|g| grants::is_valid(g)) {
            return Err(TrustError::Invalid("grants"));
        }
    }
    Ok(())
}

fn signature_from(bytes: &[u8]) -> Result<Signature, TrustError> {
    let array: [u8; 64] = bytes
        .try_into()
        .map_err(|_| TrustError::Malformed("signature"))?;
    Ok(Signature::from_bytes(&array))
}

/// A record that parsed and passed every structural check, but whose signatures are *not yet*
/// verified. Used by the pairing flow, where each party sees the other's signature first.
#[derive(Debug, Clone)]
pub struct ParsedRecord {
    /// The decoded record.
    pub record: PairingRecord,
    /// The party that sorts first.
    pub a: PublicIdentity,
    /// The other party.
    pub b: PublicIdentity,
}

/// Parses record bytes and checks field limits, both identities (key bindings, weak keys) and the
/// canonical `a < b` ordering. Does not check signatures.
pub fn parse_unsigned(bytes: &[u8]) -> Result<ParsedRecord, TrustError> {
    let record = decode_record(bytes)?;
    validate_fields(&record)?;
    let a = PublicIdentity::from_bytes(&record.identity_a)?;
    let b = PublicIdentity::from_bytes(&record.identity_b)?;
    if a.ed25519_bytes() >= b.ed25519_bytes() {
        // Also rejects a == b (pairing with yourself).
        return Err(TrustError::Invalid("party ordering"));
    }
    Ok(ParsedRecord { record, a, b })
}

/// Verifies one party's signature over `record_bytes`.
pub fn verify_signature(
    signer: &PublicIdentity,
    record_bytes: &[u8],
    signature: &[u8],
    who: &'static str,
) -> Result<(), TrustError> {
    verify_with_key(
        signer.ed25519(),
        SignContext::PairingRecord,
        record_bytes,
        &signature_from(signature)?,
    )
    .map_err(|_| TrustError::BadSignature(who))
}

/// Fully verifies a signed record: identities, canonical ordering, field limits and **both**
/// signatures. Time is checked separately by [`check_fresh`] because agents re-verify stored
/// records long after creation.
pub fn verify(signed: &SignedPairingRecord) -> Result<VerifiedPairing, TrustError> {
    let ParsedRecord { record, a, b } = parse_unsigned(&signed.record)?;
    verify_signature(&a, &signed.record, &signed.signature_a, "party A")?;
    verify_signature(&b, &signed.record, &signed.signature_b, "party B")?;
    Ok(VerifiedPairing {
        a,
        b,
        name_a: record.name_a,
        name_b: record.name_b,
        created_unix_ms: record.created_unix_ms,
        grants_a_to_b: record.grants_a_to_b,
        grants_b_to_a: record.grants_b_to_a,
        signed: signed.clone(),
    })
}

/// Rejects records created too far in the future or too long ago for *registration*.
pub fn check_fresh(pairing: &VerifiedPairing, now_ms: u64) -> Result<(), TrustError> {
    if pairing.created_unix_ms > now_ms.saturating_add(MAX_FUTURE_SKEW_MS)
        || now_ms.saturating_sub(pairing.created_unix_ms) > MAX_REGISTRATION_AGE_MS
    {
        return Err(TrustError::TimestampOutOfRange);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use nexus_crypto::IdentitySeed;

    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn id(b: u8) -> DeviceIdentity {
        DeviceIdentity::from_seed(&IdentitySeed::from_bytes([b; 32]))
    }

    fn signed_between(x: &DeviceIdentity, y: &DeviceIdentity) -> SignedPairingRecord {
        let bytes = build_record(
            x.public(),
            "X",
            y.public(),
            "Y",
            &grants::all(),
            &[grants::SCREEN.to_string()],
            NOW,
        )
        .unwrap();
        // The peer builds the *same* bytes from its own point of view.
        let bytes_y = build_record(
            y.public(),
            "Y",
            x.public(),
            "X",
            &[grants::SCREEN.to_string()],
            &grants::all(),
            NOW,
        )
        .unwrap();
        assert_eq!(
            bytes, bytes_y,
            "both sides must produce byte-identical records"
        );
        let sig_x = sign_record(x, &bytes);
        let sig_y = sign_record(y, &bytes);
        assemble(&bytes, x.public(), &sig_x, &sig_y).unwrap()
    }

    #[test]
    fn both_directions_build_the_same_canonical_bytes_and_verify() {
        let (x, y) = (id(1), id(2));
        let signed = signed_between(&x, &y);
        let v = verify(&signed).unwrap();
        check_fresh(&v, NOW + 1000).unwrap();
        assert!(v.a.ed25519_bytes() < v.b.ed25519_bytes());
        // Views are mirror images.
        let vx = v.view_for(&x.public().ed25519_bytes()).unwrap();
        let vy = v.view_for(&y.public().ed25519_bytes()).unwrap();
        assert_eq!(vx.peer, *y.public());
        assert_eq!(vy.peer, *x.public());
        assert_eq!(vx.grants_on_peer, vy.grants_from_peer);
        assert_eq!(vx.grants_from_peer, vy.grants_on_peer);
        assert_eq!(vx.peer_name, "Y");
        assert_eq!(vy.peer_name, "X");
        assert!(v.view_for(&id(9).public().ed25519_bytes()).is_none());
    }

    #[test]
    fn assemble_puts_signatures_in_canonical_slots_regardless_of_caller() {
        let (x, y) = (id(1), id(2));
        let bytes = build_record(x.public(), "X", y.public(), "Y", &[], &[], NOW).unwrap();
        let (sx, sy) = (sign_record(&x, &bytes), sign_record(&y, &bytes));
        let from_x = assemble(&bytes, x.public(), &sx, &sy).unwrap();
        let from_y = assemble(&bytes, y.public(), &sy, &sx).unwrap();
        assert_eq!(from_x, from_y);
        verify(&from_x).unwrap();
    }

    #[test]
    fn a_single_signature_is_not_enough() {
        let (x, y) = (id(1), id(2));
        let bytes = build_record(x.public(), "X", y.public(), "Y", &[], &[], NOW).unwrap();
        let sx = sign_record(&x, &bytes);
        // Party Y never signed: the server must not accept a self-declared pairing.
        let forged = assemble(&bytes, x.public(), &sx, &sx).unwrap();
        assert!(matches!(verify(&forged), Err(TrustError::BadSignature(_))));
    }

    #[test]
    fn tampering_with_the_record_invalidates_both_signatures() {
        let (x, y) = (id(1), id(2));
        let mut signed = signed_between(&x, &y);
        let mut rec = PairingRecord::decode(signed.record.as_ref()).unwrap();
        rec.grants_a_to_b.push("power".into()); // privilege escalation attempt
        signed.record = rec.encode_to_vec().into();
        assert!(matches!(verify(&signed), Err(TrustError::BadSignature(_))));
    }

    #[test]
    fn a_third_party_cannot_forge_a_pairing_between_two_victims() {
        let (x, y, mallory) = (id(1), id(2), id(3));
        let bytes = build_record(
            x.public(),
            "X",
            y.public(),
            "Y",
            &grants::all(),
            &grants::all(),
            NOW,
        )
        .unwrap();
        let forged = assemble(
            &bytes,
            x.public(),
            &sign_record(&mallory, &bytes),
            &sign_record(&mallory, &bytes),
        )
        .unwrap();
        assert!(verify(&forged).is_err());
    }

    #[test]
    fn signatures_cannot_be_transplanted_between_contexts() {
        // A signature by X made for another purpose (a revocation, say) over the same bytes must
        // not satisfy a pairing record.
        let (x, y) = (id(1), id(2));
        let bytes = build_record(x.public(), "X", y.public(), "Y", &[], &[], NOW).unwrap();
        let wrong = x.sign(SignContext::Revocation, &bytes);
        let signed = assemble(&bytes, x.public(), &wrong, &sign_record(&y, &bytes)).unwrap();
        assert!(matches!(verify(&signed), Err(TrustError::BadSignature(_))));
    }

    #[test]
    fn pairing_with_yourself_is_rejected() {
        let x = id(1);
        let bytes = build_record(x.public(), "X", x.public(), "X2", &[], &[], NOW).unwrap();
        let s = sign_record(&x, &bytes);
        let signed = assemble(&bytes, x.public(), &s, &s).unwrap();
        assert!(matches!(
            verify(&signed),
            Err(TrustError::Invalid("party ordering"))
        ));
    }

    #[test]
    fn non_canonical_ordering_is_rejected() {
        let (x, y) = (id(1), id(2));
        let (lo, hi) = if x.public().ed25519_bytes() < y.public().ed25519_bytes() {
            (&x, &y)
        } else {
            (&y, &x)
        };
        let swapped = PairingRecord {
            identity_a: hi.public().to_bytes().to_vec().into(),
            identity_b: lo.public().to_bytes().to_vec().into(),
            ..PairingRecord::default()
        };
        let bytes = swapped.encode_to_vec();
        let signed = SignedPairingRecord {
            record: bytes.clone().into(),
            signature_a: sign_record(hi, &bytes).to_bytes().to_vec().into(),
            signature_b: sign_record(lo, &bytes).to_bytes().to_vec().into(),
        };
        assert!(
            verify(&signed).is_err(),
            "two canonical records for one pair would let a revoked pairing be shadowed"
        );
    }

    #[test]
    fn freshness_window() {
        let (x, y) = (id(1), id(2));
        let v = verify(&signed_between(&x, &y)).unwrap();
        check_fresh(&v, NOW).unwrap();
        check_fresh(&v, NOW + MAX_REGISTRATION_AGE_MS).unwrap();
        assert!(
            check_fresh(&v, NOW + MAX_REGISTRATION_AGE_MS + 1).is_err(),
            "held back too long"
        );
        check_fresh(&v, NOW - MAX_FUTURE_SKEW_MS).unwrap();
        assert!(
            check_fresh(&v, NOW - MAX_FUTURE_SKEW_MS - 1).is_err(),
            "from the future"
        );
    }

    #[test]
    fn field_limits() {
        let (x, y) = (id(1), id(2));
        let long = "n".repeat(MAX_NAME_LEN + 1);
        assert!(build_record(x.public(), &long, y.public(), "Y", &[], &[], NOW).is_err());
        assert!(build_record(x.public(), "bad\nname", y.public(), "Y", &[], &[], NOW).is_err());
        assert!(
            build_record(
                x.public(),
                "X",
                y.public(),
                "Y",
                &["NOT VALID".into()],
                &[],
                NOW
            )
            .is_err()
        );
        let many: Vec<String> = (0..=MAX_GRANTS).map(|i| format!("g{i}")).collect();
        assert!(build_record(x.public(), "X", y.public(), "Y", &many, &[], NOW).is_err());
    }

    #[test]
    fn garbage_is_rejected_not_panicked_on() {
        for junk in [vec![], vec![0xff; 10], vec![0u8; 5000]] {
            let s = SignedPairingRecord {
                record: junk.into(),
                ..Default::default()
            };
            assert!(verify(&s).is_err());
        }
        let (x, y) = (id(1), id(2));
        let mut signed = signed_between(&x, &y);
        signed.signature_a = vec![0u8; 63].into();
        assert!(matches!(
            verify(&signed),
            Err(TrustError::Malformed("signature"))
        ));
    }
}
