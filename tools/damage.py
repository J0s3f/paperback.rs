"""Damages page images: white holes the size of blocks, white bands across the page, gray stains.

    python3 tools/damage.py page.png damaged.png SEED HOLES BANDS STAINS

HOLES is the share of the grid area wiped in block-sized squares (0.05 wipes about a tenth of the
blocks, since a square usually overlaps several cells), BANDS and STAINS are counts. The squares are
sized for pages of 600 dpi with the default dot density. Needs Pillow and NumPy.
"""
import sys, random
import numpy as np
from PIL import Image

src, dst, seed, holes, bands, stains = sys.argv[1], sys.argv[2], int(sys.argv[3]), float(sys.argv[4]), int(sys.argv[5]), int(sys.argv[6])
rng = random.Random(seed)
img = np.array(Image.open(src).convert("L"))
h, w = img.shape
cell = 105  # one cell at 600 dpi with the default dot pitch of 3 pixels
top, bottom = int(h * 0.04), int(h * 0.96)
# Holes: share `holes` of the grid area wiped in block-sized squares.
count = int(holes * (w * (bottom - top)) / (cell * cell))
for _ in range(count):
    x, y = rng.randrange(0, w - cell), rng.randrange(top, bottom - cell)
    img[y:y + cell, x:x + cell] = 255
for _ in range(bands):
    y = rng.randrange(top, bottom - 3 * cell)
    img[y:y + rng.randrange(cell, 3 * cell), :] = 255
for _ in range(stains):
    cx, cy, r = rng.randrange(w), rng.randrange(top, bottom), rng.randrange(cell, 4 * cell)
    yy, xx = np.ogrid[:h, :w]
    mask = (xx - cx) ** 2 + (yy - cy) ** 2 < r * r
    img[mask] = (img[mask] * 0.5 + 60).astype(np.uint8)
Image.fromarray(img).save(dst)
