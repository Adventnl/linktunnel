# Self-hosting

> **Status:** the signaling server, its stores (in-memory, SQLite, PostgreSQL, Redis), the Docker image, the Compose bundle, the Helm chart and
> systemd units exist. Verified: server and stores in tests (PostgreSQL/Redis against real containers, on an earlier machine); `helm lint` +
> `helm template`; `systemd-analyze verify`. **Not run here:** Docker Compose, a Kubernetes install, a live systemd. See [STATUS.md](STATUS.md).

## What you run
| Component | Needed? | Notes |
|---|---|---|
| `nexus-signal` | yes for **pairing** and presence over the Internet | one small process; **run exactly one replica** (presence and envelope routing are per-process) |
| TLS terminator | yes (production) | `nexus-signal` speaks plain HTTP/WebSocket; Caddy/nginx/Traefik/an Ingress in front |
| PostgreSQL + Redis | recommended | pairings in PostgreSQL, ephemeral presence in Redis; SQLite or in-memory work for one box (state lost on restart for in-memory) |
| relay (`nexus-relay`) | optional | only if peers cannot connect directly (symmetric NAT, UDP blocked). Standalone TLS/admission-limited relay binary (embeds upstream iroh-relay) |
| wake helper | optional | `nexus-agent --wake-helper` on an always-on LAN machine |

Devices on one LAN need none of this after pairing: they find each other over mDNS.

## Docker Compose (signal + PostgreSQL + Redis + Caddy)
```sh
cd deploy/compose && cp .env.example .env   # set NEXUS_DOMAIN, POSTGRES_PASSWORD
docker compose up -d --build
```
Agents then use `wss://$NEXUS_DOMAIN/v1/ws`. `docker-compose.local.yml` publishes the plain port on 127.0.0.1 for a trial.

## Kubernetes (Helm)
```sh
kubectl create secret generic nexus-signal \
  --from-literal=database-url='postgres://nexus:PASSWORD@postgres:5432/nexus' \
  --from-literal=redis-url='redis://redis:6379'
helm install nexus-signal deploy/helm/nexus-signal \
  --set audience=signal.example.com --set ingress.host=signal.example.com
```
The chart runs one replica (`Recreate`), as non-root with a read-only root filesystem, no capabilities and a NetworkPolicy that only admits the ingress
namespace (`networkPolicy.ingressNamespaceLabels`). It expects PostgreSQL and Redis to exist; it does not bundle them.

## Bare metal (systemd)
`installers/linux/build-deb.sh` builds `nexus-signal_*.deb` containing `nexus-signal.service` (DynamicUser, strict sandbox, syscall filter).
```sh
sudo systemctl edit nexus-signal   # set --audience to your host name
```
Or by hand: `nexus-signal --listen 127.0.0.1:8443 --audience signal.example.com`.
* `--audience` is signed into every login and must equal the `host[:port]` of the `wss://` URL the agents use.
* Behind a proxy add `--trust-forwarded-for` (only if the proxy overwrites `X-Forwarded-For`).
* `/healthz` and `/metrics` are served on the same port; do not expose them publicly.

Caddy example: `signal.example.com { reverse_proxy 127.0.0.1:8443 }`.

## Agents
* Nothing needs to be opened inbound for normal use. For a server you want to reach directly, pin a UDP port (`net.udp_port = 7842`) and allow it.
* Without a relay, two devices behind strict NATs may fail to connect; run `nexus-relay` and set `net.relay_urls`. Nothing is sent to third-party relays unless you opt in.
* A headless Linux server: install the `.deb`, `sudo systemctl enable --now nexus-agent`, pair (`sudo nexus --system pair offer`), then
  `sudo nexus --system auth enroll-totp`, then `sudo nexus --system trust unattended <your device>` (an unattended box cannot ask anyone). The unit runs as the unprivileged `nexus` user; see the admin-terminal drop-in for the root-shell variant and its warning.

## What the server can and cannot see
It sees which devices are paired and when they are online, and relays opaque blobs. It cannot read envelopes, create pairings, or reach a device that
did not pair with the caller. See [THREAT_MODEL.md](THREAT_MODEL.md) §4.4.

## Standalone relay

Build `cargo build --release -p nexus-relay`. Set a random `NEXUS_RELAY_TOKEN` of at least 32 bytes
in its service environment. Direct TLS:

```sh
nexus-relay --tls-listen 0.0.0.0:3341 --cert fullchain.pem --key privkey.pem
```

An optional `--quic-listen 0.0.0.0:7842` provides UDP address discovery; without it encrypted
HTTP/WebSocket relay traffic still works. Or run the loopback HTTP listener behind Caddy:
`relay.example.com { reverse_proxy 127.0.0.1:3340 }`.

For the standalone Compose bundle set `NEXUS_RELAY_DOMAIN` and `NEXUS_RELAY_TOKEN` in your environment:

```sh
cd deploy/compose
docker compose -f docker-compose.relay.yml up -d --build
```

The relay shares the proxy's network namespace, so the unencrypted HTTP hop is loopback-only.
Agents need `[net] relay_urls = ["https://relay.example.com"]` and the same admission token via
`NEXUS_NET__RELAY_TOKEN`. TLS is verified with OS roots; configure a publicly trusted certificate
(or install your private CA on clients). The token admits clients to infrastructure, while paired
device keys still authorize sessions. It is redacted from configuration Debug output. Production
WAN and Compose deployment have not been demonstrated here; the standalone binary is tested with
TLS, relay-only peers and rejected admission tokens.

Unattended targets now enforce step-up. See [STEP_UP.md](STEP_UP.md) for passkey/TOTP enrollment.
