"""Regenerate linktunnel logos and platform icons (requires Pillow)."""

from pathlib import Path

from PIL import Image, ImageOps

ROOT = Path(__file__).resolve().parents[1]
BRAND = ROOT / "assets" / "branding"
ICONS = ROOT / "apps" / "nexus-app" / "src-tauri" / "icons"
PUBLIC = ROOT / "apps" / "nexus-app" / "public"


def main():
    original = Image.open(BRAND / "linktunnel-original.png").convert("RGBA")
    # Ignore nearly invisible export noise when measuring the supplied mark.
    bounds = original.getchannel("A").point(lambda a: 255 if a > 8 else 0).getbbox()
    if bounds is None:
        raise ValueError("The logo must contain visible pixels")
    mark = ImageOps.contain(original.crop(bounds), (896, 896), Image.Resampling.LANCZOS)
    logo = Image.new("RGBA", (1024, 1024))
    logo.paste(mark, ((1024 - mark.width) // 2, (1024 - mark.height) // 2))
    for directory in (ICONS, PUBLIC):
        directory.mkdir(parents=True, exist_ok=True)
    logo.save(BRAND / "linktunnel.png")
    logo.resize((128, 128), Image.Resampling.LANCZOS).save(BRAND / "linktunnel-128.png")
    logo.save(ICONS / "source.png")
    logo.save(ICONS / "icon.png")
    logo.resize((256, 256), Image.Resampling.LANCZOS).save(PUBLIC / "linktunnel.png")
    for size, filename in ((32, "32x32.png"), (128, "128x128.png"), (256, "128x128@2x.png")):
        logo.resize((size, size), Image.Resampling.LANCZOS).save(ICONS / filename)
    logo.save(ICONS / "icon.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
    logo.save(ICONS / "icon.icns")
    logo.save(PUBLIC / "favicon.ico", sizes=[(s, s) for s in (16, 32, 48)])
    logo.resize((180, 180), Image.Resampling.LANCZOS).save(PUBLIC / "apple-touch-icon.png")
    (BRAND / "linktunnel-32.rgba").write_bytes(logo.resize((32, 32), Image.Resampling.LANCZOS).tobytes())
    for density, size in (("mdpi", 48), ("hdpi", 72), ("xhdpi", 96), ("xxhdpi", 144), ("xxxhdpi", 192)):
        directory = ROOT / "apps" / "nexus-android" / "res" / f"mipmap-{density}"
        directory.mkdir(parents=True, exist_ok=True)
        logo.resize((size, size), Image.Resampling.LANCZOS).save(directory / "ic_launcher.png")


if __name__ == "__main__":
    main()
