# User guide (current capabilities)

> This guide grows with the product. It currently covers identity, pairing, presence, diagnostics,
> unpairing and lockdown. Remote desktop, terminal, files, wake and the dashboard app arrive in
> later phases (see docs/STATUS.md). Windows and Linux instructions are **untested on real
> machines yet**.

## 1. Build
```sh
cargo build --release          # produces target/release/{nexus,nexus-agent,nexus-signal}
```

## 2. Try it on one machine
```sh
./scripts/demo-local.sh        # signaling server + two agents + CLI, as separate processes
```

## 3. Use it on two machines
1. Run a signaling server somewhere both can reach (see docs/SELF_HOSTING.md), or locally for a trial:
   `nexus-signal --listen 0.0.0.0:8443 --audience <host>:8443`.
2. On **each** device start the agent:
   `nexus-agent --signal-url wss://<host>/v1/ws` (the normal, TLS-protected form). For a quick trial on
   a **trusted LAN** without TLS add `--allow-insecure-signal` and use `ws://<host>:8443/v1/ws`:
   pairings and sessions stay end-to-end protected, but anyone on that network path can see who is paired
   and online.
3. On device **B**: `nexus pair offer` → shows six words.
4. On device **A**: `nexus pair join "<six words>"`.
5. Both screens show **four words**. Compare them. If they are identical on both, answer `y` on both.
   If they differ, answer `n`: someone is interfering with the pairing.
6. `nexus devices` lists what is paired and whether it is online. `nexus ping B` measures latency and
   throughput over the encrypted session and tells you whether the path is LAN, direct or relay.

## 4. Safety features you should know about
* `nexus unpair <name>`: ends the relationship now; the other device is told and any live session closes.
* `nexus lockdown on`: refuses every incoming session, closes live ones, blocks dialling and
  pairing. It survives restarts. `nexus lockdown off` re-enables.
* Nothing connects to a device that did not pair with you; an unpaired device cannot even see that yours
  exists.

## 5. Troubleshooting
| Symptom | Likely cause / fix |
|---|---|
| `Cannot reach the Nexus agent` | `nexus-agent` is not running (or running under a different `--home`). |
| `nexus devices` shows the peer `offline` | the peer's agent is not connected to the same signaling server. |
| `ping` fails with a timeout, both online | UDP blocked/strict NAT: configure a relay (SELF_HOSTING.md); check the firewall. |
| Pairing: `signaling server … not connected` | pairing needs the signaling server; check `nexus status`. |
| Pairing failed: code did not match | the six words were mistyped or the 5-minute window elapsed; start over. |
| Words differ between screens | **do not pair**: network interference or wrong device. Re-run on a trusted network. |
