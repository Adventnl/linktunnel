#!/usr/bin/env python3
"""Keep a private, monitorless GNOME compositor and Link Tunnel agent together.

Run inside dbus-run-session as the regular desktop user. No display manager,
automatic login, network desktop service or disabled browser sandbox is needed.
"""
import argparse
import os
from pathlib import Path
import signal
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--size", default="1920x1080")
    parser.add_argument("agent", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    agent = args.agent[1:] if args.agent[:1] == ["--"] else args.agent
    if not agent:
        parser.error("supply the agent command after --")
    width, height = args.size.split("x")
    if not (width.isdecimal() and height.isdecimal() and 320 <= int(width) <= 7680 and 240 <= int(height) <= 4320):
        parser.error("invalid virtual monitor size")
    os.environ.update(WAYLAND_DISPLAY="linktunnel-0", XDG_SESSION_TYPE="wayland",
                      XDG_CURRENT_DESKTOP="GNOME", LIBGL_ALWAYS_SOFTWARE="1")
    os.environ.pop("DISPLAY", None)
    # Apps installed per-user must be discoverable by Shell's app grid and launcher.
    exports = [str(Path.home() / ".local/share/flatpak/exports/share"),
               "/var/lib/flatpak/exports/share", "/usr/local/share", "/usr/share"]
    os.environ["XDG_DATA_DIRS"] = ":".join(exports)
    os.environ["DCONF_PROFILE"] = str(Path(__file__).with_name("dconf-profile"))
    subprocess.run(["dbus-update-activation-environment", "WAYLAND_DISPLAY",
                    "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP", "XDG_DATA_DIRS", "DCONF_PROFILE"], check=True)
    from gi.repository import Gio, GLib
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    children = []
    stopping = False

    def stop(_signum, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        shell = subprocess.Popen(["gnome-shell", "--headless", "--wayland",
                                  "--virtual-monitor", args.size,
                                  "--wayland-display=linktunnel-0"])
        children.append(shell)
        deadline = time.monotonic() + 40
        while not stopping:
            if shell.poll() is not None:
                raise RuntimeError(f"GNOME exited during startup ({shell.returncode})")
            try:
                state = bus.call_sync("org.gnome.Mutter.DisplayConfig",
                    "/org/gnome/Mutter/DisplayConfig", "org.gnome.Mutter.DisplayConfig",
                    "GetCurrentState", None, None, Gio.DBusCallFlags.NONE, 1000, None).unpack()
                if state[2]:
                    break
            except GLib.Error:
                pass
            if time.monotonic() >= deadline:
                raise RuntimeError("GNOME did not create its virtual monitor")
            time.sleep(.25)
        if stopping:
            return 0
        # Settings daemons supply GTK/Xwayland appearance and keyboard preferences.
        for name in ("gsd-xsettings", "gsd-media-keys"):
            path = Path("/usr/libexec") / name
            if path.exists():
                children.append(subprocess.Popen([str(path)]))
        host = subprocess.Popen(agent)
        children.append(host)
        while not stopping:
            if shell.poll() is not None:
                raise RuntimeError("GNOME compositor stopped; restarting the desktop")
            if host.poll() is not None:
                return host.returncode
            time.sleep(.25)
        return 0
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.terminate()
        deadline = time.monotonic() + 8
        for child in reversed(children):
            try:
                child.wait(timeout=max(.1, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


if __name__ == "__main__":
    raise SystemExit(main())
