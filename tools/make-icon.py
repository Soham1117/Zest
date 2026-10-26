"""One-off script: draws crates/zest-app/assets/zest.ico (orange disc, white Z).
Usage: python tools/make-icon.py   (needs Pillow)"""
from pathlib import Path
from PIL import Image, ImageDraw

S = 1024
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
d.ellipse((24, 24, S - 24, S - 24), fill=(255, 122, 24, 255))
# Z: top bar, diagonal, bottom bar (one polygon).
l, r, t, b = 280, 744, 292, 732
bar = 112
d.polygon([
    (l, t), (r, t), (r, t + bar), (l + 150, b - bar), (r, b - bar),
    (r, b), (l, b), (l, b - bar), (r - 150, t + bar), (l, t + bar),
], fill=(255, 255, 255, 255))
out = Path(__file__).resolve().parent.parent / "crates" / "zest-app" / "assets" / "zest.ico"
out.parent.mkdir(parents=True, exist_ok=True)
img.save(out, format="ICO", sizes=[(16, 16), (32, 32), (48, 48), (256, 256)])
print("wrote", out, out.stat().st_size)
