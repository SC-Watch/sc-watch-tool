# sc-watch - reads player handles off the Star Citizen HUD.
# Copyright (C) 2026 SC-Watch
#
# This program is free software: you can redistribute it and/or modify it
# under the terms of the GNU General Public License as published by the Free
# Software Foundation, either version 3 of the License, or (at your option)
# any later version.
#
# This program is distributed in the hope that it will be useful, but WITHOUT
# ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
# FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
# more details. You should have received a copy of the GNU General Public
# License along with this program. If not, see <https://www.gnu.org/licenses/>.

"""
Sanity-check the UI page: ui/index.html, ui/app.css, ui/app.js.

    python check_page.py

The page used to live inside ui_server.py as a 2,350-line Python string, and
nothing checked it: `py_compile` is perfectly happy with JavaScript that will
not parse, so a broken page reached the browser and the only symptom was a
panel that rendered nothing. Two real ones, both caught by eye:

  - `#44soc6f7f` written as a CSS colour
  - a backslash-n that survived one layer of escaping but not two, arriving
    as a REAL newline inside a single-quoted JS string: "SyntaxError: Invalid
    or unexpected token", entire settings panel blank

Moving the page into real files removed the second layer of escaping, which
was the cause of the second bug. The checks stay, because a newline in a
quoted string and a mistyped colour are still easy to write by hand. One is
added: index.html must pull in both files and carry no inline <script> or
<style> of its own, so the page cannot quietly drift back into one blob.

A first attempt counted quotes per line and produced 23 findings of which 21
were prose apostrophes and CSS comments - a checker nobody will trust is worse
than no checker, so this one walks the text properly instead of guessing.

No node required.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

HERE = Path(__file__).parent

# A '/' starts a regex literal rather than division when the previous
# meaningful character is one of these. The standard heuristic; without it a
# pattern like /[&<>"]/g looks like an opening double quote.
_REGEX_OK_AFTER = set("(,=:[!&|?{};+-*%~^<>") | {""}


UI = HERE / "ui"


def unterminated_strings(js: str, base_line: int) -> list[str]:
    """Every ' or " string that meets a newline before its closing quote."""
    out, i, line, n = [], 0, base_line, len(js)
    prev_sig = ""
    while i < n:
        c = js[i]
        if c == "\n":
            line += 1; i += 1; continue
        if c in " \t":
            i += 1; continue
        # comments
        if c == "/" and i + 1 < n and js[i + 1] == "/":
            while i < n and js[i] != "\n":
                i += 1
            continue
        if c == "/" and i + 1 < n and js[i + 1] == "*":
            j = js.find("*/", i + 2)
            j = n if j < 0 else j + 2
            line += js.count("\n", i, j); i = j; continue
        # regex literal
        if c == "/" and prev_sig in _REGEX_OK_AFTER:
            i += 1
            while i < n and js[i] not in "\n/":
                i += 2 if js[i] == "\\" else 1
            i += 1
            prev_sig = "/"
            continue
        # template literal: newlines are legal, ${} may nest quotes
        if c == "`":
            i += 1; depth = 0
            while i < n:
                if js[i] == "\\":
                    i += 2; continue
                if js[i] == "\n":
                    line += 1
                elif js[i] == "$" and i + 1 < n and js[i + 1] == "{":
                    depth += 1; i += 1
                elif js[i] == "}" and depth:
                    depth -= 1
                elif js[i] == "`" and not depth:
                    break
                i += 1
            i += 1; prev_sig = "`"
            continue
        # the thing we are actually looking for
        if c in "'\"":
            q, start, i = c, line, i + 1
            while i < n and js[i] != q:
                if js[i] == "\\":
                    i += 2; continue
                if js[i] == "\n":
                    out.append(f"app.js line {start}: {q} string hits a raw newline "
                               f"- JS cannot span lines with {q}")
                    break
                i += 1
            i += 1
            prev_sig = q
            continue
        prev_sig = c
        i += 1
    return out


def bad_colours(css: str) -> list[str]:
    """A #hex colour that is not 3, 6 or 8 hex digits."""
    out = []
    css = re.sub(r"/\*.*?\*/", lambda x: "\n" * x.group(0).count("\n"),
                 css, flags=re.S)
    for n, line in enumerate(css.splitlines(), 1):
        # Only VALUES, never selectors. `#modal{...}` and `#grid .tile`
        # are ids; a colour only ever appears after a property colon. The
        # first attempt checked every #token on the line and reported 32
        # selectors as broken colours.
        for decl in re.findall(r":([^;{}]*)", line):
            for tok in re.findall(r"#[0-9a-zA-Z]{2,10}", decl):
                if not re.fullmatch(
                        r"#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})",
                        tok):
                    out.append(f"app.css line {n}: '{tok}' is not a colour")
    return out


def wiring(html: str) -> list[str]:
    """index.html loads both files and keeps nothing inline."""
    out = []
    if '<link rel="stylesheet" href="/app.css">' not in html:
        out.append("index.html does not load /app.css")
    if '<script src="/app.js"></script>' not in html:
        out.append("index.html does not load /app.js")
    if re.search(r"<style[\s>]", html):
        out.append("index.html has an inline <style> - put it in app.css")
    if re.search(r"<script>(?!</script>)", html):
        out.append("index.html has an inline <script> - put it in app.js")
    return out


def main() -> int:
    try:
        html, css, js = ((UI / n).read_text(encoding="utf-8")
                         for n in ("index.html", "app.css", "app.js"))
    except OSError as exc:
        print(f"cannot read the page: {exc}")
        return 1
    problems = wiring(html) + bad_colours(css) + unterminated_strings(js, 1)
    print(f"page: index.html {len(html.splitlines())} lines, "
          f"app.css {len(css.splitlines())}, app.js {len(js.splitlines())}")
    for p in problems:
        print("  !", p)
    print("problems:", len(problems) or "none")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
