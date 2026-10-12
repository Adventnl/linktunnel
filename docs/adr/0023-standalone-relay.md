# ADR 0023: standalone Nexus relay over iroh-relay

Accepted 2026-10-12. Extends ADR 0003.

Ship nexus-relay as a standalone workspace binary embedding iroh-relay 1.3's server API.
Preserve existing end-to-end QUIC encryption, device allowlisting, and relay-to-direct migration.
The relay never decodes Nexus screen/control payloads. The owner controls infrastructure admission
with a token, separate from device pairing; --public deliberately disables infrastructure admission.

Direct TLS uses a PEM certificate/key with TLS 1.3 only. Plain HTTP binds loopback only behind a
TLS proxy. Optional UDP QUIC discovery shares the TLS configuration. Per-client ingress limits and
key-cache limits bound resource usage; use proxy limits for additional connection/IP admission.
Agents send net.relay_token to custom relays; Debug implementations redact it. No third-party
relay is enabled by default. Shutdown follows the upstream server lifecycle.

Compose puts the relay and TLS proxy in a shared network namespace so the HTTP relay remains
loopback-only, including inside containers. Test the actual child binary with pinned TLS roots,
relay-only peers, transmitted data, and a denied token. Certificates are loaded at startup;
operators restart to rotate them. Production WAN/NAT and container deployment remain device-lab work.
