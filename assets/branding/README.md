# linktunnel branding

Use **linktunnel** (lowercase) for the product name. `linktunnel-original.png` is the supplied artwork; `linktunnel.png` is the transparent square master with consistent padding.

To regenerate the desktop icons, web favicons, Android launcher icons, verification logo and native viewer icon:

```sh
python -m pip install Pillow
python scripts/generate-branding.py
```

The generator trims excess transparent margins while preserving the supplied mark and its blue gradient. Platform sizes all come from the same master.

Existing `nexus` binary/crate names, application identifiers, service IDs, protocol constants and state directories are retained for compatibility with existing installations, identities and pairings.
