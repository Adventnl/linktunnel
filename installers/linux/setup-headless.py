#!/usr/bin/env python3
"""Set up a boot-time Link Tunnel desktop or terminal on Ubuntu Server.

Run with sudo as your normal server login; never runs the desktop as root.
The nexus-remote package (or nexus-agent and nexus on PATH) must be installed first.
"""

import argparse
import datetime
import os
from pathlib import Path, PurePosixPath
import pwd
import shutil
import subprocess
import sys
import time
from urllib.parse import urlsplit


DESKTOP_PACKAGES = [
    "xvfb", "xauth", "dbus-x11", "x11-utils", "xfce4-session", "xfwm4",
    "xfce4-panel", "xfce4-settings", "xfdesktop4", "thunar", "xfce4-terminal",
    "adwaita-icon-theme", "fonts-dejavu-core", "libgl1-mesa-dri",
    "ca-certificates", "epiphany-browser",
]
UNIT_NAME = "linktunnel-server.service"
SESSION = ["dbus-run-session", "--", "startxfce4"]
GNOME_PACKAGES = [
    "gnome-shell", "gnome-settings-daemon", "gnome-console", "nautilus",
    "gnome-control-center", "gnome-backgrounds", "gnome-themes-extra",
    "gnome-shell-extension-ubuntu-dock", "adwaita-icon-theme", "fonts-dejavu-core",
    "libgl1-mesa-dri", "xwayland", "pipewire", "wireplumber", "python3-gi",
    "gir1.2-gstreamer-1.0", "gir1.2-gst-plugins-base-1.0", "gstreamer1.0-pipewire",
    "gstreamer1.0-plugins-base", "gstreamer1.0-plugins-good", "flatpak",
    "xdg-desktop-portal-gnome", "dconf-cli", "ca-certificates",
]


def signal_url(value):
    """Validate before making any machine changes; plain WS needs an explicit flag."""
    try:
        parsed = urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise argparse.ArgumentTypeError(str(error)) from error
    if (parsed.scheme not in ("ws", "wss") or not parsed.hostname
            or parsed.username is not None or parsed.password is not None
            or parsed.path != "/v1/ws" or parsed.query or parsed.fragment
            or port == 0 or any(c.isspace() or ord(c) < 32 for c in value)):
        raise argparse.ArgumentTypeError("use ws[s]://host[:port]/v1/ws without credentials")
    return value


def size(value):
    try:
        width, height = (int(part) for part in value.lower().split("x"))
        if not (320 <= width <= 7680 and 240 <= height <= 4320):
            raise ValueError()
    except ValueError as error:
        raise argparse.ArgumentTypeError("size must be WIDTHxHEIGHT, 320x240 to 7680x4320") from error
    return f"{width}x{height}"


def unit_quote(value):
    """Escape a literal systemd command/environment argument, including specifiers."""
    return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%") + '"'


def render_unit(agent, profile, desktop="xfce", size="1920x1080"):
    # default.target + linger starts this without a graphical login. No desktop-session
    # dependency or private /tmp. Run as a regular desktop account, including its
    # normal sudo/browser sandbox support, rather than the restricted nexus account.
    command = f"{unit_quote(agent)} --file-store --max-fps 30"
    dependencies = ""
    if desktop == "gnome":
        session = PurePosixPath(str(profile)) / "desktop/gnome-session.py"
        command = f"/usr/bin/dbus-run-session -- /usr/bin/python3 {unit_quote(session)} --size {unit_quote(size)} -- {command}"
        dependencies = "Wants=pipewire.socket wireplumber.service\nAfter=pipewire.socket\n"
    return f"""[Unit]
Description=Link Tunnel headless server
After=network.target
{dependencies}\
StartLimitIntervalSec=60
StartLimitBurst=10

[Service]
Type=simple
Environment={unit_quote('NEXUS_HOME=' + str(profile))}
Environment=RUST_LOG=info
Environment=LIBGL_ALWAYS_SOFTWARE=1
UnsetEnvironment=DISPLAY WAYLAND_DISPLAY XAUTHORITY SESSION_MANAGER DBUS_SESSION_BUS_ADDRESS
ExecStart={command}
Restart=always
RestartSec=5
KillMode=control-group
TimeoutStopSec=15
UMask=0077
WorkingDirectory=%h

[Install]
WantedBy=default.target
"""


def configure_gnome(profile):
    """Keep this virtual desktop's preferences separate from any physical session."""
    environment = os.environ | {"DCONF_PROFILE": str(profile / "desktop/dconf-profile")}
    settings = """[org/gnome/desktop/interface]
gtk-theme='Adwaita'
icon-theme='Adwaita'
cursor-theme='Adwaita'
color-scheme='default'
accent-color='blue'
enable-animations=false

[org/gnome/desktop/background]
picture-uri='file:///usr/share/backgrounds/gnome/adwaita-l.jxl'
picture-uri-dark='file:///usr/share/backgrounds/gnome/adwaita-d.jxl'
picture-options='zoom'

[org/gnome/desktop/session]
idle-delay=uint32 0

[org/gnome/desktop/screensaver]
lock-enabled=false

[org/gnome/shell]
favorite-apps=['org.mozilla.firefox.desktop', 'org.chromium.Chromium.desktop', 'org.gnome.Nautilus.desktop', 'org.gnome.Console.desktop']
enabled-extensions=['ubuntu-dock@ubuntu.com']
disable-user-extensions=false
welcome-dialog-last-shown-version='50.0'

[org/gnome/shell/extensions/dash-to-dock]
dock-position='BOTTOM'
extend-height=false
dash-max-icon-size=48
dock-fixed=true
autohide=false
intellihide=false
show-trash=false
show-mounts=false
"""
    subprocess.run(["dbus-run-session", "--", "dconf", "load", "/"],
                   input=settings, text=True, env=environment, check=True)


def update_config(existing, args):
    # A real TOML editor preserves unrelated policy, comments, and settings on reruns.
    import tomlkit
    config = tomlkit.parse(existing)
    for section in ("net", "display"):
        if section not in config:
            config[section] = tomlkit.table()
    config["net"]["signal_url"] = args.signal_url
    config["net"]["allow_insecure_signal"] = args.allow_insecure_signal
    config["display"]["virtual"] = "always" if args.desktop == "xfce" else "never"
    config["display"]["size"] = args.size
    config["display"]["session"] = SESSION if args.desktop == "xfce" else []
    # Only set access defaults for a new installation. Keep explicit policy on reruns.
    if not existing.strip():
        config["security"] = {"consent": "unattended", "unattended_requires_stepup": False}
        config["terminal"] = {"enabled": True}
        config["audio"] = {"enabled": False}
    return tomlkit.dumps(config)


def write_owned(path, content, account, mode=0o600):
    """Atomically replace managed text without following destination symlinks."""
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = path.with_name(path.name + ".new")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            stream.write(content)
        os.chown(temporary, account.pw_uid, account.pw_gid)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def parse_options(argv=None):
    """Validate setup options before installing packages or touching services."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--user", default=os.environ.get("SUDO_USER"), help="normal server login")
    parser.add_argument("--signal-url", type=signal_url, required=True)
    parser.add_argument("--allow-insecure-signal", action="store_true", help="plain ws on a trusted LAN/VPN")
    parser.add_argument("--desktop", choices=("gnome", "xfce", "terminal"), default="gnome")
    parser.add_argument("--size", type=size, default="1920x1080")
    args = parser.parse_args(argv)
    if args.signal_url.startswith("ws:") and not args.allow_insecure_signal:
        parser.error("ws:// requires --allow-insecure-signal (trusted LAN/VPN only)")
    return parser, args


def main(argv=None):
    parser, args = parse_options(argv)
    if sys.platform != "linux" or os.geteuid() != 0:
        parser.error("run on Ubuntu Server with sudo python3 setup-headless.py ...")
    if not args.user or args.user == "root":
        parser.error("specify --user with your normal Ubuntu login, not root")
    account = pwd.getpwnam(args.user)
    if account.pw_uid < 1000 or account.pw_shell.endswith(("nologin", "false")):
        parser.error("--user must be a regular login account")
    agent, cli = shutil.which("nexus-agent"), shutil.which("nexus")
    if not agent or not cli:
        parser.error("install nexus-remote first, or install nexus-agent and nexus on PATH")
    if not Path("/run/systemd/system").is_dir():
        parser.error("this setup requires Ubuntu Server booted with systemd")
    if args.desktop == "gnome" and not Path(__file__).with_name("headless-gnome.py").is_file():
        parser.error("keep headless-gnome.py beside setup-headless.py")

    def run(*command):
        subprocess.run(command, check=True, stdin=subprocess.DEVNULL,
                       env=os.environ | {"DEBIAN_FRONTEND": "noninteractive", "NEEDRESTART_MODE": "l"})

    # User-provided home directories are never used for privileged writes. Configure as
    # the target user after apt/linger; the whole GUI and browser run as that same user.
    run("apt-get", "update")
    packages = ["python3-tomlkit", "dbus-user-session"]
    if args.desktop == "xfce":
        packages += DESKTOP_PACKAGES
    elif args.desktop == "gnome":
        packages += GNOME_PACKAGES
    run("apt-get", "install", "-y", "--no-install-recommends", *packages)
    run("loginctl", "enable-linger", account.pw_name)
    run("systemctl", "start", f"user@{account.pw_uid}.service")

    # Drop privileges permanently before accessing files in the user's home.
    os.initgroups(account.pw_name, account.pw_gid)
    os.setgid(account.pw_gid)
    os.setuid(account.pw_uid)
    os.environ.update(HOME=account.pw_dir, USER=account.pw_name, LOGNAME=account.pw_name,
                      XDG_RUNTIME_DIR=f"/run/user/{account.pw_uid}",
                      DBUS_SESSION_BUS_ADDRESS=f"unix:path=/run/user/{account.pw_uid}/bus")
    home = Path(account.pw_dir)
    profile = home / ".local/share/linktunnel-server"
    if args.desktop == "gnome":
        write_owned(profile / "desktop/gnome-session.py",
                    Path(__file__).with_name("headless-gnome.py").read_text(), account, 0o700)
        write_owned(profile / "desktop/dconf-profile", "user-db:linktunnel\n", account)
        # Install browsers for the desktop owner; their normal sandboxes remain enabled.
        run("flatpak", "remote-add", "--user", "--if-not-exists", "flathub",
            "https://dl.flathub.org/repo/flathub.flatpakrepo")
        run("flatpak", "install", "--user", "-y", "--noninteractive", "flathub",
            "org.mozilla.firefox", "org.chromium.Chromium")
        if not (home / ".config/dconf/linktunnel").exists():
            configure_gnome(profile)
        for browser, app_id in (("firefox", "org.mozilla.firefox"), ("chromium", "org.chromium.Chromium")):
            write_owned(home / ".local/bin" / browser,
                        f'#!/bin/sh\nexec /usr/bin/flatpak run {app_id} "$@"\n', account, 0o700)
    config_path = profile / "config/config.toml"
    existing = config_path.read_text() if config_path.exists() else ""
    updated = update_config(existing, args)
    if existing and existing != updated:
        stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
        write_owned(config_path.with_name(f"config.toml.before-{stamp}"), existing, account)
    write_owned(config_path, updated, account)
    unit = home / ".config/systemd/user" / UNIT_NAME
    write_owned(unit, render_unit(agent, profile, args.desktop, args.size), account)
    wrapper = home / ".local/bin/linktunnel-server"
    # Shell quoting comes from shlex, not systemd quoting.
    import shlex
    write_owned(wrapper, "#!/bin/sh\nexec " + shlex.quote(cli) + " --home "
                + shlex.quote(str(profile)) + ' "$@"\n', account, 0o700)
    run("systemctl", "--user", "daemon-reload")
    run("systemctl", "--user", "enable", UNIT_NAME)
    run("systemctl", "--user", "restart", UNIT_NAME)
    run("systemctl", "--user", "is-active", UNIT_NAME)
    for _ in range(30):
        status = subprocess.run([cli, "--home", str(profile), "status"],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                timeout=5, check=False)
        if status.returncode == 0:
            break
        time.sleep(0.5)
    else:
        raise RuntimeError(f"Agent did not become ready; check journalctl --user -u {UNIT_NAME}")
    print(f"\nLink Tunnel {args.desktop} profile installed for {account.pw_name}.")
    print(f"Config: {config_path}\nLogs: journalctl --user -u {UNIT_NAME} -f")
    print(f"As {account.pw_name}, run: {wrapper} status")
    print(f"Then: {wrapper} pair offer (compare the four words on both devices)")
    print("Pair each controller once. Use Connect for the desktop or Terminal for a shell.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, KeyError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        sys.exit(f"Setup failed: {error}")
