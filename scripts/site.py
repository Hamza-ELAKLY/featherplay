#!/usr/bin/env python3
"""Builds Featherplay's website (GitHub Pages) into a folder.

    scripts/site.py <out-dir>

The home page is site/home.html; the privacy and code signing policies are the
repository's own PRIVACY.md and CODE_SIGNING.md, turned into pages here, so the site
says exactly what the repository does. All pages share site/layout.html.

The Markdown reader handles only what those two files use (headings, paragraphs, "-"
lists, links, **bold**, *italics*, `code`) and stops at anything else, rather than
publishing it wrong. No dependencies beyond Python 3.
"""

import html
import re
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
REPO = "https://github.com/Hamza-ELAKLY/featherplay"
# Marks a folder as a build of the site, so the next build may clear it.
BUILT = ".site-build"

# Markdown files that are pages of the site, and the page each becomes.
PAGES = {
    "PRIVACY.md": ("privacy.html", "How Featherplay handles your data: what it reads, where it stays, and how to remove it."),
    "CODE_SIGNING.md": ("code-signing.html", "How Featherplay's releases are built and signed."),
}

# Copied as they are: (source in the repository, name on the site).
FILES = [
    ("site/style.css", "style.css"),
    ("ui/fonts/PlusJakartaSans.ttf", "PlusJakartaSans.ttf"),
    ("ui/fonts/OFL.txt", "PlusJakartaSans-OFL.txt"),
    ("ui/icon.png", "icon.png"),
    ("assets/showcase.webp", "showcase.webp"),
    ("assets/social-preview.png", "social-preview.png"),
]


def link(target: str) -> str:
    """Where a link in a Markdown file points on the site."""
    if re.match(r"^(https?:|mailto:|#)", target):
        return target
    page, _, anchor = target.partition("#")
    if page in PAGES:
        return PAGES[page][0] + (f"#{anchor}" if anchor else "")
    # Anything else in the repository: on GitHub.
    return f"{REPO}/blob/main/{target}"


def inline(text: str) -> str:
    """Inline Markdown to HTML: code spans first, so nothing inside them is touched."""
    out = []
    for i, part in enumerate(re.split(r"(`[^`]+`)", text)):
        if i % 2:
            out.append(f"<code>{html.escape(part[1:-1])}</code>")
            continue
        if "`" in part:
            raise ValueError(f"unmatched backtick in: {text}")
        part = html.escape(part, quote=False)
        part = re.sub(
            r"\[([^\]]+)\]\(([^)\s]+)\)",
            lambda m: f'<a href="{html.escape(link(m.group(2)))}">{m.group(1)}</a>',
            part,
        )
        part = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", part)
        part = re.sub(r"(?<![*\w])\*(?!\s)(.+?)(?<!\s)\*(?![*\w])", r"<em>\1</em>", part)
        if re.search(r"[*_]{2}|\[[^\]]*\]\(", part):
            raise ValueError(f"Markdown left over in: {text}")
        out.append(part)
    return "".join(out)


def slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


def bullets(items: list[tuple[int, str]], start: int, indent: int) -> tuple[str, int]:
    """The list starting at `items[start]`, whose items are indented by `indent`, as HTML,
    and the index after it. Deeper items nest inside the item before them."""
    parts = []
    i = start
    while i < len(items) and items[i][0] == indent:
        text = inline(items[i][1])
        i += 1
        if i < len(items) and items[i][0] > indent:
            inner, i = bullets(items, i, items[i][0])
            text += inner
        parts.append(f"<li>{text}</li>")
    if i < len(items) and items[i][0] > indent:
        raise ValueError(f"a list item indented oddly: {items[i][1]}")
    return "<ul>" + "".join(parts) + "</ul>", i


def markdown(source: str) -> tuple[str, str]:
    """The page's title (its "# " heading) and its body as HTML."""
    title = None
    out: list[str] = []
    para: list[str] = []
    items: list[tuple[int, str]] = []

    def flush() -> None:
        if para:
            out.append(f"<p>{inline(' '.join(para))}</p>")
            para.clear()
        if items:
            if items[0][0] != 0:
                raise ValueError(f"a list starting indented: {items[0][1]}")
            html_list, _ = bullets(items, 0, 0)
            out.append(html_list)
            items.clear()

    for number, line in enumerate(source.splitlines(), 1):
        stripped = line.strip()
        indent = len(line) - len(line.lstrip(" "))
        if not stripped:
            # A blank line ends a paragraph; a list goes on if an item follows.
            if para:
                flush()
            continue
        if re.match(r"^(>|```|\||\d+\.\s|<|\* |\+ )", stripped):
            raise ValueError(f"line {number}: unsupported Markdown: {line}")
        heading = re.match(r"^(#{1,3}) (.+)$", line)
        item = re.match(r"^( *)- (.+)$", line)
        if heading:
            flush()
            level, text = len(heading.group(1)), heading.group(2).strip()
            if level == 1:
                if title is not None:
                    raise ValueError(f"line {number}: a second title")
                title = text
                out.append(f"<h1>{inline(text)}</h1>")
            else:
                out.append(f'<h{level} id="{slug(text)}">{inline(text)}</h{level}>')
        elif item:
            if para:
                flush()
            items.append((len(item.group(1)), item.group(2)))
        elif items and indent > items[-1][0]:
            # The item's text, carried on to the next line.
            items[-1] = (items[-1][0], items[-1][1] + " " + stripped)
        else:
            if items:
                flush()
            para.append(stripped)
    flush()
    if title is None:
        raise ValueError("no '# ' title")
    return title, "\n".join(out)


def page(layout: str, title: str, description: str, main: str) -> str:
    return (
        layout.replace("{{title}}", html.escape(title))
        .replace("{{description}}", html.escape(description))
        .replace("{{main}}", main)
    )


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = Path(sys.argv[1])
    # Only an earlier build is ever cleared, never a folder that happens to be given.
    if out.exists() and any(out.iterdir()):
        if not (out / BUILT).is_file():
            sys.exit(f"{out} isn't empty and isn't an earlier build of the site: give a new folder")
        shutil.rmtree(out)
    out.mkdir(parents=True, exist_ok=True)
    (out / BUILT).write_text("Built by scripts/site.py; cleared by the next build.\n")
    layout = (SITE / "layout.html").read_text(encoding="utf-8")

    home = (SITE / "home.html").read_text(encoding="utf-8")
    description = (
        "Featherplay: a lightweight, portable desktop player for the audio of YouTube "
        "videos, playlists and Mixes. Free and open source, no ads, no telemetry."
    )
    (out / "index.html").write_text(
        page(layout, "Featherplay · a feather-light player for YouTube audio", description, home),
        encoding="utf-8",
    )
    for source, (name, about) in PAGES.items():
        title, body = markdown((ROOT / source).read_text(encoding="utf-8"))
        main_html = f'<article class="doc">\n{body}\n</article>'
        (out / name).write_text(page(layout, f"{title} · Featherplay", about, main_html), encoding="utf-8")
    for source, name in FILES:
        shutil.copyfile(ROOT / source, out / name)
    print(f"Built {len(PAGES) + 1} pages in {out}")


if __name__ == "__main__":
    main()
