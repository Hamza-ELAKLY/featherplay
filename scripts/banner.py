#!/usr/bin/env python3
"""Draws the README banner and GitHub's link preview (dark, with the accent glowing out
of the top-right corner) as self-contained SVGs: the wordmark and
tagline are outlined from the interface font (Plus Jakarta Sans), so they look the same
wherever the image is shown, whatever fonts the viewer has.

Needs fontTools (`pip install fonttools`). Usage: scripts/banner.py <output-folder>
"""
import math
import pathlib
import sys

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

root = pathlib.Path(__file__).resolve().parent.parent
out = pathlib.Path(sys.argv[1])
FONT = root / "ui/fonts/PlusJakartaSans.ttf"
FEATHER = ("M19.8 4.2C15.5 2 9.5 4 7 8.5S5.2 16.5 6.5 17.5C8 18.8 13.5 18 16.6 14.1S21.2 6.6 19.8 4.2Z "
           "M17.5 6.5 4 20 M13.2 10.9 9.6 10.4 M15.6 8.4 14.8 5.5")
WIDTH, HEIGHT = 1280, 400


def font_at(weight):
    return instantiateVariableFont(TTFont(FONT), {"wght": weight})


def outline(font, text, x, baseline, size, tracking=0.0):
    """The text as one SVG path, starting at x on the baseline; returns (path, width)."""
    scale = size / font["head"].unitsPerEm
    glyphs, cmap, hmtx = font.getGlyphSet(), font.getBestCmap(), font["hmtx"]
    pen = SVGPathPen(glyphs)
    cursor = 0.0
    for char in text:
        name = cmap[ord(char)]
        # Font units are y-up; SVG is y-down.
        glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, x + cursor, baseline)))
        cursor += hmtx[name][0] * scale + tracking
    return pen.getCommands(), cursor - tracking


def waveform(x, y, width, count, played):
    """Bar heights shaped like a song: swelling, falling back, a quiet ending."""
    gap = 3.0
    bar = (width - gap * (count - 1)) / count
    bars = []
    for i in range(count):
        t = i / (count - 1)
        level = 0.35 + 0.45 * math.sin(math.pi * t) ** 0.7 + 0.18 * math.sin(i * 1.7) * math.sin(i * 0.37)
        level *= 0.55 if t > 0.9 else 1.0
        height = max(6.0, 64 * min(1.0, max(0.12, level)))
        kind = "played" if t < played else "head" if abs(t - played) < 0.5 / count else "rest"
        bars.append((x + i * (bar + gap), y + (64 - height) / 2, bar, height, kind))
    return bars


def banner(theme, wrap=True):
    """One theme's banner; with `wrap` false, its drawing alone, to go in another SVG."""
    dark = theme == "dark"
    c = {
        "ground": "#111114" if dark else "#f4f4f6",
        "border": "#2a2a30" if dark else "#dedee4",
        "text": "#efeff3" if dark else "#18181c",
        "muted": "#c4c4cc" if dark else "#5d5d66",
        "accent": "#d33a51" if dark else "#c42f45",
        "head": "#f7b7c0" if dark else "#e57b8b",
        "rest": "#303037" if dark else "#d6d6dd",
        "track": "#2c2c32" if dark else "#dcdce2",
    }
    heavy, medium = font_at(800), font_at(500)
    # The group (wordmark, tagline, facts, waveform) is centred top to bottom, with the
    # same margin on the left of the icon as on the right of the waveform; the icon is
    # centred on the group.
    margin = 84
    cap = 0.72 * 96
    group = cap + 50 + 38 + 25 + 64 + 15
    top = (HEIGHT - group) / 2
    word_baseline = top + cap
    text_x = margin + 2 * 88 + 44
    word, _ = outline(heavy, "Featherplay", text_x - 4, word_baseline, 96, tracking=-1.5)
    tagline, _ = outline(medium, "A feather-light player for YouTube audio", text_x, word_baseline + 50, 30)
    facts, _ = outline(medium, "Portable  ·  ~50 MB of memory  ·  No account, no ads and no tracking", text_x, word_baseline + 88, 20)
    wave_top = word_baseline + 113
    icon_y = top + group / 2

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}">',
        "<defs>",
        f'<radialGradient id="glow-{theme}" cx="0.15" cy="0.45" r="0.6"><stop offset="0" stop-color="{c["accent"]}" stop-opacity="{0.28 if dark else 0.14}"/>'
        f'<stop offset="1" stop-color="{c["accent"]}" stop-opacity="0"/></radialGradient>',
        "</defs>",
        f'<rect x="1" y="1" width="{WIDTH - 2}" height="{HEIGHT - 2}" rx="28" fill="{c["ground"]}" stroke="{c["border"]}" stroke-width="2"/>',
        f'<rect x="1" y="1" width="{WIDTH - 2}" height="{HEIGHT - 2}" rx="28" fill="url(#glow-{theme})"/>',
        # The app icon: a white feather on a cool red disc.
        f'<circle cx="{margin + 88}" cy="{icon_y:.1f}" r="88" fill="{c["accent"]}"/>',
        f'<g transform="translate({margin + 24} {icon_y - 64:.1f}) scale(5.333)"><path d="{FEATHER}" fill="none" stroke="#ffffff" stroke-width="1.5" '
        'stroke-linecap="round" stroke-linejoin="round"/></g>',
        f'<path d="{word}" fill="{c["text"]}"/>',
        f'<path d="{tagline}" fill="{c["muted"]}"/>',
        f'<path d="{facts}" fill="{c["muted"]}" opacity="0.8"/>',
    ]
    # The waveform seek bar, as in the app: played bars in the accent, a thin line under.
    left, right, top = text_x, WIDTH - margin, wave_top
    for bx, by, bw, bh, kind in waveform(left, top, right - left, 96, 0.42):
        fill = {"played": c["accent"], "head": c["head"], "rest": c["rest"]}[kind]
        parts.append(f'<rect x="{bx:.1f}" y="{by:.1f}" width="{bw:.1f}" height="{bh:.1f}" rx="1.5" fill="{fill}"/>')
    played_x = left + (right - left) * 0.42
    parts += [
        f'<rect x="{left}" y="{top + 72}" width="{right - left}" height="3" rx="1.5" fill="{c["track"]}"/>',
        f'<rect x="{left}" y="{top + 72}" width="{played_x - left:.1f}" height="3" rx="1.5" fill="{c["accent"]}"/>',
        f'<circle cx="{played_x:.1f}" cy="{top + 73.5}" r="7" fill="{c["text"]}" stroke="{c["ground"]}" stroke-width="3"/>',
        "</svg>",
    ]
    if not wrap:
        return "\n".join(parts[1:-1])
    return "\n".join(parts) + "\n"


# The glow out of the top-right corner: the accent, deepening to the dark ground.
CORNER = [(0, "#b52c42"), (0.3, "#6a1d2b"), (0.7, "#221419"), (1, "#111114")]


def corner_glow(width, height, reach, rounded):
    stops = "".join(f'<stop offset="{o}" stop-color="{c}"/>' for o, c in CORNER)
    shape = (f'<rect x="1" y="1" width="{width - 2}" height="{height - 2}" rx="28" fill="url(#corner)" stroke="#2a2a30" stroke-width="2"/>'
             if rounded else f'<rect width="{width}" height="{height}" fill="url(#corner)"/>')
    return (f'<defs><radialGradient id="corner" cx="{width}" cy="0" r="{reach}" gradientUnits="userSpaceOnUse">'
            f"{stops}</radialGradient></defs>{shape}")


def dark_banner():
    """The dark banner on the corner glow (its own flat card and glow left out)."""
    lines = banner("dark", wrap=False).split("\n")
    start = next(i for i, line in enumerate(lines) if "url(#glow" in line) + 1
    return "\n".join([
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}">',
        corner_glow(WIDTH, HEIGHT, 1300, rounded=True),
        *lines[start:],
        "</svg>",
    ]) + "\n"


def social():
    """GitHub's link preview (1280 x 640, dark): everything centred, a margin all round."""
    width, height = 1280, 640
    c = {"ground": "#111114", "border": "#2a2a30", "text": "#efeff3", "muted": "#a9a9b3",
         "accent": "#d33a51", "head": "#f7b7c0", "rest": "#303037", "track": "#2c2c32"}
    heavy, medium = font_at(800), font_at(500)
    size, radius, gap = 112, 76, 40
    _, word_width = outline(heavy, "Featherplay", 0, 0, size, tracking=-1.8)
    row = 2 * radius + gap + word_width
    left = (width - row) / 2
    cap = 0.72 * size
    row_middle = 195
    word, _ = outline(heavy, "Featherplay", left + 2 * radius + gap, row_middle + cap / 2, size, tracking=-1.8)
    line = "A feather-light player for YouTube audio"
    _, line_width = outline(medium, line, 0, 0, 36)
    tagline, _ = outline(medium, line, (width - line_width) / 2, 329, 36)
    fact_text = "Portable  ·  ~50 MB of memory  ·  No account, no ads and no tracking"
    _, facts_width = outline(medium, fact_text, 0, 0, 24)
    facts, _ = outline(medium, fact_text, (width - facts_width) / 2, 377, 24)
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">',
        corner_glow(width, height, 1500, rounded=False),
        f'<circle cx="{left + radius:.1f}" cy="{row_middle}" r="{radius}" fill="{c["accent"]}"/>',
        f'<g transform="translate({left + radius - 55:.1f} {row_middle - 55}) scale(4.583)"><path d="{FEATHER}" fill="none" '
        'stroke="#ffffff" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></g>',
        f'<path d="{word}" fill="{c["text"]}"/>',
        f'<path d="{tagline}" fill="{c["muted"]}"/>',
        f'<path d="{facts}" fill="{c["muted"]}" opacity="0.8"/>',
    ]
    left_wave, right_wave, top = 200, width - 200, 437
    for bx, by, bw, bh, kind in waveform(left_wave, top, right_wave - left_wave, 90, 0.42):
        fill = {"played": c["accent"], "head": c["head"], "rest": c["rest"]}[kind]
        parts.append(f'<rect x="{bx:.1f}" y="{by:.1f}" width="{bw:.1f}" height="{bh:.1f}" rx="1.5" fill="{fill}"/>')
    played_x = left_wave + (right_wave - left_wave) * 0.42
    parts += [
        f'<rect x="{left_wave}" y="{top + 76}" width="{right_wave - left_wave}" height="3" rx="1.5" fill="{c["track"]}"/>',
        f'<rect x="{left_wave}" y="{top + 76}" width="{played_x - left_wave:.1f}" height="3" rx="1.5" fill="{c["accent"]}"/>',
        f'<circle cx="{played_x:.1f}" cy="{top + 77.5}" r="7" fill="{c["text"]}" stroke="{c["ground"]}" stroke-width="3"/>',
        "</svg>",
    ]
    return "\n".join(parts) + "\n"


out.mkdir(parents=True, exist_ok=True)
(out / "social-preview.svg").write_text(social())
path = out / "banner.svg"
path.write_text(dark_banner())
print(path, path.stat().st_size // 1024, "KB")
