"""Draw the Dunbrec icon: a scope screen with a sine trace.

Writes assets/dunbrec.png (256 px, window icon) and assets/dunbrec.ico
(16-256 px, Windows executable/installer). Run: python tools/make_icon.py
"""
import math
from pathlib import Path

from PIL import Image, ImageDraw

SIZE = 1024  # draw large, then downsample for smooth edges
img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
pad, radius = 48, 190
d.rounded_rectangle([pad, pad, SIZE - pad, SIZE - pad], radius, fill=(22, 34, 44, 255))

# Graticule.
left, right, top, bottom = 150, SIZE - 150, 210, SIZE - 210
for i in range(5):
    x = left + (right - left) * i / 4
    d.line([(x, top), (x, bottom)], fill=(48, 78, 70, 255), width=6)
for j in range(5):
    y = top + (bottom - top) * j / 4
    d.line([(left, y), (right, y)], fill=(48, 78, 70, 255), width=6)

# Sine trace.
mid, amp = (top + bottom) / 2, (bottom - top) * 0.36
# A dense chain of round dots gives a smooth, evenly thick stroke.
r = 22
for k in range(4001):
    x = left + (right - left) * k / 4000
    y = mid - amp * math.sin(2 * math.pi * 1.5 * k / 4000)
    d.ellipse([x - r, y - r, x + r, y + r], fill=(250, 210, 30, 255))

out = Path(__file__).resolve().parent.parent / "assets"
out.mkdir(exist_ok=True)
img.resize((256, 256), Image.LANCZOS).save(out / "dunbrec.png")
img.resize((256, 256), Image.LANCZOS).save(
    out / "dunbrec.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
)
print("wrote", out / "dunbrec.png", "and", out / "dunbrec.ico")
