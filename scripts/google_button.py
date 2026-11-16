#!/usr/bin/env python3
"""Outlines the label of Google's "Sign in with Google" button from Roboto Medium, the
font Google's branding rules ask for, as one SVG path for `ui/app.slint`: the button looks
right without the app carrying a font for three words.

Needs fontTools (`pip install fonttools`) and Roboto's variable font (from
github.com/google/fonts, ofl/roboto). Usage: scripts/google_button.py <Roboto[wdth,wght].ttf>
Prints the path's width, height and commands, at 14 px on a 20 px line.
"""
import sys

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

TEXT = "Sign in with Google"
SIZE = 14.0
LINE = 20.0

font = instantiateVariableFont(TTFont(sys.argv[1]), {"wght": 500, "wdth": 100})
scale = SIZE / font["head"].unitsPerEm
glyphs = font.getGlyphSet()
cmap = font.getBestCmap()
hmtx = font["hmtx"]
ascent, descent = font["hhea"].ascent, font["hhea"].descent
# The baseline, with the text's height centred on the line.
baseline = (LINE + (ascent + descent) * scale) / 2

pen = SVGPathPen(glyphs, ntos=lambda v: f"{v:.2f}".rstrip("0").rstrip("."))
x = 0.0
for ch in TEXT:
    name = cmap[ord(ch)]
    glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, x, baseline)))
    x += hmtx[name][0] * scale

print(f"width {x:.2f}  height {LINE:.0f}")
print(pen.getCommands())
