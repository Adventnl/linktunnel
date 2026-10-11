# Self-hosting

> **Status:** the signaling server runs today with **in-memory stores** (state is lost on restart;
> agents transparently re-register their pairings when it comes back, which is tested in
> `pairings_survive_agent_restarts_and_a_signaling_server_restart`). Postgres/Redis stores, the
> Docker Compose bundle, the Helm chart and the packaged relay are Phase 2-10 deliverables that are
> **not done yet**; see docs/STATUS.md.

## What you run
| Component | Needed? | Notes |
|---|---|---|
| `nexus-signal` | yes | presence, pairing registry, sealed-envelope routing. One small process. |
| TLS terminator | yes (production) | `nexus-signal` speaks plain HTTP/WebSocket; put Caddy/nginx/Traefik in front. |
| `iroh-relay` | optional | only if peers cannot connect directly (symmetric NAT, UDP blocked). Off by default. |
| `nexus-wake` | optional (Phase 8) | a device on the target's LAN that sends Wake-on-LAN packets. |

## Signaling server
```sh
cargo build --release -p nexus-signal
./target/release/nexus-signal \
    --listen 127.0.0.1:8443 \
    --audience signal.example.com            # exactly what clients put in the URL host[:port]
```
* `--audience` is signed into every login; it must equal the `host[:port]` of the `wss://` URL the
  agents use. A mismatch is rejected on both sides.
* Behind a reverse proxy add `--trust-forwarded-for` so the failed-login limiter sees real client
  IPs (only if the proxy overwrites `X-Forwarded-For`; otherwise clients could spoof it).
* `/healthz` and `/metrics` (Prometheus text) are served on the same port; do not expose them publicly.

Caddy example:
```
signal.example.com {
    reverse_proxy 127.0.0.1:8443
}
```
Agents then use `--signal-url wss://signal.example.com/v1/ws`.

## Agents
* Nothing needs to be opened inbound for normal use. For a server you want to reach directly, pin a UDP
  port (`net.udp_port = 7842` in `config.toml`) and allow it in the firewall.
* Without any relay, two devices behind strict NATs may fail to connect; run `iroh-relay` yourself and set
  `net.relay_urls = ["https://relay.example.com"]`. Nothing is sent to third-party relays unless you
  opt in.

## What the server can and cannot see
It sees which devices are paired and when they are online, and relays opaque blobs. It cannot read
envelopes, create pairings, or reach a device that did not pair with the caller. See
docs/THREAT_MODEL.md §4.4.
