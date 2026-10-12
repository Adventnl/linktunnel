# nexus-relay

Standalone relay carrying end-to-end encrypted Nexus connections. No media decoding or identity
keys live here. See [self hosting](../../docs/SELF_HOSTING.md) and [ADR 0023](../../docs/adr/0023-standalone-relay.md).

```sh
cargo build --release -p nexus-relay
# Set NEXUS_RELAY_TOKEN to a random secret of at least 32 bytes in your service environment.
nexus-relay --tls-listen 0.0.0.0:3341 --cert fullchain.pem --key privkey.pem
```

Use `--quic-listen 0.0.0.0:7842` for optional UDP discovery. `--listen 127.0.0.1:3340` is the
HTTP probe/proxy listener (default). Without direct TLS, expose it through a TLS proxy; plain
HTTP is never allowed to bind a public interface. `--public` deliberately allows any relay client.
The per-client ingress default is 100 Mbit/s; customize `--bytes-per-second`.

`cargo test -p nexus-relay` starts the real binary with pinned TLS and tests traffic/admission.
