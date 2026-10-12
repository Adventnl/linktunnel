//! Verified, single-use step-up authentication. Challenges stay server-side and are bound to
//! the authenticated device and the exact encrypted session; TOTP steps cannot be replayed.
#![forbid(unsafe_code)]
pub mod browser;
use nexus_crypto::store::SecretStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use totp_rs::{Algorithm, TOTP};
use webauthn_rs::prelude::*;
use zeroize::{Zeroize, Zeroizing};

/// An authentication or persistence failure. Credentials and responses are never included.
#[derive(Debug, thiserror::Error)]
#[error("step-up verification failed: {0}")]
pub struct AuthError(String);
fn err(e: impl std::fmt::Display) -> AuthError {
    AuthError(e.to_string())
}
const SECRET_NAME: &str = "stepup-credentials";
const TTL: Duration = Duration::from_secs(120);
const MAX_PENDING: usize = 64;

#[derive(Default, Clone, Serialize, Deserialize)]
struct Credentials {
    passkeys: Vec<Passkey>,
    totp_secret: Option<Vec<u8>>,
    last_totp_step: Option<u64>,
}
impl Drop for Credentials {
    fn drop(&mut self) {
        if let Some(secret) = &mut self.totp_secret {
            secret.zeroize();
        }
    }
}
/// A challenge safe to deliver to a browser / controller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Challenge {
    /// Random single-use challenge ID.
    pub id: String,
    /// WebAuthn navigator.credentials.get options (absent without an enrolled passkey).
    pub public_key: Option<serde_json::Value>,
    /// Whether a TOTP fallback is enrolled.
    pub totp: bool,
}
struct Pending {
    peer: [u8; 32],
    session: [u8; 32],
    expires: Instant,
    state: Option<PasskeyAuthentication>,
}
enum Enrollment {
    Passkey(PasskeyRegistration),
    Totp(Zeroizing<Vec<u8>>),
}
struct PendingEnrollment {
    expires: Instant,
    state: Enrollment,
}

/// Credential store and challenge verifier. Protect with a mutex when shared between sessions.
pub struct Verifier {
    webauthn: Webauthn,
    store: Arc<dyn SecretStore>,
    credentials: Credentials,
    pending: HashMap<String, Pending>,
    enrollment: HashMap<String, PendingEnrollment>,
}
impl Verifier {
    /// Loads credentials from the same private secret store used for device identity.
    /// The loopback browser bridge uses localhost as RP and allows ephemeral localhost ports.
    pub fn new(store: Arc<dyn SecretStore>) -> Result<Self, AuthError> {
        let origin = Url::parse("http://localhost").map_err(err)?;
        let webauthn = WebauthnBuilder::new("localhost", &origin)
            .map_err(err)?
            .allow_any_port(true)
            .rp_name("Nexus Remote")
            .build()
            .map_err(err)?;
        let credentials = match store.load(SECRET_NAME).map_err(err)? {
            Some(data) => serde_json::from_slice(&data).map_err(err)?,
            None => Credentials::default(),
        };
        Ok(Self {
            webauthn,
            store,
            credentials,
            pending: HashMap::new(),
            enrollment: HashMap::new(),
        })
    }
    /// Reports enrolled methods without exposing any credential material.
    pub fn methods(&self) -> (usize, bool) {
        (
            self.credentials.passkeys.len(),
            self.credentials.totp_secret.is_some(),
        )
    }
    fn prune(&mut self) {
        let now = Instant::now();
        self.pending.retain(|_, p| p.expires > now);
        self.enrollment.retain(|_, p| p.expires > now);
    }
    fn save(&self, next: &Credentials) -> Result<(), AuthError> {
        let data = Zeroizing::new(serde_json::to_vec(next).map_err(err)?);
        self.store.store(SECRET_NAME, &data).map_err(err)
    }
    /// Starts local enrollment; returns (handle, browser options or otpauth URI).
    /// The caller must authenticate the local administrator before exposing this operation.
    pub fn enroll(
        &mut self,
        passkey: bool,
        account: &str,
    ) -> Result<(String, serde_json::Value), AuthError> {
        self.prune();
        if self.enrollment.len() >= MAX_PENDING {
            return Err(err("too many pending enrollments"));
        }
        let id = random_id()?;
        let (state, options) = if passkey {
            let exclude = self
                .credentials
                .passkeys
                .iter()
                .map(|c| c.cred_id().clone())
                .collect();
            let (options, state) = self
                .webauthn
                .start_passkey_registration(Uuid::new_v4(), account, account, Some(exclude))
                .map_err(err)?;
            (
                Enrollment::Passkey(state),
                serde_json::to_value(options).map_err(err)?,
            )
        } else {
            if self.credentials.totp_secret.is_some() {
                return Err(err("TOTP is already enrolled"));
            }
            let mut secret = Zeroizing::new(vec![0u8; 20]);
            getrandom::fill(&mut secret).map_err(err)?;
            let otp = totp(&secret, account)?;
            let options =
                serde_json::json!({"uri": otp.get_url(), "secret": otp.get_secret_base32()});
            (Enrollment::Totp(secret), options)
        };
        self.enrollment.insert(
            id.clone(),
            PendingEnrollment {
                expires: Instant::now() + TTL,
                state,
            },
        );
        Ok((id, options))
    }
    /// Completes enrollment only after a valid browser response or possession of the TOTP secret.
    pub fn finish_enrollment(
        &mut self,
        id: &str,
        response: &str,
        now_secs: u64,
    ) -> Result<(), AuthError> {
        let pending = self
            .enrollment
            .remove(id)
            .ok_or_else(|| err("unknown or consumed enrollment"))?;
        if pending.expires <= Instant::now() {
            return Err(err("enrollment expired"));
        }
        let mut next = self.credentials.clone();
        match pending.state {
            Enrollment::Passkey(state) => {
                let registration: RegisterPublicKeyCredential =
                    serde_json::from_str(response).map_err(err)?;
                let key = self
                    .webauthn
                    .finish_passkey_registration(&registration, &state)
                    .map_err(err)?;
                if next.passkeys.iter().any(|p| p.cred_id() == key.cred_id()) {
                    return Err(err("credential already enrolled"));
                }
                next.passkeys.push(key);
            }
            Enrollment::Totp(secret) => {
                let step = matching_step(&secret, response, now_secs)?
                    .ok_or_else(|| err("incorrect enrollment code"))?;
                next.totp_secret = Some(secret.to_vec());
                next.last_totp_step = Some(step);
            }
        }
        self.save(&next)?;
        self.credentials = next;
        Ok(())
    }
    /// Issues a challenge bound to a paired device and the two Hello nonces for this session.
    pub fn begin(&mut self, peer: [u8; 32], session: [u8; 32]) -> Result<Challenge, AuthError> {
        self.prune();
        if self.pending.len() >= MAX_PENDING {
            return Err(err("too many authentication challenges"));
        }
        let (options, state) = if self.credentials.passkeys.is_empty() {
            (None, None)
        } else {
            let (options, state) = self
                .webauthn
                .start_passkey_authentication(&self.credentials.passkeys)
                .map_err(err)?;
            (
                Some(serde_json::to_value(options).map_err(err)?),
                Some(state),
            )
        };
        let id = random_id()?;
        self.pending.insert(
            id.clone(),
            Pending {
                peer,
                session,
                expires: Instant::now() + TTL,
                state,
            },
        );
        Ok(Challenge {
            id,
            public_key: options,
            totp: self.credentials.totp_secret.is_some(),
        })
    }
    /// Cancels a pending session challenge when its connection ends or times out.
    pub fn cancel(&mut self, id: &str) {
        self.pending.remove(id);
    }
    /// Consumes a challenge on every attempt, including failures. There is no reusable bearer
    /// token: success authorizes only the connection whose binding was supplied to `begin`.
    pub fn finish(
        &mut self,
        id: &str,
        peer: [u8; 32],
        session: [u8; 32],
        code: &str,
        assertion: &str,
        now_secs: u64,
    ) -> Result<(), AuthError> {
        let pending = self
            .pending
            .remove(id)
            .ok_or_else(|| err("unknown or consumed challenge"))?;
        if pending.expires <= Instant::now()
            || !bool::from(pending.peer.ct_eq(&peer))
            || !bool::from(pending.session.ct_eq(&session))
        {
            return Err(err("challenge expired or belongs to another session"));
        }
        let mut next = self.credentials.clone();
        if !code.is_empty() && assertion.is_empty() {
            let secret = next
                .totp_secret
                .as_ref()
                .ok_or_else(|| err("TOTP is not enrolled"))?;
            let step =
                matching_step(secret, code, now_secs)?.ok_or_else(|| err("incorrect code"))?;
            if next.last_totp_step.is_some_and(|last| step <= last) {
                return Err(err("code has already been used"));
            }
            next.last_totp_step = Some(step);
        } else if code.is_empty() && !assertion.is_empty() {
            let state = pending.state.ok_or_else(|| err("no passkey challenge"))?;
            let assertion: PublicKeyCredential = serde_json::from_str(assertion).map_err(err)?;
            let result = self
                .webauthn
                .finish_passkey_authentication(&assertion, &state)
                .map_err(err)?;
            if !result.user_verified() {
                return Err(err("authenticator did not verify its user"));
            }
            for key in &mut next.passkeys {
                let _ = key.update_credential(&result);
            }
        } else {
            return Err(err("provide exactly one factor"));
        }
        // Persist counters/replay floor before granting access. Failed persistence fails closed.
        self.save(&next)?;
        self.credentials = next;
        Ok(())
    }
}
fn random_id() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(err)?;
    Ok(data_encoding::BASE64URL_NOPAD.encode(&bytes))
}
fn totp(secret: &[u8], account: &str) -> Result<TOTP, AuthError> {
    TOTP::new(
        Algorithm::SHA1,
        6,
        0,
        30,
        secret.to_vec(),
        Some("Nexus Remote".into()),
        account.into(),
    )
    .map_err(err)
}
fn matching_step(secret: &[u8], code: &str, now: u64) -> Result<Option<u64>, AuthError> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let otp = totp(secret, "device")?;
    let step = now / 30;
    for candidate in [step, step.saturating_sub(1), step.saturating_add(1)] {
        if otp.check(code, candidate.saturating_mul(30)) {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn verifier() -> Verifier {
        Verifier::new(Arc::new(nexus_crypto::store::MemoryStore::default())).unwrap()
    }
    #[test]
    fn passkey_registration_assertion_and_replay() {
        use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};
        let mut v = verifier();
        let mut client = WebauthnAuthenticator::new(SoftPasskey::new(true));
        let origin = Url::parse("http://localhost:41234").unwrap();
        let (id, options) = v.enroll(true, "owner").unwrap();
        let registration = client
            .do_registration(origin.clone(), serde_json::from_value(options).unwrap())
            .unwrap();
        v.finish_enrollment(
            &id,
            &serde_json::to_string(&registration).unwrap(),
            1_900_000_000,
        )
        .unwrap();
        assert_eq!(v.methods(), (1, false));
        let c = v.begin([1; 32], [2; 32]).unwrap();
        let assertion = client
            .do_authentication(
                origin,
                serde_json::from_value(c.public_key.unwrap()).unwrap(),
            )
            .unwrap();
        let json = serde_json::to_string(&assertion).unwrap();
        v.finish(&c.id, [1; 32], [2; 32], "", &json, 1_900_000_000)
            .unwrap();
        assert!(
            v.finish(&c.id, [1; 32], [2; 32], "", &json, 1_900_000_000)
                .is_err()
        );
        let next = v.begin([1; 32], [2; 32]).unwrap();
        assert!(
            v.finish(&next.id, [1; 32], [2; 32], "", &json, 1_900_000_000)
                .is_err(),
            "old assertion must not validate a new challenge"
        );
    }

    #[test]
    fn totp_enrollment_proof_replay_and_binding() {
        let mut v = verifier();
        let now = 1_900_000_000;
        let (id, options) = v.enroll(false, "test").unwrap();
        let otp = TOTP::from_url(options["uri"].as_str().unwrap()).unwrap();
        assert!(!v.methods().1);
        v.finish_enrollment(&id, &otp.generate(now), now).unwrap();
        assert!(v.methods().1);
        let c = v.begin([1; 32], [2; 32]).unwrap();
        assert!(
            v.finish(&c.id, [1; 32], [2; 32], &otp.generate(now), "", now)
                .is_err(),
            "enrollment code cannot be reused"
        );
        let now = now + 60;
        let c = v.begin([1; 32], [2; 32]).unwrap();
        assert!(
            v.finish(&c.id, [3; 32], [2; 32], &otp.generate(now), "", now)
                .is_err()
        );
        assert!(
            v.finish(&c.id, [1; 32], [2; 32], &otp.generate(now), "", now)
                .is_err(),
            "wrong peer consumes challenge"
        );
        let c = v.begin([1; 32], [2; 32]).unwrap();
        v.finish(&c.id, [1; 32], [2; 32], &otp.generate(now), "", now)
            .unwrap();
        let c = v.begin([1; 32], [2; 32]).unwrap();
        assert!(
            v.finish(&c.id, [1; 32], [2; 32], &otp.generate(now), "", now)
                .is_err()
        );
    }
    #[test]
    fn expired_challenges_and_invalid_passkeys_fail_closed() {
        let mut v = verifier();
        let (id, options) = v.enroll(true, "test").unwrap();
        assert_eq!(options["publicKey"]["rp"]["id"], "localhost");
        assert!(v.finish_enrollment(&id, "{}", 1_900_000_000).is_err());
        assert_eq!(v.methods(), (0, false));
        let c = v.begin([1; 32], [2; 32]).unwrap();
        v.pending.get_mut(&c.id).unwrap().expires = Instant::now() - Duration::from_secs(1);
        assert!(
            v.finish(&c.id, [1; 32], [2; 32], "123456", "", 1_900_000_000)
                .is_err()
        );
    }
    #[test]
    fn totp_replay_floor_survives_restart() {
        let store: Arc<dyn SecretStore> = Arc::new(nexus_crypto::store::MemoryStore::default());
        let mut v = Verifier::new(store.clone()).unwrap();
        let now = 1_900_000_000;
        let (id, options) = v.enroll(false, "test").unwrap();
        let otp = TOTP::from_url(options["uri"].as_str().unwrap()).unwrap();
        v.finish_enrollment(&id, &otp.generate(now), now).unwrap();
        drop(v);
        let mut v = Verifier::new(store).unwrap();
        let c = v.begin([1; 32], [2; 32]).unwrap();
        assert!(
            v.finish(&c.id, [1; 32], [2; 32], &otp.generate(now), "", now)
                .is_err()
        );
    }
}
