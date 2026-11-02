# Ubuntu Server without a monitor

Link Tunnel supports three modes on Ubuntu Server:

| Mode | What you get |
| --- | --- |
| `gnome` (default) | Modern GNOME Shell on a private 1920×1080 Wayland virtual monitor, a bottom dock, Files, Console, Firefox and Chromium. Choose **Connect** from Windows, Mac or Linux. |
| `xfce` | A lighter Xvfb/Xfce desktop with Thunar, terminal and GNOME Web. |
| `terminal` | A remote shell through **Terminal** or `nexus shell`; no X server or desktop is started. |

The desktop runs as your normal Ubuntu account. It starts at boot without a monitor,
display manager or local graphical login. Closing the remote viewer leaves applications
running. Rebooting the server or restarting its agent ends the virtual desktop and its
applications. Encoding is in software at up to 30 fps; use `--size 1280x720` during
setup if a modest server needs a smaller desktop.

Windows and Mac can both control this server. The server does not need the dashboard
or viewer installed or running; its agent receives the connections. The setup adds
the desktop and applications to Ubuntu Server, so installing the agent alone is not
enough to get a full desktop. The default uses real GNOME Shell with simple native Adwaita
appearance and a dock containing both browsers, Files and Console. Settings live in a private
`linktunnel` dconf database so they do not change another desktop session for the same account.
The private headless session stays awake; screen locking is not provided without GDM.

GNOME runs with its headless virtual-monitor mode, captured through Mutter and PipeWire.
It is supervised together with the agent inside a private
[D-Bus session](https://dbus.freedesktop.org/doc/dbus-run-session.1.html), with systemd
[user lingering](https://www.freedesktop.org/software/systemd/man/latest/loginctl.html)
for boot-time startup. No VNC or RDP service is required.

## Before replacing Ubuntu Desktop

Back up your files before installing Server. In the Ubuntu Server installer, enable
**OpenSSH server**, create a normal login account, and configure networking. Keep SSH
access until you have tested Link Tunnel from a second device and after a reboot.
A clean install erases the previous Link Tunnel identity unless you back it up separately;
the setup below creates a new server profile, so pair your controllers again.

If you use Tailscale, install it on the server, sign into the same tailnet as your Windows
PC/Mac, and test SSH over it. All devices must reach the same signaling server. Your PC's
existing VPN server is `ws://100.114.106.93:8443/v1/ws`; that PC must remain online.
Use `wss://.../v1/ws` for a public TLS signaling server. Do not expose the plain WS port
to the public internet. Full-disk encryption may still require a passphrase at boot;
Link Tunnel cannot unlock a machine before Ubuntu has booted.

## Install the agent

If you have a built `nexus-remote_*.deb`, install that package first:

```sh
sudo apt install ./nexus-remote_0.1.0_amd64.deb
```

To build it on the server, install Rust with rustup if it is not already present, then:

```sh
sudo apt update
sudo apt install -y build-essential cmake pkg-config libasound2-dev git
git clone https://github.com/Adventnl/linktunnel.git
cd linktunnel
bash installers/linux/build-deb.sh --profile release
sudo apt install ./target/deb/nexus-remote_*.deb
```

Use a checkout containing this setup script. Rust uses the version pinned by the repository.
The package installs the agent/CLI; you do not need the Tauri dashboard on the server.

## Set up the virtual desktop

Run this from your normal Ubuntu login over SSH (sudo keeps the login in `SUDO_USER`):

```sh
sudo python3 /usr/share/nexus-remote/setup-headless.py \
  --signal-url ws://100.114.106.93:8443/v1/ws --allow-insecure-signal
```

From the source checkout, use `installers/linux/setup-headless.py` instead. Pass
`--user yourlogin` if sudo does not identify your normal login. The script installs the
desktop packages, enables user lingering, and starts `linktunnel-server.service`.
Some packages need Ubuntu's `universe` repository enabled. Firefox (`org.mozilla.firefox`)
and Chromium (`org.chromium.Chromium`) install per-user from Flathub with their normal
sandboxes. Both appear in the dock and app grid; `~/.local/bin/firefox` and
`~/.local/bin/chromium` launch them from a terminal. Run `flatpak update --user` for updates.
The setup installs Mesa's software renderer for machines with no GPU. Package and browser
installation may take several minutes.

For **terminal only**, add `--desktop terminal` to the same command. You can rerun with
`--desktop gnome` later; use `--desktop xfce` for the lighter alternative. A rerun keeps identity, pairings and unrelated configuration,
backs up a changed config, and restarts the agent (ending current sessions).

## Pair and connect

As your normal Ubuntu user, not root:

```sh
~/.local/bin/linktunnel-server status
~/.local/bin/linktunnel-server pair offer
```

Enter the offer in the Windows/Mac app. Compare the four verification words on both
devices and confirm both. Repeat for each controller. New profiles allow future access
to paired devices without a local approval dialog. Existing explicit access policy is
preserved; if needed, use `~/.local/bin/linktunnel-server trust unattended CONTROLLER`.

In the controller, click **Connect** for the desktop or **Terminal** for a shell. Terminal runs
as your Ubuntu user; use normal `sudo` authentication for administration. The file manager
can browse that user's files; Link Tunnel's separate file-transfer feature requires shared
folders configured in `[files]`.

On the existing Windows SYSTEM host, CLI equivalents run in Administrator PowerShell:

```powershell
.\nexus.exe --system remote view YOUR-SERVER
.\nexus.exe --system shell YOUR-SERVER
```

Test keyboard/mouse, open Files, Firefox and Chromium, disconnect/reconnect, then reboot the server
over SSH. Confirm it becomes reachable without any local login.

## Configuration and recovery

State: `~/.local/share/linktunnel-server/` (config, identity, pairings, logs and runtime).
The helper `~/.local/bin/linktunnel-server` always selects that profile.
Use this helper rather than plain `nexus`, which may select a different identity.
Do not also enable the old desktop or dedicated `nexus` system service for this profile.

```sh
systemctl --user status linktunnel-server
journalctl --user -u linktunnel-server -n 100 --no-pager
~/.local/bin/linktunnel-server doctor
~/.local/bin/linktunnel-server devices
# Stop or remove automatic startup, retaining your identity and files:
systemctl --user disable --now linktunnel-server
```

If Connect shows no desktop, check the journal for compositor/session startup and verify
`gnome-shell`, `dbus-run-session`, the Python GI/GStreamer packages and PipeWire are installed.
For the Xfce alternative, check `Xvfb` and `startxfce4`. Terminal remains available if capture
startup fails. A server with an explicit attended or step-up policy still needs its existing
approval/authentication workflow. A user service is used instead of the hardened `nexus`
system-account unit so the desktop has the user's home and the browser can run its sandbox.

The GNOME virtual monitor, PipeWire frames and Mutter input were exercised on Ubuntu 26.04
without a monitor. A service restart can be checked without rebooting; a real boot acceptance
check still needs a server reboot.
