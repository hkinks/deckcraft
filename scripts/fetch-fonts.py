#!/usr/bin/env python3
"""Download the presentation fonts the agent server offers the web editor (fonts on demand).

All are SIL Open Font License fonts from the google/fonts repository, pinned to one commit, and
land in server/fonts/<dir>/ with their licence, plus server/fonts/catalog.json, which the server
serves at /fonts/catalog.json. The browser loads a family's files the first time a deck uses it or
the person picks it. Run again to update after changing FAMILIES; files already present are kept.

    scripts/fetch-fonts.py [--out server/fonts]
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.request
from pathlib import Path

COMMIT = "e7db6ca603fd4862010383ff17ffd6cc4ffac1c0"
RAW = f"https://raw.githubusercontent.com/google/fonts/{COMMIT}/ofl"
API = f"https://api.github.com/repos/google/fonts/contents/ofl/{{dir}}?ref={COMMIT}"

# (directory in google/fonts/ofl, family name as the font names itself, category)
FAMILIES = [
    # Metric-compatible stand-ins for Office's fonts: same widths, so lines break as in PowerPoint.
    ("carlito", "Carlito", "office"),
    ("caladea", "Caladea", "office"),
    ("arimo", "Arimo", "office"),
    ("tinos", "Tinos", "office"),
    ("cousine", "Cousine", "office"),
    # Sans serif
    ("inter", "Inter", "sans"),
    ("roboto", "Roboto", "sans"),
    ("opensans", "Open Sans", "sans"),
    ("lato", "Lato", "sans"),
    ("montserrat", "Montserrat", "sans"),
    ("poppins", "Poppins", "sans"),
    ("nunitosans", "Nunito Sans", "sans"),
    ("raleway", "Raleway", "sans"),
    ("worksans", "Work Sans", "sans"),
    ("dmsans", "DM Sans", "sans"),
    ("ibmplexsans", "IBM Plex Sans", "sans"),
    ("firasans", "Fira Sans", "sans"),
    ("ptsans", "PT Sans", "sans"),
    ("manrope", "Manrope", "sans"),
    ("rubik", "Rubik", "sans"),
    ("barlow", "Barlow", "sans"),
    ("notosans", "Noto Sans", "sans"),
    ("outfit", "Outfit", "sans"),
    ("plusjakartasans", "Plus Jakarta Sans", "sans"),
    ("spacegrotesk", "Space Grotesk", "sans"),
    ("quicksand", "Quicksand", "sans"),
    # Display
    ("oswald", "Oswald", "display"),
    # Serif
    ("merriweather", "Merriweather", "serif"),
    ("playfairdisplay", "Playfair Display", "serif"),
    ("lora", "Lora", "serif"),
    ("ptserif", "PT Serif", "serif"),
    ("librebaskerville", "Libre Baskerville", "serif"),
    ("ebgaramond", "EB Garamond", "serif"),
    ("notoserif", "Noto Serif", "serif"),
    ("crimsontext", "Crimson Text", "serif"),
    # Monospace
    ("jetbrainsmono", "JetBrains Mono", "mono"),
]

# Static families ship a file per weight; these are the ones a deck needs.
STATIC_STYLES = re.compile(r"-(Regular|Italic|Bold|BoldItalic|Light|LightItalic|Medium|MediumItalic|SemiBold|SemiBoldItalic)\.ttf$")


def get(url: str) -> bytes:
    req = urllib.request.Request(url, headers={"user-agent": "deckcraft-fetch-fonts"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def files_of(d: str) -> list[str]:
    names = [e["name"] for e in json.loads(get(API.format(dir=d))) if e["type"] == "file"]
    ttf = [n for n in names if n.endswith(".ttf")]
    variable = [n for n in ttf if "[" in n]
    chosen = variable or [n for n in ttf if STATIC_STYLES.search(n)]
    licence = [n for n in names if n.upper() in ("OFL.TXT", "LICENSE.TXT")]
    return sorted(chosen) + licence


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(Path(__file__).resolve().parent.parent / "server" / "fonts"))
    out = Path(ap.parse_args().out)
    out.mkdir(parents=True, exist_ok=True)
    catalog = []
    for d, family, category in FAMILIES:
        try:
            names = files_of(d)
        except Exception as e:  # noqa: BLE001 - report and go on with the other families
            print(f"{family}: can't list ofl/{d}: {e}", file=sys.stderr)
            continue
        files, size = [], 0
        for n in names:
            dest = out / d / n
            if not dest.exists():
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(get(f"{RAW}/{d}/{urllib.request.quote(n)}"))
            if n.endswith(".ttf"):
                files.append(f"{d}/{n}")
                size += dest.stat().st_size
        catalog.append({"family": family, "category": category, "files": files, "bytes": size, "license": "OFL-1.1"})
        print(f"{family}: {len(files)} file(s), {size // 1024} KiB")
    (out / "catalog.json").write_text(json.dumps({"source": f"google/fonts@{COMMIT}", "families": catalog}, indent=1))
    print(f"{len(catalog)} families in {out / 'catalog.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
