# ADR-0017: LAN discovery (mDNS) without leaking presence

**Status:** Accepted (verified on loopback/LAN in a container)

## Context
Paired devices should find each other on a LAN even when no signaling server is reachable. mDNS is the standard way. But plain DNS-SD
announces "a Nexus device called Alex's PC is here" to everyone on the Wi-Fi, which contradicts *an unpaired device cannot even see
that another exists* (spec §2.2).

## Decision
Advertise `_nexus-remote._udp` with **no TXT data, a random-looking host name, and an instance name that is a rotating tag**:
`tag = BLAKE3-derive("nexus-remote mdns tag v1", device_id ‖ hour)[..8]`. A paired peer knows `device_id`, so it computes the tags for
the previous, current and next hour and matches them against what it hears. Anyone else sees an opaque 8-byte value that changes every
hour (it cannot be followed across hours or networks). Discovery yields *addresses only*; the QUIC handshake still authenticates the key
against the allow-list, so a spoofed advertisement can at worst waste a connection attempt.

## Consequences
* ✅ `paired_devices_find_each_other_on_the_lan_without_any_server`: two real agents, no server, traffic flows; with discovery off the same
  test cannot connect.
* The device ID is a hash of the public key and is shown in the UI/CLI; someone who learns it (for example from a screenshot) can test
  whether that device is on a given LAN. Accepted and documented; switch off with `net.lan_discovery = false`.
* mDNS reveals to the LAN that *some* Nexus device is present.
* Link-local IPv6 addresses are skipped (no scope id); IPv4 and global IPv6 are used.

## Alternatives
Plain DNS-SD names (leaks); a keyed broadcast over UDP (reinvents mDNS, no OS integration); only signaling-provided address hints (needs
the server, the thing we want to avoid on a LAN).
