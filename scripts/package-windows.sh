#!/usr/bin/env bash
# Builds the portable Windows release: dist/Featherplay-<version>-windows-x64.zip, the bare
# executable the in-app updater fetches (dist/featherplay-windows-x64.exe), and
# SHA256SUMS.txt for both. All three go on the GitHub release. Needs the x86_64-pc-windows-gnu Rust target and mingw-w64 (cross-building
# from Linux), python3 and curl.
set -euo pipefail
cd "$(dirname "$0")/.."

target=x86_64-pc-windows-gnu
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
repository=$(sed -n 's/^repository = "\(.*\)"/\1/p' Cargo.toml | head -1)
name="Featherplay-$version-windows-x64"
stage="target/package/Featherplay"

cargo build --release --target "$target"

rm -rf target/package && mkdir -p "$stage/data" dist
cp "target/$target/release/featherplay.exe" "$stage/"

# yt-dlp's official standalone build, checked against the checksums published with it.
# Its version is pinned first, so the licences below are the ones for the same build.
ytdlp_tag=$(curl -fsSL -o /dev/null -w '%{url_effective}' https://github.com/yt-dlp/yt-dlp/releases/latest | sed 's|.*/tag/||')
[ -n "$ytdlp_tag" ] || { echo "couldn't find yt-dlp's latest version" >&2; exit 1; }
ytdlp="https://github.com/yt-dlp/yt-dlp/releases/download/$ytdlp_tag"
curl -fsSL -o "$stage/yt-dlp.exe" "$ytdlp/yt-dlp.exe"
expected=$(curl -fsSL "$ytdlp/SHA2-256SUMS" | awk '$2 == "yt-dlp.exe" { print $1 }')
actual=$(sha256sum "$stage/yt-dlp.exe" | cut -d' ' -f1)
[ -n "$expected" ] && [ "$expected" = "$actual" ] || { echo "yt-dlp.exe checksum mismatch" >&2; exit 1; }
# The standalone yt-dlp packs Python and other libraries; their licences travel with it.
curl -fsSL "https://raw.githubusercontent.com/yt-dlp/yt-dlp/$ytdlp_tag/THIRD_PARTY_LICENSES.txt" \
    | sed 's/\r$//; s/$/\r/' > "$stage/YT-DLP-THIRD-PARTY-LICENSES.txt"
[ -s "$stage/YT-DLP-THIRD-PARTY-LICENSES.txt" ] || { echo "yt-dlp's licences are missing" >&2; exit 1; }

# Text files get Windows line endings, so Notepad on any version shows them right.
sed -e "s|{version}|$version|" -e "s|{repository}|$repository|" packaging/windows/README.txt | sed 's/$/\r/' > "$stage/README.txt"
sed 's/$/\r/' LICENSE > "$stage/LICENSE.txt"
python3 scripts/notices.py "$target" target/package/notices.txt
sed 's/$/\r/' target/package/notices.txt > "$stage/THIRD-PARTY-NOTICES.txt"

python3 - "$stage" "dist/$name.zip" <<'PY'
import pathlib, sys, zipfile
stage, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for path in sorted(stage.rglob("*")):
        arcname = path.relative_to(stage.parent).as_posix() + ("/" if path.is_dir() else "")
        z.write(path, arcname)
PY
# The bare executable as well, under a fixed name: what the in-app updater downloads.
cp "$stage/featherplay.exe" dist/featherplay-windows-x64.exe
(cd dist && sha256sum "$name.zip" featherplay-windows-x64.exe > SHA256SUMS.txt)
ls -l "dist/$name.zip" dist/featherplay-windows-x64.exe && cat dist/SHA256SUMS.txt
