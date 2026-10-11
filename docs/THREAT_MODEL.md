# Threat model (v1)

Scope: Phases 1-2 (identity, pairing, signaling, transport, local IPC, lockdown). Media, input
injection, file transfer, terminal and the Windows helper are covered by v2 when they exist.
Every mitigation below names the **test** that demonstrates it; a mitigation without a test is
marked *design only*.

## 1. Assets
| Asset | Why it matters |
|---|---|
| Device root seed (→ Ed25519 + X25519 keys) | whoever holds it *is* that device to every peer |
| The trust set (who is paired, with what grants) | defines who may take control of this machine |
| Session content (pixels, keystrokes, files, clipboard) | the user's data and credentials |
| Presence metadata (who is paired, online, where) | reveals habits and topology |
| The target's availability | a remote-control agent must not be a DoS or lockout vector |

## 2. Actors
| Actor | Capabilities assumed |
|---|---|
| Passive network observer | sees all traffic on any link |
| Active network attacker (MITM) | modifies, drops, replays, injects packets; runs rogue relays |
| Malicious or compromised **signaling server** | full control of its process, DB and traffic |
| Malicious **relay operator** | sees and manipulates relayed packets and metadata |
| Unpaired remote device | knows a target's public key, speaks the protocol |
| Local unprivileged user (other account on the machine) | can read world-readable files, connect to sockets |
| Local same-user malware | runs as the user (largely out of scope; see §6) |
| A previously paired device that was revoked | holds old valid records and keys |
| Shoulder-surfer | sees a pairing QR/words on a screen |

**Out of scope for v1:** compromised OS/kernel, malware running as the same user that can read
process memory or the keychain, hardware attacks, supply-chain compromise of the build
(mitigated, not eliminated, by `cargo-deny`/audit, pinned toolchain).

## 3. Trust boundaries
```
 user ──(authenticated local IPC)── agent ──(TLS1.3/QUIC, pinned keys)── peer agent
                                     │
                                     └──(WSS, key-possession login)── signaling server (untrusted for content)
                                     └──(HTTPS)── relay (sees ciphertext only)
```

## 4. STRIDE

### 4.1 Spoofing
| Threat | Mitigation | Evidence |
|---|---|---|
| Attacker impersonates a device to the signaling server | login = signature over `audience ‖ server_nonce ‖ client_nonce` with the device key | `a_signature_by_a_different_key_is_rejected` (nexus-signal/tests/auth.rs) |
| Replay of a recorded login | server nonce is fresh per connection | `a_recorded_login_cannot_be_replayed_on_a_new_connection` |
| Login proof for server X used against server Y | audience is signed | `proof_made_for_another_server_is_rejected` |
| Impostor answers at a known address with its own key | TLS raw-public-key auth: dial target's key must match | `connecting_to_the_wrong_key_at_a_known_address_fails` (nexus-net) |
| Unpaired device connects to a target | allow-list checked right after the handshake, before any app byte | `an_unpaired_device_cannot_get_a_session`, `relay_cannot_be_used_to_reach_an_unpaired_device` |
| Forged pairing between two victims | pairing record needs **both** signatures, registrant must be a party | `a_record_with_only_one_signature_is_rejected`, `a_third_party_cannot_register_a_pairing_between_two_other_devices` |
| Substituting someone's X25519 key under their Ed25519 key | key-binding signature verified on every identity | `x25519_key_swap_attack_is_rejected` (nexus-crypto) |
| MITM during pairing | Noise `XXpsk0` with one-time code + human-compared SAS | `man_in_the_middle_who_knows_the_code_produces_different_sas`, `man_in_the_middle_is_caught_by_the_humans` |
| Server impersonates a peer inside an envelope | Noise `X` authenticates the sender's static key; sender must be a paired peer | `open_from_peers_enforces_sender_freshness_and_replay` |

### 4.2 Tampering
| Threat | Mitigation | Evidence |
|---|---|---|
| Server/relay alters pairing traffic | Noise AEAD; any bit flip aborts both sides | `a_server_that_tampers_with_the_pairing_traffic_breaks_it` |
| Server alters or forges a revocation | revocation signed by the revoker; only a paired peer's revocation is applied | `remote_revocation_must_come_from_a_paired_peer_and_target_us` |
| Local file tampering adds a trusted peer | every stored record is re-verified on load | `hand_edited_files_cannot_add_peers` |
| Corrupted key file | checksum + magic + length; wrong perms refused | `corruption_is_detected`, `files_are_private_and_loose_permissions_are_refused` |
| Parser abuse (oversize/garbage frames) | caps checked before allocation; total decoders | `decoder_is_total`, `oversized_declared_length_is_rejected_before_buffering`, `the_server_survives_a_malformed_stream` |

### 4.3 Repudiation
Every pairing is a record signed by both parties and every revocation is signed by its issuer,
giving non-repudiable evidence of the trust relationships. The session **audit log** (ADR-0013)
is *design only* until Phase 9.

### 4.4 Information disclosure
| Threat | Mitigation | Evidence |
|---|---|---|
| Observer reads session content | QUIC/TLS 1.3, forward secret | transport design; `paired_devices_connect_negotiate_and_see_a_lan_path` (handshake succeeds only with pinned keys) |
| Signaling server reads envelopes | Noise `X` sealed to the recipient | `the_relay_cannot_read_the_message` (nexus-crypto), `a_sealed_envelope_travels_end_to_end_and_the_server_cannot_read_it` |
| Server learns the pairing code | rendezvous id is a one-way hash; code never leaves the devices | `code_derivations_known_answer`, design |
| Unpaired device learns a device exists / is online | presence and envelopes only between active pairings; unpaired = nonexistent | `unpaired_devices_learn_nothing_about_a_device`, `envelopes_to_unpaired_or_unknown_devices_are_indistinguishable` |
| Secrets in logs | `Redacted` wrapper, `Debug` impls of keys/codes print `[redacted]` | `debug_never_leaks_secrets`, `debug_output_hides_the_code` |
| Other local users read the key or IPC | `0600` files, `0700` dirs, owner check, peer-UID check, token | `files_are_private_and_loose_permissions_are_refused`, `token_and_socket_are_private_and_a_second_agent_is_refused` |
| Metadata at the signaling server | **accepted:** it learns who is paired with whom and when they are online. Minimise by self-hosting. | — |
| Metadata at the relay | **accepted:** sees key-to-key connectivity and volume, never content. Relays are off by default. | `defaults_never_use_third_party_infrastructure` |

### 4.5 Denial of service
| Threat | Mitigation | Evidence |
|---|---|---|
| Login brute force / signature flood | per-IP failed-login budget then lockout | `too_many_failed_logins_lock_out_the_source_address` |
| Pairing-code guessing | 10 joins/min/device; code is 66 bits and expires in 5 min | `rendezvous_abuse_is_bounded` |
| Envelope / message floods | per-device envelope budget; per-connection message rate | `offline_recipient_oversize_and_rate_limits` |
| Memory exhaustion via keyed limiters, replay tables | bounded tables with eviction | `a_flood_of_active_keys_cannot_grow_the_table`, `memory_is_bounded` |
| Slow-loris on handshakes / hello | handshake and hello timeouts; concurrent-handshake cap; QUIC address validation | `silent_peers_are_timed_out_during_hello` |
| Slow consumer ties up the server | bounded outbound queue, then disconnect | design (`Hub::send_to`) — *no dedicated test yet* |
| A paired but malicious device hogs the target | **partially:** diagnostics are bounded per request; media/file quotas arrive with those features |
| Remote lockout of the owner | revocation is local-first; lockdown is a local switch | `unpairing_ends_the_relationship_on_both_sides_immediately`, `lockdown_refuses_everything_at_once_and_persists` |

### 4.6 Elevation of privilege
| Threat | Mitigation | Evidence |
|---|---|---|
| Peer grants itself more than the owner allows | responder enforces its own ceiling on grants | `responder_policy_caps_what_the_initiator_can_grant_itself` |
| Signature reuse across protocols | purpose-labelled signing contexts | `signatures_are_domain_separated`, `signatures_cannot_be_transplanted_between_contexts` |
| Revoked pairing resurrected from an old record | stores reject records not newer than a revocation | `revoked_pairings_vanish_and_cannot_be_resurrected_by_old_records`, `revocation_removes_and_blocks_stale_records` |
| Revoked peer keeps a live session | allow-list revocation closes live QUIC connections | `revoking_a_peer_terminates_its_live_session_immediately` |
| Local user drives the agent over IPC | socket/token private; UID checked; wrong token rejected | `a_wrong_token_is_rejected`, `a_client_that_sends_requests_before_authenticating_gets_nothing` |
| Remote code execution via a parser | total decoders, size caps, `forbid(unsafe_code)`; fuzzing in Phase 9 | property tests; *fuzzing not yet run* |

## 5. Pairing-specific analysis
* **Code entropy.** 66 bits (six BIP-39 words), one-time, 5-minute lifetime, 10 attempts/min/device.
  Online guessing is infeasible. A compromised server could attempt offline guessing of the *rendezvous
  hash* (2^66 work) but gains only the ability to attend the rendezvous within the lifetime; the
  Noise PSK exchange still requires the code itself, and the SAS exposes any MITM.
* **Shoulder-surfed code.** An observer who learns the code can complete the Noise handshake with
  the displaying device *instead of* the real peer. The human SAS comparison is what stops this:
  the two honest humans would read different words (proved in the MITM tests). Users must compare the
  words, not just click through; `--assume-words-match` exists for tests and prints a warning.
* **Granting.** The initiator proposes grants; the responder enforces a ceiling. Neither side is signed
  into anything until both humans confirmed.

## 6. Residual risks and non-goals (honest list)
1. **Same-user malware** can use the IPC token, read process memory and (depending on OS) the keychain.
   OS keychain ACLs and the privileged-helper split reduce this on macOS/Windows but do not
   eliminate it.
2. **A stolen unlocked device** can approve pairings and take control. Step-up authentication
   (TOTP / WebAuthn assertions verified by the target) is designed (ADR-0006) but **not implemented yet**.
3. **Signaling-server metadata.** See §4.4.
4. **Clock skew:** envelope freshness uses a ±2 min window and the registry rejects records stamped
   > 10 min in the future; a device with a wildly wrong clock cannot pair.
5. **Windows and Linux builds are not yet exercised on real machines** in this project; platform
   specific code (named-pipe IPC, Credential Manager / Secret Service stores) is type-checked only.
6. **UDP port mapping** (UPnP/NAT-PMP) is enabled by default for connectivity; it can be disabled
   (`port_mapping = false`). Routers' implementations of these protocols are a LAN attack surface
   outside our control.
7. **Pre-authentication surface** on the QUIC port is the TLS/QUIC handshake (rustls + noq) plus the
   allow-list check; no application parsing happens before it.
8. **Revocation latency** to an *offline* peer: the peer learns on reconnect (the server holds the
   revocation; the revoker re-sends it after reconnecting). Until then the revoker has already
   stopped accepting that peer.

## 7. Review checklist for new features
- Does it parse bytes from a peer? → total decoder, cap before allocate, property test, fuzz target.
- Does it add a signature or KDF? → new context label / KDF string, known-answer test from an
  independent implementation.
- Does it widen what a paired peer can do? → grant string + target-side enforcement + audit entry.
- Does it persist state? → atomic write, owner-only permissions, re-verification on load.
- Does it send data to a server? → can the server be told less (sealed / hashed)?
