#!/usr/bin/env python3
"""Installer regressions, without installing packages or touching system services."""
import argparse
import importlib.util
import os
from pathlib import Path
import sys
import shutil
import subprocess
import tempfile
import tomllib
import types
import unittest
from unittest.mock import patch

# pwd is Unix-only; pure rendering/configuration checks also run on Windows.
if sys.platform == "win32":
    sys.modules["pwd"] = types.ModuleType("pwd")
spec = importlib.util.spec_from_file_location(
    "headless", Path(__file__).resolve().parents[1] / "installers/linux/setup-headless.py")
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class SetupTests(unittest.TestCase):
    def options(self, **overrides):
        _, options = setup.parse_options(["--signal-url", "wss://signal.example/v1/ws"])
        return argparse.Namespace(**(vars(options) | overrides))

    def test_default_desktop_is_1080p_and_smaller_displays_are_optional(self):
        config = tomllib.loads(setup.update_config("", self.options()))
        self.assertEqual(config["display"]["size"], "1920x1080")
        _, smaller = setup.parse_options([
            "--signal-url", "wss://signal.example/v1/ws", "--size", "1280x720"])
        config = tomllib.loads(setup.update_config("", smaller))
        self.assertEqual(config["display"]["size"], "1280x720")

    def test_rerun_and_terminal_switch_preserve_security_and_other_settings(self):
        existing = '''# Custom policy must survive.
[security]
consent = "attended"
unattended_requires_stepup = true
[files]
shared = [{ path = "/srv/share", writable = false }]
[net]
signal_urls = ["wss://backup.example/v1/ws"]
'''
        desktop = setup.update_config(existing, self.options())
        self.assertIn("# Custom policy must survive.", desktop)
        parsed = tomllib.loads(desktop)
        self.assertEqual(parsed["security"], tomllib.loads(existing)["security"])
        self.assertEqual(parsed["files"], tomllib.loads(existing)["files"])
        self.assertEqual(parsed["net"]["signal_urls"], ["wss://backup.example/v1/ws"])
        self.assertEqual(parsed["display"]["session"], [])
        self.assertEqual(parsed["display"]["virtual"], "never")
        self.assertEqual(desktop, setup.update_config(desktop, self.options()))
        terminal = tomllib.loads(setup.update_config(desktop, self.options(desktop="terminal")))
        self.assertEqual(terminal["display"]["virtual"], "never")
        self.assertEqual(terminal["display"]["session"], [])
        self.assertEqual(terminal["security"], parsed["security"])

    def test_new_profile_has_terminal_and_paired_access_without_enabling_audio(self):
        config = tomllib.loads(setup.update_config("", self.options()))
        self.assertTrue(config["terminal"]["enabled"])
        self.assertFalse(config["audio"]["enabled"])
        self.assertEqual(config["security"]["consent"], "unattended")
        self.assertFalse(config["security"]["unattended_requires_stepup"])
        self.assertEqual(config["display"]["virtual"], "never")

    def test_gnome_uses_a_private_compositor_and_keeps_browsers_sandboxed(self):
        unit = setup.render_unit('/usr/bin/nexus-agent', '/home/user/server', 'gnome')
        self.assertIn('/usr/bin/dbus-run-session -- /usr/bin/python3', unit)
        self.assertIn('desktop/gnome-session.py', unit)
        self.assertIn('Wants=pipewire.socket wireplumber.service', unit)
        self.assertNotIn('NoNewPrivileges=', unit)
        self.assertNotIn('--no-sandbox', Path(setup.__file__).read_text())
        xfce = tomllib.loads(setup.update_config('', self.options(desktop='xfce')))
        self.assertEqual(xfce['display']['virtual'], 'always')
        self.assertEqual(xfce['display']['session'], setup.SESSION)

    @unittest.skipUnless(sys.platform == 'linux' and shutil.which('dconf'), 'Linux dconf CLI')
    def test_gnome_preferences_roundtrip_through_a_private_dconf_database(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = Path(directory)
            (profile / 'desktop').mkdir()
            (profile / 'desktop/dconf-profile').write_text('user-db:linktunnel\n')
            environment = {'HOME': directory, 'XDG_CONFIG_HOME': str(profile / '.config')}
            with patch.dict(os.environ, environment):
                setup.configure_gnome(profile)
                result = subprocess.run(['dconf', 'read', '/org/gnome/desktop/interface/icon-theme'],
                    env=os.environ | {'DCONF_PROFILE': str(profile / 'desktop/dconf-profile')},
                    text=True, capture_output=True, check=True)
                self.assertEqual(result.stdout.strip(), "'Adwaita'")
            self.assertTrue((profile / '.config/dconf/linktunnel').is_file())

    def test_invalid_urls_and_sizes_fail_before_setup(self):
        for value in ("https://host/v1/ws", "ws://host/", "ws://host:0/v1/ws",
                      "ws://user:secret@host/v1/ws", "ws://host:99999/v1/ws",
                      "wss://host/v1/ws?token=secret", "ws://host/v1/ws\n"):
            with self.subTest(url=value), self.assertRaises(argparse.ArgumentTypeError):
                setup.signal_url(value)
        for value in ("1x1", "1280", "1280x720x24", "7681x4320"):
            with self.subTest(size=value), self.assertRaises(argparse.ArgumentTypeError):
                setup.size(value)
        self.assertEqual(setup.size("1920X1080"), "1920x1080")
        self.assertEqual(setup.signal_url("ws://[::1]:8443/v1/ws"), "ws://[::1]:8443/v1/ws")

    def test_service_boots_without_login_and_does_not_inherit_desktop(self):
        unit = setup.render_unit('/opt/Link Tunnel/nexus-agent', '/home/a%u/server')
        self.assertIn('ExecStart="/opt/Link Tunnel/nexus-agent" --file-store', unit)
        self.assertIn('NEXUS_HOME=/home/a%%u/server', unit)
        self.assertIn("WantedBy=default.target", unit)
        self.assertIn("UnsetEnvironment=DISPLAY WAYLAND_DISPLAY XAUTHORITY", unit)
        self.assertIn("KillMode=control-group", unit)
        self.assertNotIn("graphical-session.target", unit)
        self.assertNotIn("NoNewPrivileges=", unit)

    @unittest.skipIf(sys.platform == "win32", "Unix file ownership")
    def test_atomic_write_preserves_identity_and_private_permissions(self):
        import os
        account = setup.pwd.getpwuid(os.getuid())
        with tempfile.TemporaryDirectory() as directory:
            profile = Path(directory)
            identity = profile / "identity.key"
            identity.write_bytes(b"existing identity")
            config = profile / "config/config.toml"
            setup.write_owned(config, "first", account)
            setup.write_owned(config, "second", account)
            self.assertEqual(config.read_text(), "second")
            self.assertEqual(config.stat().st_mode & 0o777, 0o600)
            self.assertEqual(identity.read_bytes(), b"existing identity")


if __name__ == "__main__":
    unittest.main()
