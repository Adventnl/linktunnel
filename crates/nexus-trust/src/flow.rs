//! The pairing state machine.
//!
//! Drives the whole device-pairing exchange without doing any I/O: feed it the bytes that arrive
//! from the peer ([`PairingFlow::handle_incoming`]), send the bytes it hands back
//! ([`PairingFlow::take_outgoing`]), and react to its [`FlowEvent`]s. The transport can be the
//! signaling rendezvous, a LAN socket or an in-memory pipe in a test.
//!
//! ```text
//!   Noise XXpsk0 handshake (3 messages)            ── authenticates keys via the one-time code
//!   SasReady event on both sides                   ── humans compare the 4 words
//!   confirm_sas(true|false) on both sides          ── CONFIRM / ABORT over the encrypted channel
//!   initiator → PROPOSAL (record + its signature)  ── the exact bytes both will sign
//!   responder validates, → COUNTERSIGN             ── Completed on both sides
//! ```
//!
//! Nothing is signed until *both* humans have confirmed the code, and the responder enforces its
//! own ceiling on what the initiator may be granted, so a compromised or careless initiator
//! cannot grant itself more than the device owner pre-approved.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use nexus_crypto::noise::{
    PairingChannel, PairingCode, PairingOutcome, PairingSession, Role, ShortAuthString,
};
use nexus_crypto::{DeviceIdentity, PublicIdentity, Signature};

use crate::error::TrustError;
use crate::record::{self, VerifiedPairing};

const MSG_HANDSHAKE: u8 = 1;
const MSG_ENCRYPTED: u8 = 2;

const KIND_CONFIRM: u8 = 1;
const KIND_ABORT: u8 = 2;
const KIND_PROPOSAL: u8 = 3;
const KIND_COUNTERSIGN: u8 = 4;

/// Largest message the flow will look at.
const MAX_INCOMING: usize = 16 * 1024;
const SIGNATURE_LEN: usize = 64;

/// Why a flow ended without producing a pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbortReason {
    /// This device's user said the codes did not match (or declined).
    LocalDecline,
    /// The other device's user said the codes did not match (or declined).
    PeerDeclined,
    /// The peer sent something that breaks the protocol.
    Protocol(&'static str),
    /// The handshake failed (wrong code, tampering, wrong keys).
    Handshake,
    /// The proposal asked for more than this device's policy allows.
    PolicyViolation(&'static str),
    /// The proposal's contents do not match what was exchanged during the handshake.
    Mismatch(&'static str),
}

/// Something the application should react to.
#[derive(Debug, Clone)]
pub enum FlowEvent {
    /// Show these words to the user; call [`PairingFlow::confirm_sas`] with their answer.
    SasReady {
        /// The short authentication string both devices must display identically.
        sas: ShortAuthString,
        /// The name the peer gave itself (untrusted text).
        peer_name: String,
        /// The peer's verified public identity.
        peer: Box<PublicIdentity>,
    },
    /// Pairing succeeded; persist and register this record.
    Completed(Box<VerifiedPairing>),
    /// The flow ended without a pairing.
    Aborted(AbortReason),
}

/// What the initiator asks for.
#[derive(Debug, Clone, Default)]
pub struct Proposal {
    /// What the initiator may do on the responder.
    pub initiator_on_responder: Vec<String>,
    /// What the responder may do on the initiator.
    pub responder_on_initiator: Vec<String>,
}

/// What the responder is willing to grant.
#[derive(Debug, Clone, Default)]
pub struct ResponderPolicy {
    /// Upper bound on what the initiator may be allowed to do on the responder.
    pub max_initiator_on_me: Vec<String>,
}

#[derive(Debug)]
enum Stage {
    Handshake,
    /// SAS shown (or about to be); waiting for local decision and/or the peer's confirmation.
    Confirming,
    InitiatorAwaitCountersign {
        record: Vec<u8>,
        local_sig: Signature,
    },
    ResponderAwaitProposal,
    Done,
    Failed,
}

/// One side of a pairing exchange.
pub struct PairingFlow {
    role: Role,
    identity: Arc<DeviceIdentity>,
    name: String,
    session: Option<PairingSession>,
    channel: Option<PairingChannel>,
    outcome: Option<PairingOutcome>,
    stage: Stage,
    local_confirmed: bool,
    peer_confirmed: bool,
    proposal: Proposal,
    policy: ResponderPolicy,
    outgoing: VecDeque<Vec<u8>>,
    events: VecDeque<FlowEvent>,
}

impl std::fmt::Debug for PairingFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingFlow")
            .field("role", &self.role)
            .field("stage", &self.stage)
            .finish_non_exhaustive()
    }
}

impl PairingFlow {
    /// Starts as the device that scans/enters the code and proposes the pairing. The first
    /// handshake message is ready in [`take_outgoing`](Self::take_outgoing) immediately.
    pub fn initiator(
        identity: Arc<DeviceIdentity>,
        name: &str,
        code: &PairingCode,
        proposal: Proposal,
    ) -> Result<Self, TrustError> {
        let mut flow = Self::new(Role::Initiator, identity, name, code)?;
        flow.proposal = proposal;
        flow.drive_handshake()?;
        Ok(flow)
    }

    /// Starts as the device that displays the code and waits.
    pub fn responder(
        identity: Arc<DeviceIdentity>,
        name: &str,
        code: &PairingCode,
        policy: ResponderPolicy,
    ) -> Result<Self, TrustError> {
        let mut flow = Self::new(Role::Responder, identity, name, code)?;
        flow.policy = policy;
        Ok(flow)
    }

    fn new(
        role: Role,
        identity: Arc<DeviceIdentity>,
        name: &str,
        code: &PairingCode,
    ) -> Result<Self, TrustError> {
        let session = PairingSession::new(role, &identity, name, code)?;
        Ok(Self {
            role,
            identity,
            name: name.to_owned(),
            session: Some(session),
            channel: None,
            outcome: None,
            stage: Stage::Handshake,
            local_confirmed: false,
            peer_confirmed: false,
            proposal: Proposal::default(),
            policy: ResponderPolicy::default(),
            outgoing: VecDeque::new(),
            events: VecDeque::new(),
        })
    }

    /// Messages to deliver to the peer, in order. Drains the queue.
    pub fn take_outgoing(&mut self) -> Vec<Vec<u8>> {
        self.outgoing.drain(..).collect()
    }

    /// The next application event, if any.
    pub fn next_event(&mut self) -> Option<FlowEvent> {
        self.events.pop_front()
    }

    /// True once the flow has completed or failed.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(self.stage, Stage::Done | Stage::Failed)
    }

    fn fail(&mut self, reason: AbortReason, notify_peer: bool) -> TrustError {
        if notify_peer {
            self.send_encrypted(KIND_ABORT, &[]);
        }
        self.stage = Stage::Failed;
        self.session = None;
        self.events.push_back(FlowEvent::Aborted(reason));
        TrustError::Aborted("pairing failed")
    }

    fn send_encrypted(&mut self, kind: u8, body: &[u8]) {
        let Some(channel) = self.channel.as_mut() else {
            return;
        };
        let mut plain = Vec::with_capacity(1 + body.len());
        plain.push(kind);
        plain.extend_from_slice(body);
        if let Ok(cipher) = channel.encrypt(&plain) {
            let mut msg = Vec::with_capacity(1 + cipher.len());
            msg.push(MSG_ENCRYPTED);
            msg.extend_from_slice(&cipher);
            self.outgoing.push_back(msg);
        }
    }

    /// Sends our turns while the handshake lets us, and finalises it when complete.
    fn drive_handshake(&mut self) -> Result<(), TrustError> {
        loop {
            let Some(session) = self.session.as_mut() else {
                return Ok(());
            };
            if session.my_turn() {
                let m = session.next_message()?;
                let mut msg = Vec::with_capacity(1 + m.len());
                msg.push(MSG_HANDSHAKE);
                msg.extend_from_slice(&m);
                self.outgoing.push_back(msg);
            } else {
                break;
            }
        }
        if self
            .session
            .as_ref()
            .is_some_and(PairingSession::is_complete)
            && let Some(session) = self.session.take()
        {
            let (outcome, channel) = session.finish_with_channel()?;
            self.events.push_back(FlowEvent::SasReady {
                sas: outcome.sas,
                peer_name: outcome.peer_name.clone(),
                peer: Box::new(outcome.peer.clone()),
            });
            self.outcome = Some(outcome);
            self.channel = Some(channel);
            self.stage = Stage::Confirming;
        }
        Ok(())
    }

    /// Feeds one message received from the peer. An `Err` means the flow has failed (an
    /// [`FlowEvent::Aborted`] is queued); the caller should stop and tear down the transport.
    pub fn handle_incoming(&mut self, data: &[u8], now_ms: u64) -> Result<(), TrustError> {
        if self.is_finished() {
            return Err(TrustError::Flow("flow already finished"));
        }
        if data.is_empty() || data.len() > MAX_INCOMING {
            return Err(self.fail(AbortReason::Protocol("message size"), false));
        }
        match (data[0], &self.stage) {
            (MSG_HANDSHAKE, Stage::Handshake) => {
                let step = match self.session.as_mut() {
                    Some(s) => s.handle_message(&data[1..]),
                    None => return Err(TrustError::Flow("no handshake in progress")),
                };
                if step.is_err() {
                    return Err(self.fail(AbortReason::Handshake, false));
                }
                if self.drive_handshake().is_err() {
                    return Err(self.fail(AbortReason::Handshake, false));
                }
                Ok(())
            }
            (MSG_ENCRYPTED, Stage::Handshake) => Err(self.fail(
                AbortReason::Protocol("encrypted message before handshake end"),
                false,
            )),
            (MSG_ENCRYPTED, _) => self.handle_encrypted(&data[1..], now_ms),
            _ => Err(self.fail(AbortReason::Protocol("unexpected message type"), false)),
        }
    }

    fn handle_encrypted(&mut self, cipher: &[u8], now_ms: u64) -> Result<(), TrustError> {
        let plain = match self.channel.as_mut().map(|c| c.decrypt(cipher)) {
            Some(Ok(p)) if !p.is_empty() => p,
            _ => return Err(self.fail(AbortReason::Protocol("undecryptable message"), false)),
        };
        let (kind, body) = (plain[0], &plain[1..]);
        match (kind, &self.stage) {
            (KIND_ABORT, _) => {
                self.stage = Stage::Failed;
                self.events
                    .push_back(FlowEvent::Aborted(AbortReason::PeerDeclined));
                Err(TrustError::Aborted("peer aborted"))
            }
            (KIND_CONFIRM, Stage::Confirming) if !self.peer_confirmed => {
                self.peer_confirmed = true;
                self.advance(now_ms)
            }
            (KIND_PROPOSAL, Stage::ResponderAwaitProposal) => self.on_proposal(body, now_ms),
            (KIND_COUNTERSIGN, Stage::InitiatorAwaitCountersign { .. }) => {
                self.on_countersign(body)
            }
            _ => Err(self.fail(
                AbortReason::Protocol("message not valid in this state"),
                true,
            )),
        }
    }

    /// Records the local user's decision about the displayed words.
    pub fn confirm_sas(&mut self, words_match: bool, now_ms: u64) -> Result<(), TrustError> {
        if !matches!(self.stage, Stage::Confirming) || self.local_confirmed {
            return Err(TrustError::Flow("no confirmation pending"));
        }
        if !words_match {
            return Err(self.fail(AbortReason::LocalDecline, true));
        }
        self.local_confirmed = true;
        self.send_encrypted(KIND_CONFIRM, &[]);
        self.advance(now_ms)
    }

    /// Moves on once both humans have confirmed.
    fn advance(&mut self, now_ms: u64) -> Result<(), TrustError> {
        if !(self.local_confirmed && self.peer_confirmed) {
            return Ok(());
        }
        match self.role {
            Role::Responder => {
                self.stage = Stage::ResponderAwaitProposal;
                Ok(())
            }
            Role::Initiator => {
                let Some(outcome) = self.outcome.clone() else {
                    return Err(self.fail(AbortReason::Protocol("missing outcome"), true));
                };
                let bytes = match record::build_record(
                    self.identity.public(),
                    &self.name,
                    &outcome.peer,
                    &outcome.peer_name,
                    &self.proposal.initiator_on_responder,
                    &self.proposal.responder_on_initiator,
                    now_ms,
                ) {
                    Ok(b) => b,
                    Err(_) => {
                        return Err(
                            self.fail(AbortReason::PolicyViolation("invalid proposal"), true)
                        );
                    }
                };
                let sig = record::sign_record(&self.identity, &bytes);
                let Ok(len) = u16::try_from(bytes.len()) else {
                    return Err(self.fail(AbortReason::PolicyViolation("record too large"), true));
                };
                let mut body = Vec::with_capacity(2 + bytes.len() + SIGNATURE_LEN);
                body.extend_from_slice(&len.to_le_bytes());
                body.extend_from_slice(&bytes);
                body.extend_from_slice(&sig.to_bytes());
                self.send_encrypted(KIND_PROPOSAL, &body);
                self.stage = Stage::InitiatorAwaitCountersign {
                    record: bytes,
                    local_sig: sig,
                };
                Ok(())
            }
        }
    }

    fn on_proposal(&mut self, body: &[u8], now_ms: u64) -> Result<(), TrustError> {
        let Some(outcome) = self.outcome.clone() else {
            return Err(self.fail(AbortReason::Protocol("missing outcome"), true));
        };
        if body.len() < 2 + SIGNATURE_LEN {
            return Err(self.fail(AbortReason::Protocol("short proposal"), true));
        }
        let record_len = usize::from(u16::from_le_bytes([body[0], body[1]]));
        if body.len() != 2 + record_len + SIGNATURE_LEN {
            return Err(self.fail(AbortReason::Protocol("proposal length"), true));
        }
        let record_bytes = &body[2..2 + record_len];
        let peer_sig_bytes = &body[2 + record_len..];

        let Ok(parsed) = record::parse_unsigned(record_bytes) else {
            return Err(self.fail(AbortReason::Mismatch("malformed record"), true));
        };
        // The record must be exactly about us and the device we just authenticated.
        let me = self.identity.public();
        let (initiator_is_a, ok) = if parsed.a == *me && parsed.b == outcome.peer {
            (false, true)
        } else if parsed.b == *me && parsed.a == outcome.peer {
            (true, true)
        } else {
            (false, false)
        };
        if !ok {
            return Err(self.fail(AbortReason::Mismatch("record parties"), true));
        }
        let (initiator_name, responder_name, initiator_on_me, me_on_initiator) = if initiator_is_a {
            (
                &parsed.record.name_a,
                &parsed.record.name_b,
                &parsed.record.grants_a_to_b,
                &parsed.record.grants_b_to_a,
            )
        } else {
            (
                &parsed.record.name_b,
                &parsed.record.name_a,
                &parsed.record.grants_b_to_a,
                &parsed.record.grants_a_to_b,
            )
        };
        let _ = me_on_initiator; // generosity from the initiator needs no ceiling
        if *initiator_name != outcome.peer_name || *responder_name != self.name {
            return Err(self.fail(AbortReason::Mismatch("names"), true));
        }
        let allowed: BTreeSet<&String> = self.policy.max_initiator_on_me.iter().collect();
        if !initiator_on_me.iter().all(|g| allowed.contains(g)) {
            return Err(self.fail(AbortReason::PolicyViolation("grants exceed policy"), true));
        }
        if parsed.record.created_unix_ms.abs_diff(now_ms) > record::MAX_FUTURE_SKEW_MS {
            return Err(self.fail(AbortReason::Mismatch("timestamp"), true));
        }
        if record::verify_signature(&outcome.peer, record_bytes, peer_sig_bytes, "initiator")
            .is_err()
        {
            return Err(self.fail(AbortReason::Mismatch("initiator signature"), true));
        }

        // Everything checks out: countersign the exact bytes we were shown.
        let local_sig = record::sign_record(&self.identity, record_bytes);
        let Ok(peer_sig_array) = <[u8; SIGNATURE_LEN]>::try_from(peer_sig_bytes) else {
            return Err(self.fail(AbortReason::Protocol("signature length"), true));
        };
        let peer_sig = Signature::from_bytes(&peer_sig_array);
        let signed = match record::assemble(record_bytes, me, &local_sig, &peer_sig) {
            Ok(s) => s,
            Err(_) => return Err(self.fail(AbortReason::Mismatch("assemble"), true)),
        };
        let verified = match record::verify(&signed) {
            Ok(v) => v,
            Err(_) => return Err(self.fail(AbortReason::Mismatch("final verification"), true)),
        };
        self.send_encrypted(KIND_COUNTERSIGN, &local_sig.to_bytes());
        self.stage = Stage::Done;
        self.events
            .push_back(FlowEvent::Completed(Box::new(verified)));
        Ok(())
    }

    fn on_countersign(&mut self, body: &[u8]) -> Result<(), TrustError> {
        let Stage::InitiatorAwaitCountersign { record, local_sig } =
            std::mem::replace(&mut self.stage, Stage::Failed)
        else {
            return Err(self.fail(AbortReason::Protocol("unexpected countersign"), true));
        };
        let Some(outcome) = self.outcome.clone() else {
            return Err(self.fail(AbortReason::Protocol("missing outcome"), true));
        };
        let Ok(sig_array) = <[u8; SIGNATURE_LEN]>::try_from(body) else {
            return Err(self.fail(AbortReason::Protocol("signature length"), true));
        };
        if record::verify_signature(&outcome.peer, &record, body, "responder").is_err() {
            return Err(self.fail(AbortReason::Mismatch("responder signature"), true));
        }
        let signed = match record::assemble(
            &record,
            self.identity.public(),
            &local_sig,
            &Signature::from_bytes(&sig_array),
        ) {
            Ok(s) => s,
            Err(_) => return Err(self.fail(AbortReason::Mismatch("assemble"), true)),
        };
        let verified = match record::verify(&signed) {
            Ok(v) => v,
            Err(_) => return Err(self.fail(AbortReason::Mismatch("final verification"), true)),
        };
        self.stage = Stage::Done;
        self.events
            .push_back(FlowEvent::Completed(Box::new(verified)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use nexus_crypto::IdentitySeed;

    use super::*;
    use crate::grants;

    const NOW: u64 = 1_800_000_000_000;

    fn id(b: u8) -> Arc<DeviceIdentity> {
        Arc::new(DeviceIdentity::from_seed(&IdentitySeed::from_bytes(
            [b; 32],
        )))
    }

    fn all() -> Vec<String> {
        grants::all()
    }

    struct Pair {
        i: PairingFlow,
        r: PairingFlow,
    }

    fn pair_with(proposal: Proposal, policy: ResponderPolicy) -> Pair {
        let code = PairingCode::generate().unwrap();
        Pair {
            i: PairingFlow::initiator(id(1), "MacBook", &code, proposal).unwrap(),
            r: PairingFlow::responder(id(2), "Windows PC", &code, policy).unwrap(),
        }
    }

    fn default_pair() -> Pair {
        pair_with(
            Proposal {
                initiator_on_responder: all(),
                responder_on_initiator: vec![grants::SCREEN.into()],
            },
            ResponderPolicy {
                max_initiator_on_me: all(),
            },
        )
    }

    /// Delivers everything queued on both sides until quiescent. Returns errors seen.
    fn pump(p: &mut Pair, now: u64) -> Vec<TrustError> {
        pump_with_clocks(p, now, now)
    }

    /// Like [`pump`] but the two devices have different clocks.
    fn pump_with_clocks(p: &mut Pair, now_initiator: u64, now_responder: u64) -> Vec<TrustError> {
        let mut errors = Vec::new();
        loop {
            let to_r = p.i.take_outgoing();
            let to_i = p.r.take_outgoing();
            if to_r.is_empty() && to_i.is_empty() {
                return errors;
            }
            for m in to_r {
                if !p.r.is_finished()
                    && let Err(e) = p.r.handle_incoming(&m, now_responder)
                {
                    errors.push(e);
                }
            }
            for m in to_i {
                if !p.i.is_finished()
                    && let Err(e) = p.i.handle_incoming(&m, now_initiator)
                {
                    errors.push(e);
                }
            }
        }
    }

    fn drain(f: &mut PairingFlow) -> Vec<FlowEvent> {
        std::iter::from_fn(|| f.next_event()).collect()
    }

    fn sas_of(events: &[FlowEvent]) -> ShortAuthString {
        events
            .iter()
            .find_map(|e| match e {
                FlowEvent::SasReady { sas, .. } => Some(*sas),
                _ => None,
            })
            .expect("SasReady event")
    }

    #[test]
    fn happy_path_end_to_end() {
        let mut p = default_pair();
        assert!(pump(&mut p, NOW).is_empty());
        let (ie, re) = (drain(&mut p.i), drain(&mut p.r));
        assert_eq!(sas_of(&ie), sas_of(&re), "both screens show the same words");
        if let FlowEvent::SasReady {
            peer_name, peer, ..
        } = &ie[0]
        {
            assert_eq!(peer_name, "Windows PC");
            assert_eq!(&**peer, id(2).public());
        }

        p.i.confirm_sas(true, NOW).unwrap();
        p.r.confirm_sas(true, NOW).unwrap();
        assert!(pump(&mut p, NOW).is_empty());

        let (ie, re) = (drain(&mut p.i), drain(&mut p.r));
        let (FlowEvent::Completed(a), FlowEvent::Completed(b)) = (&ie[0], &re[0]) else {
            panic!("expected Completed on both sides: {ie:?} / {re:?}");
        };
        assert_eq!(
            a.signed, b.signed,
            "both sides hold the identical dual-signed record"
        );
        record::verify(&a.signed).unwrap();
        let view = a.view_for(&id(1).public().ed25519_bytes()).unwrap();
        assert_eq!(view.peer, *id(2).public());
        assert_eq!(view.grants_on_peer.len(), all().len());
        assert_eq!(view.grants_from_peer, vec![grants::SCREEN.to_string()]);
        assert!(p.i.is_finished() && p.r.is_finished());
    }

    #[test]
    fn confirmation_order_does_not_matter() {
        // The responder's human answers first, before the initiator even sees the words.
        let mut p = default_pair();
        pump(&mut p, NOW);
        p.r.confirm_sas(true, NOW).unwrap();
        pump(&mut p, NOW);
        p.i.confirm_sas(true, NOW).unwrap();
        assert!(pump(&mut p, NOW).is_empty());
        assert!(matches!(
            drain(&mut p.i).last(),
            Some(FlowEvent::Completed(_))
        ));
        assert!(matches!(
            drain(&mut p.r).last(),
            Some(FlowEvent::Completed(_))
        ));
    }

    #[test]
    fn nothing_is_signed_until_both_humans_confirm() {
        let mut p = default_pair();
        pump(&mut p, NOW);
        p.i.confirm_sas(true, NOW).unwrap();
        pump(&mut p, NOW);
        assert!(!p.i.is_finished() && !p.r.is_finished());
        assert!(
            !drain(&mut p.i)
                .iter()
                .any(|e| matches!(e, FlowEvent::Completed(_))),
            "initiator completed on its own confirmation alone"
        );
    }

    #[test]
    fn a_mismatch_on_either_side_aborts_both() {
        for decliner_is_initiator in [true, false] {
            let mut p = default_pair();
            pump(&mut p, NOW);
            drain(&mut p.i);
            drain(&mut p.r);
            let result = if decliner_is_initiator {
                p.i.confirm_sas(false, NOW)
            } else {
                p.r.confirm_sas(false, NOW)
            };
            assert!(result.is_err());
            pump(&mut p, NOW);
            let (ie, re) = (drain(&mut p.i), drain(&mut p.r));
            let (decl, other) = if decliner_is_initiator {
                (ie, re)
            } else {
                (re, ie)
            };
            assert!(matches!(
                decl.last(),
                Some(FlowEvent::Aborted(AbortReason::LocalDecline))
            ));
            assert!(matches!(
                other.last(),
                Some(FlowEvent::Aborted(AbortReason::PeerDeclined))
            ));
        }
    }

    #[test]
    fn man_in_the_middle_is_caught_by_the_humans() {
        // Mallory knows the code and relays between two separate flows.
        let code = PairingCode::generate().unwrap();
        let mut alice = PairingFlow::initiator(id(1), "Alice", &code, Proposal::default()).unwrap();
        let mut bob =
            PairingFlow::responder(id(2), "Bob", &code, ResponderPolicy::default()).unwrap();
        let mut mallory_to_alice =
            PairingFlow::responder(id(3), "Mallory", &code, ResponderPolicy::default()).unwrap();
        let mut mallory_to_bob =
            PairingFlow::initiator(id(3), "Mallory", &code, Proposal::default()).unwrap();
        for _ in 0..4 {
            for m in alice.take_outgoing() {
                let _ = mallory_to_alice.handle_incoming(&m, NOW);
            }
            for m in mallory_to_alice.take_outgoing() {
                let _ = alice.handle_incoming(&m, NOW);
            }
            for m in mallory_to_bob.take_outgoing() {
                let _ = bob.handle_incoming(&m, NOW);
            }
            for m in bob.take_outgoing() {
                let _ = mallory_to_bob.handle_incoming(&m, NOW);
            }
        }
        let (a, b) = (sas_of(&drain(&mut alice)), sas_of(&drain(&mut bob)));
        assert_ne!(a, b, "Alice and Bob would read different words and refuse");
    }

    #[test]
    fn responder_policy_caps_what_the_initiator_can_grant_itself() {
        let mut p = pair_with(
            Proposal {
                initiator_on_responder: all(),
                responder_on_initiator: vec![],
            },
            ResponderPolicy {
                max_initiator_on_me: vec![grants::SCREEN.into()],
            }, // view only
        );
        pump(&mut p, NOW);
        p.i.confirm_sas(true, NOW).unwrap();
        p.r.confirm_sas(true, NOW).unwrap();
        let errors = pump(&mut p, NOW);
        assert!(!errors.is_empty());
        assert!(
            drain(&mut p.r)
                .iter()
                .any(|e| matches!(e, FlowEvent::Aborted(AbortReason::PolicyViolation(_))))
        );
        assert!(
            !drain(&mut p.i)
                .iter()
                .any(|e| matches!(e, FlowEvent::Completed(_)))
        );
    }

    #[test]
    fn stale_proposal_timestamps_are_refused() {
        let mut p = default_pair();
        pump(&mut p, NOW);
        // The initiator's clock is an hour fast, so the record it proposes is stamped "in the
        // future" from the responder's point of view.
        let fast = NOW + 3_600_000;
        p.i.confirm_sas(true, fast).unwrap();
        p.r.confirm_sas(true, NOW).unwrap();
        pump_with_clocks(&mut p, fast, NOW);
        assert!(
            drain(&mut p.r)
                .iter()
                .any(|e| matches!(e, FlowEvent::Aborted(AbortReason::Mismatch("timestamp"))))
        );
    }

    #[test]
    fn wrong_code_never_reaches_the_sas_stage() {
        let (c1, c2) = (
            PairingCode::generate().unwrap(),
            PairingCode::generate().unwrap(),
        );
        let mut p = Pair {
            i: PairingFlow::initiator(id(1), "A", &c1, Proposal::default()).unwrap(),
            r: PairingFlow::responder(id(2), "B", &c2, ResponderPolicy::default()).unwrap(),
        };
        assert!(!pump(&mut p, NOW).is_empty());
        assert!(
            drain(&mut p.r)
                .iter()
                .any(|e| matches!(e, FlowEvent::Aborted(AbortReason::Handshake)))
        );
        assert!(
            !drain(&mut p.i)
                .iter()
                .any(|e| matches!(e, FlowEvent::SasReady { .. }))
        );
    }

    #[test]
    fn tampered_and_replayed_messages_kill_the_flow() {
        let mut p = default_pair();
        pump(&mut p, NOW);
        p.i.confirm_sas(true, NOW).unwrap();
        let mut confirm = p.i.take_outgoing();
        let mut tampered = confirm.pop().unwrap();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(p.r.handle_incoming(&tampered, NOW).is_err());
        assert!(p.r.is_finished());
        assert!(
            p.r.handle_incoming(&[MSG_ENCRYPTED, 1, 2, 3], NOW).is_err(),
            "no use after failure"
        );
    }

    #[test]
    fn protocol_violations_are_rejected() {
        let mut p = default_pair();
        // Encrypted traffic before the handshake finishes.
        assert!(p.r.handle_incoming(&[MSG_ENCRYPTED, 0, 0, 0], NOW).is_err());
        // Garbage / oversize / empty.
        let mut p = default_pair();
        assert!(p.r.handle_incoming(&[], NOW).is_err());
        let mut p = default_pair();
        assert!(
            p.r.handle_incoming(&vec![MSG_HANDSHAKE; MAX_INCOMING + 1], NOW)
                .is_err()
        );
        let mut p = default_pair();
        assert!(p.r.handle_incoming(&[0x7f, 1, 2], NOW).is_err());
        // confirm_sas before there is anything to confirm.
        let mut p = default_pair();
        assert!(p.r.confirm_sas(true, NOW).is_err());
    }

    #[test]
    fn flows_can_complete_a_second_time_with_fresh_codes() {
        // Same two devices pairing again (e.g. after a revocation) gets a fresh, valid record.
        for _ in 0..2 {
            let mut p = default_pair();
            pump(&mut p, NOW);
            p.i.confirm_sas(true, NOW).unwrap();
            p.r.confirm_sas(true, NOW).unwrap();
            assert!(pump(&mut p, NOW).is_empty());
            assert!(matches!(
                drain(&mut p.i).last(),
                Some(FlowEvent::Completed(_))
            ));
        }
    }
}
