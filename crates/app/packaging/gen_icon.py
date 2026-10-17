"""Regenerate Kandrium's packaging icon set from the vendored 1024x1024 master.

The master (`icon.png`, the production artwork from the original Electron app)
is the single source of truth. This script downsamples it to the density-1
sizes cargo-packager consumes and re-saves the @2x (1024) master. macOS applies
its own squircle mask, so we keep the full-bleed art with its alpha channel.
Run: python3 crates/app/packaging/gen_icon.py
"""

from PIL import Image

MASTER = "crates/app/packaging/icon.png"
OUT_DIR = "crates/app/packaging"

img = Image.open(MASTER).convert("RGBA")
if img.size != (1024, 1024):
    raise SystemExit(f"master must be 1024x1024, got {img.size}")

# Density-1 sizes the macOS .icns format supports.
for s in (16, 32, 128, 256, 512):
    img.resize((s, s), Image.LANCZOS).save(f"{OUT_DIR}/{s}x{s}.png")

# @2x master (1024) consumed by cargo-packager.
img.save(f"{OUT_DIR}/icon@2x.png")
print("wrote crates/app/packaging/icon*.png")
