# Threat model (v2)

Scope: everything implemented to date: identity, pairing, signaling, transport, local IPC, lockdown (§1-7, unchanged from
v1 except where noted) **and, new in v2, §8: screen/input/clipboard/files/terminal/port-forwarding/power/audio, the audit
log, updates, LAN discovery, virtual displays, the SSH proxy, the Windows service and the desktop app.**
Every mitigation below names the **test** that demonstrates it (`scripts/check-threat-model.sh` fails CI if a cited test
disappears); a mitigation without a test is marked *design only*. Fuzz targets are cited by path.

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
giving non-repudiable evidence of the trust relationships. The session **audit log** (ADR-0013) is implemented:
see §8.8.

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
| Remote code execution via a parser | total decoders, size caps, `forbid(unsafe_code)`; coverage-guided fuzzing of every byte parser (§8.12) | property tests; `fuzz/fuzz_targets/wire_formats.rs` and 12 others |

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
5. **Windows and macOS code has never been executed** in this project (type-checked only); the Linux paths are
   exercised. See STATUS.md and §8.10.
6. **UDP port mapping** (UPnP/NAT-PMP) is enabled by default for connectivity; it can be disabled
   (`port_mapping = false`). Routers' implementations of these protocols are a LAN attack surface
   outside our control.
7. **Pre-authentication surface** on the QUIC port is the TLS/QUIC handshake (rustls + noq) plus the
   allow-list check; no application parsing happens before it.
8. **Revocation latency** to an *offline* peer: the peer learns on reconnect (the server holds the
   revocation; the revoker re-sends it after reconnecting). Until then the revoker has already
   stopped accepting that peer.

## 7. Review checklist for new features (unchanged)
- Does it parse bytes from a peer? → total decoder, cap before allocate, property test, fuzz target.
- Does it add a signature or KDF? → new context label / KDF string, known-answer test from an
  independent implementation.
- Does it widen what a paired peer can do? → grant string + target-side enforcement + audit entry.
- Does it persist state? → atomic write, owner-only permissions, re-verification on load.
- Does it send data to a server? → can the server be told less (sealed / hashed)?


## 8. v2: the control surface (everything a paired device can do once connected)

Common to all of §8: a request is acted on only after (1) the QUIC handshake pinned the peer's key against the allow-list,
(2) the target checked the peer's **grant** for that feature in the signed pairing record, and (3) the target applied its
**consent policy** (attended = a person approves; unattended = pre-authorised per device). Each feature below adds its own
limits. All of it is written to the audit log (§8.8).

### 8.1 Screen, input and the viewer
| Threat | Mitigation | Evidence |
|---|---|---|
| A paired device without the `screen` or `input` grant sees or types | the **target** enforces grants, not the controller | `grants_are_enforced_by_the_target`, `a_denied_session_shows_nothing_and_injects_nothing` |
| Attended machine used without a person saying yes | prompt with timeout; nobody to ask = refuse at once | `attended_access_waits_for_a_person_and_honours_yes_and_no`, `an_unanswered_prompt_expires_and_an_unwatched_machine_refuses_at_once` |
| Prompt-spam / approval-fatigue attack by a paired device | per-peer session rate limit; three refusals lock the device out of prompting | `a_device_that_keeps_being_refused_stops_getting_prompts`, `repeated_refusals_lock_a_peer_out_for_a_while` |
| Malformed video/cursor/input datagrams crash or exploit the other side | total decoders, size caps, fuzzing (incl. OpenH264 on the controller) | `decode_never_panics`, `fuzz/fuzz_targets/h264_decoder.rs`, `fuzz/fuzz_targets/input_stream.rs` |
| Stuck modifier keys / runaway input after a drop | injector releases everything when the session ends | design (`Guarded` injector) — *no dedicated test cited* |
| Stale frames shown as current | per-frame streams are reset when stale; decoder refuses deltas before a keyframe | `delta_before_keyframe_is_refused` |

### 8.2 Files
| Threat | Mitigation | Evidence |
|---|---|---|
| Path traversal / absolute paths / `..` out of the shared folder | every wire path is canonicalised and must stay under a root | `outside_relative_and_dotdot_are_refused`, `fuzz/fuzz_targets/file_paths.rs` |
| Symlink inside the share pointing outside | resolution follows links *then* re-checks containment | `symlinks_cannot_escape`, `symlinks_pointing_out_of_the_share_are_refused` |
| Writing to a read-only share, or without the grant | per-root writability and the `files` grant | `read_only_roots_refuse_writes`, `without_the_files_grant_nothing_is_served` |
| Disk-filling upload; two writers corrupting one file | size limit; exclusive writer; `.part` files verified by BLAKE3 before rename | `uploads_over_the_size_limit_are_refused`, `a_second_writer_to_the_same_file_is_refused` |
| A resumed transfer splices attacker bytes | resume only from a hash-verified true prefix | `download_resumes_from_a_true_prefix_and_restarts_from_a_bad_one` |

### 8.3 Clipboard
| Threat | Mitigation | Evidence |
|---|---|---|
| Clipboard exfiltrated without consent / pre-existing secrets pushed on connect | per-direction opt-in, grant required, current contents not sent at connect | `without_the_clipboard_grant_nothing_crosses`, `existing_clipboard_contents_are_not_sent_on_connect` |
| Hostile PNG/format crashes the receiver | size cap, strict decode, fuzzed | `fuzz/fuzz_targets/clipboard_content.rs` |

### 8.4 Terminal and port forwarding (the `terminal` grant is administrator-grade)
| Threat | Mitigation | Evidence |
|---|---|---|
| Forwarding turns the target into an open relay into its LAN | loopback-only by default; explicit allow rules; connection cap | `loopback_only_by_default`, `explicit_allow_rules_work_and_the_connection_limit_holds` |
| A hostname that resolves off-machine bypasses the loopback rule | the *resolved* address is checked | `a_name_that_resolves_off_machine_is_refused_by_the_loopback_rule` |
| Hijacking someone's named persistent shell | a second controller takes over only through the policy path and the first is told | `a_second_controller_takes_over_a_named_terminal` |
| Shell as a more privileged account than intended | agent runs as its user; the systemd unit is unprivileged; a root shell is an explicit, warned drop-in; the Windows SYSTEM service passes `--no-terminal` | `installers/linux/systemd/nexus-agent.service`, WINDOWS.md |
| Stdio proxy exposes a listener | `nexus proxy` rides a loopback forward for the duration of one connection (any local process could connect to that ephemeral port during that time): **accepted**, documented | — |

### 8.5 Power and wake
| Threat | Mitigation | Evidence |
|---|---|---|
| A device without the grant shuts the target down | `power` grant + consent; destructive actions need the person's yes when attended | `without_the_grant_or_a_backend_nothing_happens`, `a_person_can_refuse_a_shutdown` |
| The wake relay used to probe/flood a LAN | needs the `wake` grant and `wake_relay` enabled; MACs validated; rate-limited by the session limits | `wake_relays_check_grant_config_and_input` |

### 8.6 Audio (new)
| Threat | Mitigation | Evidence |
|---|---|---|
| Eavesdropping on what the machine plays | separate **`audio` grant**, consent like video; microphone is a separate off-by-default switch; sharing can be disabled | `audio_needs_the_grant_and_a_source` |
| Hostile audio datagrams (header, libopus) | header validated, shape must match the negotiated stream, decode errors counted not fatal, fuzzed | `garbage_and_foreign_streams_are_rejected_without_panicking`, `fuzz/fuzz_targets/audio_player.rs` |
| Jitter-buffer memory growth / replay | bounded depth, late/duplicate packets dropped | `never_panics_and_never_replays`, `fuzz/fuzz_targets/jitter_buffer.rs` |
| The target can be made to stream while the owner is away | same consent policy; audit entry `audio.started` | `muting_silences_the_stream_and_audio_coexists_with_video` |

### 8.7 LAN discovery (mDNS, new)
| Threat | Mitigation | Evidence |
|---|---|---|
| An unpaired device on the Wi-Fi learns that a device exists / its name | no names or TXT data; instance = hourly-rotating keyed tag only paired peers can compute (ADR-0017) | `the_tag_reveals_nothing_readable`, `tags_are_stable_per_epoch_distinct_across_epochs_and_devices` |
| Spoofed advertisement redirects a controller to an attacker | discovery yields addresses only; the QUIC handshake pins the key | `a_paired_peer_finds_the_advertiser_and_a_stranger_does_not` |
| Presence tracking across networks/hours | the tag rotates hourly | `tags_are_stable_per_epoch_distinct_across_epochs_and_devices` |
| Residual: that *a* Nexus device is on this LAN; a known device id can be tested for | **accepted**; `net.lan_discovery = false` | `paired_devices_find_each_other_on_the_lan_without_any_server` |

### 8.8 Audit log
| Threat | Mitigation | Evidence |
|---|---|---|
| A compromised controller or local user edits the log to hide actions | hash chain + Ed25519-signed head | `editing_deleting_or_reordering_a_record_is_detected` |
| Truncating the tail | the signed head records the length | `cutting_records_off_the_end_is_caught_by_the_signed_head` |
| Substituting another device's log | head signed by this device's key | `a_forged_or_foreign_head_is_rejected` |
| Log growth | rotation keeps the chain unbroken | `rotation_keeps_the_chain_unbroken` |
| What is recorded | sessions, consent, pairing, lockdown, screen/input start, file operations with paths, terminals, forwards, power, audio | `what_happened_is_written_to_a_log_that_verifies_and_detects_tampering` |
| Residual: an attacker with the *device key* can re-sign a forged log | **accepted** (they *are* the device); ship the log off-box for stronger guarantees | — |

### 8.9 Updates
| Threat | Mitigation | Evidence |
|---|---|---|
| Malicious update server / MITM serves a trojan | Ed25519-signed manifests with a dedicated signing context; BLAKE3+size verified downloads | `tampering_a_wrong_key_or_no_key_is_rejected`, `hostile_updates_are_rejected` |
| Freeze / replay of an old manifest | expiry is enforced | `expired_manifests_are_rejected_not_ignored` |
| Downgrade to a vulnerable build | no-downgrade + `min_supported` | `equal_older_and_other_channel_manifests_change_nothing` |
| A bad update bricks the agent | atomic swap keeps `.old`, rollback command | `replacing_keeps_the_old_one_and_rolling_back_restores_it` |
| Manifest parser abuse | fuzzed | `fuzz/fuzz_targets/update_manifest.rs` |
| Residual: no update daemon applies these automatically yet | planned | — |

### 8.10 Windows service (new; **code never executed**)
| Threat | Mitigation | Evidence |
|---|---|---|
| A normal user reads the identity key from `C:\ProgramData\Nexus` | ACL reset to SYSTEM + Administrators at every start and by the MSI | design — *Win32 unrun* |
| The SYSTEM agent gives out a SYSTEM shell | `--no-terminal` by default; explicit `--allow-system-terminal` | design |
| Crash/restart loops hammer the machine | exponential back-off 1 s→30 s, reset after a healthy minute | `a_crashing_child_is_restarted_with_growing_delays_capped`, `a_healthy_child_resets_the_delay` |
| The console switches user and the agent serves the wrong session | child replaced on session change | `a_fast_user_switch_replaces_the_child` |
| Argument injection when launching the child | one correctly quoted command line | `spaces_quotes_and_backslashes`, `whole_command_line` |
| Attended prompt unavailable on the lock screen | request times out → refused; use pre-authorised devices for the lock screen | design (WINDOWS.md) |
| Residual: a SYSTEM process driving the secure desktop is the most privileged thing in this project | the agent's grants/consent still gate it; review before enabling on shared machines | — |

### 8.11 Virtual display and desktop app
| Threat | Mitigation | Evidence |
|---|---|---|
| Another local user attaches to the virtual X display | `-nolisten tcp`; X default same-user access | `starts_serves_capture_and_input_and_cleans_up` |
| Orphaned X servers / sockets after crashes | SIGTERM→kill, socket and lock removed; collision-free numbering | `two_displays_do_not_collide` |
| A crashing session command spins | exponential back-off | `a_crashing_session_command_is_restarted_with_backoff` |
| The web UI is used to reach the network or keys | the app has no key or network access: only the agent's local IPC; strict CSP; one capability (`core:default`) | ADR-0019; `json_shape_matches_the_typescript_types` |
| Hostile terminal output (escape sequences) abuses the UI | xterm.js renders it; input size-capped (1 MiB) in the backend | `ipc_events_translate_and_irrelevant_ones_are_dropped` |

### 8.12 Fuzzing (new)
`fuzz/` holds 13 `cargo-fuzz` targets: `wire_formats`, `protobuf_messages`, `frame_decoder`, `audio_player` (libopus), `jitter_buffer`,
`h264_decoder` (OpenH264), `crypto_inputs`, `trust_records`, `update_manifest`, `file_paths` (asserts the sandbox invariant),
`clipboard_content`, `config_toml`, `input_stream`. `scripts/fuzz-smoke.sh` ran each ≥ 45 s here (400 s for the codecs): no product crash. A nightly CI
job runs each for 4 minutes. This is smoke-level assurance, **not** a substitute for a long campaign or independent review.

### 8.13 Session resume (new)
Brief drops (Wi-Fi switch, sleep, crash of the controller's agent) should not need a person at the target to approve again, but resuming must
not turn attended mode into "approve once, access forever".
| Threat | Mitigation | Evidence |
|---|---|---|
| A resume window silently extends an approval | it opens only after a person *approved* this device, slides only while that session is alive, and lasts `security.resume_window_secs` (default 120, 0 = off) after the link dies | `a_dropped_link_may_resume_briefly_and_a_deliberate_end_may_not` |
| A deliberate disconnect leaves a way back in | goodbye, refusal and local stop clear the window; the next session asks | `a_dropped_link_may_resume_briefly_and_a_deliberate_end_may_not`, `scripts/demo-resume.sh` |
| Resume-state growth | bounded table | `resume_state_is_bounded` |
| Unaudited silent access | every resume is written to the audit log (`consent.resumed`) | `scripts/demo-resume.sh` |
| The viewer reconnecting forever / after a refusal | exponential back-off 1 s→8 s, gives up after 5 minutes, never retries a permission/not-found refusal | `a_lost_link_is_retried_and_a_deliberate_end_is_not`, `retry_delays_grow_then_cap` |
| Residual: a stolen *approved* controller can reconnect inside the window | **accepted**; shorten or disable the window | — |

### 8.14 What v2 still does not cover
Independent review; the macOS helper split; WebAuthn/TOTP step-up; the target-side "session active" indicator and panic hotkey; supply-chain
attestation of release binaries; behaviour of any Windows/macOS-specific path on real machines.
