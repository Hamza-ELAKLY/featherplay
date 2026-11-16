#!/usr/bin/env python3
"""Writes THIRD-PARTY-NOTICES.txt for a release: every library compiled into the program
for the given target (not build-only tools), with its licence and the licence texts its
authors ship, plus the bundled font and yt-dlp.

Usage: scripts/notices.py <target-triple> <output-file>
"""
import json
import pathlib
import subprocess
import sys

target, output = sys.argv[1], pathlib.Path(sys.argv[2])
root = pathlib.Path(__file__).resolve().parent.parent

# What is linked into the program: normal dependencies only, for this target.
tree = subprocess.run(
    ["cargo", "tree", "-e", "normal", "--target", target, "--prefix", "none", "--format", "{p}"],
    cwd=root, check=True, capture_output=True, text=True,
).stdout
linked = {tuple(line.split()[:2]) for line in tree.splitlines() if line.strip()}

meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--filter-platform", target],
    cwd=root, check=True, capture_output=True, text=True,
).stdout)

LICENCE_FILES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT")
sections, texts_seen = [], {}
for package in sorted(meta["packages"], key=lambda p: (p["name"], p["version"])):
    if (package["name"], "v" + package["version"]) not in linked or package["name"] == "featherplay":
        continue
    folder = pathlib.Path(package["manifest_path"]).parent
    files = sorted(f for f in folder.iterdir() if f.is_file() and f.name.upper().startswith(LICENCE_FILES))
    if not files and folder.name.startswith("i-slint"):
        files = sorted((folder / "LICENSES").glob("*")) if (folder / "LICENSES").is_dir() else []
    lines = [f"{package['name']} {package['version']}", f"Licence: {package.get('license') or 'see below'}"]
    if package.get("repository"):
        lines.append(f"Source: {package['repository']}")
    for f in files:
        text = f.read_text(errors="replace").strip()
        # The same licence text is printed once and referred to afterwards.
        key = text
        if key in texts_seen:
            lines.append(f"[{f.name}: same text as in {texts_seen[key]}]")
        else:
            texts_seen[key] = f"{package['name']} {package['version']}"
            lines += ["", f"--- {f.name} ---", text]
    if not files:
        lines.append("[No licence file in the published package; licence as stated above.]")
    sections.append("\n".join(lines))

# Some packages state a licence without shipping its text: add the standard text once.
STANDARD = {"MPL-2.0": "Mozilla Public License Version 2.0", "Apache-2.0": "Apache License"}
registry = pathlib.Path.home() / ".cargo/registry/src"
appendix = []
for spdx, marker in STANDARD.items():
    stated = any(spdx in (p.get("license") or "") for p in meta["packages"] if (p["name"], "v" + p["version"]) in linked)
    if not stated or any(marker in text for text in texts_seen):
        continue
    found = next((f for f in registry.glob("*/*/LICENSE*") if f.is_file() and marker in f.read_text(errors="replace")), None)
    if found is None:
        sys.exit(f"no {spdx} text found to include")
    appendix.append(f"The {spdx} licence, in full\n\n" + found.read_text(errors="replace").strip())

extras = [
    "Plus Jakarta Sans (the interface font, embedded)\nLicence: SIL Open Font License 1.1\n\n"
    + (root / "ui/fonts/OFL.txt").read_text().strip(),
    "yt-dlp (yt-dlp.exe, shipped beside the program)\nLicence: The Unlicense\nSource: https://github.com/yt-dlp/yt-dlp\n\n"
    "This is free and unencumbered software released into the public domain. The standalone\n"
    "build also packs Python and libraries under their own licences, listed in full in\n"
    "YT-DLP-THIRD-PARTY-LICENSES.txt next to this file.",
]
header = (
    "Featherplay includes the following third-party software. Each is used under the licence\n"
    "shown. Featherplay itself is released under the GNU General Public License, version 3\n"
    "or later (LICENSE.txt); Slint, the interface toolkit, is used under its GPL-3.0 licence.\n"
    f"\n{len(sections)} libraries are compiled into the program.\n"
)
rule = "\n\n" + "=" * 78 + "\n\n"
output.write_text(header + rule + rule.join(extras + sections + appendix) + "\n")
print(f"{output}: {len(sections)} libraries, {output.stat().st_size // 1024} KB")
