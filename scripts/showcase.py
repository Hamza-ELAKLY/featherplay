#!/usr/bin/env python3
"""Turns the frames rendered by the `showcase` test (see src/showcase.rs) into the
README's looping animation, an animated WebP of the window alone. The last half second
blends back into the first frame, so the loop has no seam.

Needs Pillow (`pip install pillow`). Usage: scripts/showcase.py <frames-folder> <out.webp>
"""
import pathlib
import re
import sys

from PIL import Image

FPS = 25
BLEND = FPS // 2

folder, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
frames = []
for path in sorted(folder.glob("frame-*.rgba")):
    width, height = map(int, re.search(r"-(\d+)x(\d+)\.rgba$", path.name).groups())
    frames.append(Image.frombytes("RGBA", (width, height), path.read_bytes()).convert("RGB"))
if not frames:
    sys.exit(f"no frames in {folder}")

# The loop's seam: the end fades into where the animation starts.
for i in range(BLEND):
    k = len(frames) - BLEND + i
    frames[k] = Image.blend(frames[k], frames[0], (i + 1) / (BLEND + 1))

# Lossless: the interface's flat colours compress better this way than lossy, and the
# lossy encoder's frame-to-frame updates left faint streaks.
frames[0].save(out, save_all=True, append_images=frames[1:], duration=1000 // FPS, loop=0, lossless=True,
               quality=60, method=4)
print(f"{out}: {len(frames)} frames, {frames[0].size[0]}x{frames[0].size[1]}, {out.stat().st_size / 1e6:.1f} MB")
