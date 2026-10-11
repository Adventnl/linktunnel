//! Signed revocations: either party can end a pairing, immediately, on their own authority.

use nexus_crypto::{
    DeviceIdentity, PublicIdentity, SignContext, Signature, VerifyingKey, verify_with_key,
};
use nexus_proto::v1::{Revocation, SignedRevocation};
use prost::Message;

use crate::error::TrustError;

/// How far in the future a revocation's timestamp may be.
pub const MAX_FUTURE_SKEW_MS: u64 = 10 * 60 * 1000;

/// A revocation whose signature has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRevocation {
    /// Ed25519 key of the device that revoked.
    pub revoker: [u8; 32],
    /// Ed25519 key of the device that was revoked.
    pub peer: [u8; 32],
    /// When the revoker signed it.
    pub unix_ms: u64,
}

/// `identity` revokes its pairing with the device whose Ed25519 key is `peer`.
#[must_use]
pub fn revoke(identity: &DeviceIdentity, peer: &PublicIdentity, now_ms: u64) -> SignedRevocation {
    revoke_key(identity, &peer.ed25519_bytes(), now_ms)
}

/// Like [`revoke`] when only the peer's Ed25519 key is at hand (e.g. re-sending a stored
/// revocation to the server after a reconnect).
#[must_use]
pub fn revoke_key(identity: &DeviceIdentity, peer: &[u8; 32], now_ms: u64) -> SignedRevocation {
    let revocation = Revocation {
        revoker: identity.public().ed25519_bytes().to_vec().into(),
        peer: peer.to_vec().into(),
        unix_ms: now_ms,
    }
    .encode_to_vec();
    let signature = identity.sign(SignContext::Revocation, &revocation);
    SignedRevocation {
        revocation: revocation.into(),
        signature: signature.to_bytes().to_vec().into(),
    }
}

fn key32(bytes: &[u8], what: &'static str) -> Result<[u8; 32], TrustError> {
    bytes.try_into().map_err(|_| TrustError::Malformed(what))
}

/// Verifies the signature against the revoker key named *inside* the revocation. The caller must
/// then check that this revoker is actually a party to the pairing it wants to end.
pub fn verify(signed: &SignedRevocation, now_ms: u64) -> Result<VerifiedRevocation, TrustError> {
    if signed.revocation.len() > 256 {
        return Err(TrustError::Invalid("revocation size"));
    }
    let r = Revocation::decode(signed.revocation.as_ref())
        .map_err(|_| TrustError::Malformed("revocation"))?;
    let revoker = key32(&r.revoker, "revoker key")?;
    let peer = key32(&r.peer, "peer key")?;
    if revoker == peer {
        return Err(TrustError::Invalid("self revocation"));
    }
    if r.unix_ms > now_ms.saturating_add(MAX_FUTURE_SKEW_MS) {
        return Err(TrustError::TimestampOutOfRange);
    }
    let key =
        VerifyingKey::from_bytes(&revoker).map_err(|_| TrustError::Malformed("revoker key"))?;
    let sig: [u8; 64] = signed
        .signature
        .as_ref()
        .try_into()
        .map_err(|_| TrustError::Malformed("signature"))?;
    verify_with_key(
        &key,
        SignContext::Revocation,
        &signed.revocation,
        &Signature::from_bytes(&sig),
    )
    .map_err(|_| TrustError::BadSignature("revoker"))?;
    Ok(VerifiedRevocation {
        revoker,
        peer,
        unix_ms: r.unix_ms,
    })
}

#[cfg(test)]
mod tests {
    use nexus_crypto::IdentitySeed;

    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn id(b: u8) -> DeviceIdentity {
        DeviceIdentity::from_seed(&IdentitySeed::from_bytes([b; 32]))
    }

    #[test]
    fn roundtrip() {
        let (x, y) = (id(1), id(2));
        let signed = revoke(&x, y.public(), NOW);
        let v = verify(&signed, NOW).unwrap();
        assert_eq!(v.revoker, x.public().ed25519_bytes());
        assert_eq!(v.peer, y.public().ed25519_bytes());
        assert_eq!(v.unix_ms, NOW);
    }

    #[test]
    fn only_the_named_revoker_can_have_signed() {
        let (x, y, mallory) = (id(1), id(2), id(3));
        // Mallory claims X revoked Y.
        let r = Revocation {
            revoker: x.public().ed25519_bytes().to_vec().into(),
            peer: y.public().ed25519_bytes().to_vec().into(),
            unix_ms: NOW,
        }
        .encode_to_vec();
        let forged = SignedRevocation {
            signature: mallory
                .sign(SignContext::Revocation, &r)
                .to_bytes()
                .to_vec()
                .into(),
            revocation: r.into(),
        };
        assert!(matches!(
            verify(&forged, NOW),
            Err(TrustError::BadSignature(_))
        ));
    }

    #[test]
    fn tampering_and_wrong_context_are_rejected() {
        let (x, y) = (id(1), id(2));
        let mut signed = revoke(&x, y.public(), NOW);
        let mut bytes = signed.revocation.to_vec();
        *bytes.last_mut().unwrap() ^= 1;
        signed.revocation = bytes.into();
        assert!(verify(&signed, NOW).is_err());

        let r = revoke(&x, y.public(), NOW);
        let mismatched = SignedRevocation {
            signature: x
                .sign(SignContext::PairingRecord, &r.revocation)
                .to_bytes()
                .to_vec()
                .into(),
            revocation: r.revocation,
        };
        assert!(verify(&mismatched, NOW).is_err());
    }

    #[test]
    fn self_revocation_and_future_timestamps_are_rejected() {
        let x = id(1);
        assert!(verify(&revoke(&x, x.public(), NOW), NOW).is_err());
        let y = id(2);
        assert!(matches!(
            verify(&revoke(&x, y.public(), NOW + MAX_FUTURE_SKEW_MS + 1), NOW),
            Err(TrustError::TimestampOutOfRange)
        ));
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(verify(&SignedRevocation::default(), NOW).is_err());
        assert!(
            verify(
                &SignedRevocation {
                    revocation: vec![0xff; 300].into(),
                    ..Default::default()
                },
                NOW
            )
            .is_err()
        );
    }
}
