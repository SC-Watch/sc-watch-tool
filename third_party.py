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
Generate THIRD-PARTY-NOTICES.txt from the packages actually being shipped.

    python third_party.py OUTPUT.txt

Run by build.py inside the build venv, so it describes the bundle that is
really being produced rather than a list somebody maintained by hand and
stopped updating two versions ago.

WHY THIS FILE EXISTS AT ALL

Every dependency here is permissively licensed, and every one of those licences
asks the same small thing in return: that its copyright notice travels with the
binary. MIT and BSD say so in one sentence; Apache-2.0 spells it out in section
4. Shipping an installer with none of them is the one licence obligation this
project could actually breach, and it costs one text file not to.

pyttsx3 is the only one with more than attribution attached. MPL-2.0 is
file-level copyleft, so its own source has to be obtainable by anyone who gets
a binary containing it. We do not modify it, so naming it and pointing at
upstream satisfies that - but if its files are ever patched, those patches have
to be published.

WHAT IS NOT LISTED

Build-only tools. PyInstaller runs at build time and is not part of the
program, though its bootloader IS linked into the executables; that bootloader
carries a GPL exception written for exactly this purpose, permitting it to be
bundled into an application under any licence.
"""

from __future__ import annotations

import importlib.metadata as md
import sys
from pathlib import Path

# Present in the build venv but not in the shipped program.
BUILD_ONLY = {
    "pyinstaller", "pyinstaller-hooks-contrib", "altgraph", "pefile",
    "pywin32-ctypes", "setuptools", "pip", "wheel", "packaging",
}

# Wheels whose own metadata is wrong or blank. Recording UNKNOWN in a notices
# file is worse than useless: it looks like nobody checked. Each of these was
# looked up rather than guessed.
OVERRIDES = {
    "pypiwin32": {"license": "PSF-2.0",
                  "url": "https://github.com/mhammond/pywin32",
                  "note": "A deprecated alias wheel for pywin32; same terms."},
    "pywin32": {"url": "https://github.com/mhammond/pywin32"},
    "rapidocr-onnxruntime": {"url": "https://github.com/RapidAI/RapidOCR"},
    "flatbuffers": {"url": "https://github.com/google/flatbuffers"},
    "protobuf": {"url": "https://github.com/protocolbuffers/protobuf"},
    "six": {"url": "https://github.com/benjaminp/six"},
}

# Things that ship inside another package and have their own terms. The OCR
# models are the whole reason this tool works and they are not ours; rapidocr
# vendors them without a licence file of their own, so they are named here.
EXTRA = [
    {
        "name": "PP-OCRv3 detection, recognition and classification models",
        "version": "ch_PP-OCRv3",
        "license": "Apache-2.0",
        "url": "https://github.com/PaddlePaddle/PaddleOCR",
        "note": "Distributed inside rapidocr-onnxruntime. These are the "
                "weights that read the HUD; the thresholds in this project "
                "are calibrated to their specific failure modes.",
    },
]

HEADER = """\
THIRD-PARTY NOTICES
===================

sc-watch is licensed under the GNU General Public License v3.0; see LICENSE.

This file covers the OTHER people's software distributed with it. Their
licences apply to their code, not to sc-watch, and nothing here changes the
terms of the program itself.

Every component below is permissively licensed and is compatible with GPL-3.0.
"""


def collect() -> list[dict]:
    out = []
    for dist in md.distributions():
        name = (dist.metadata["Name"] or "").strip()
        if not name or name.lower() in BUILD_ONLY:
            continue
        meta = dist.metadata
        lic = (meta.get("License-Expression")
               or meta.get("License")
               or next((c.split("::")[-1].strip()
                        for c in (meta.get_all("Classifier") or [])
                        if c.startswith("License")), "")).strip()
        if len(lic) > 80:                     # some wheels paste the full text
            lic = lic.splitlines()[0][:80]
        url = (meta.get("Home-page")
               or next((u.split(",")[-1].strip()
                        for u in (meta.get_all("Project-URL") or [])), ""))
        text = ""
        for f in dist.files or []:
            s = str(f)
            if s.endswith(("LICENSE", "LICENSE.txt", "LICENSE.md", "COPYING")) \
                    and "dist-info" in s and "/licenses/" not in s.replace("\\", "/"):
                continue
            if s.endswith(("LICENSE", "LICENSE.txt", "COPYING")):
                try:
                    text = Path(dist.locate_file(f)).read_text(
                        encoding="utf-8", errors="replace")
                    break
                except OSError:
                    pass
        entry = {"name": name, "version": dist.version, "license": lic,
                 "url": url, "text": text, "note": ""}
        entry.update(OVERRIDES.get(name.lower(), {}))
        out.append(entry)
    out += EXTRA
    return sorted(out, key=lambda d: d["name"].lower())


def render(items: list[dict]) -> str:
    parts = [HEADER, "\nComponents\n----------\n"]
    for i in items:
        parts.append(f"  {i['name']:<28} {i['version']:<14} {i['license']}\n")
    for i in items:
        parts.append("\n" + "=" * 74 + "\n")
        parts.append(f"{i['name']} {i['version']}\n")
        if i.get("url"):
            parts.append(f"{i['url']}\n")
        parts.append(f"License: {i['license']}\n")
        if i.get("note"):
            parts.append(f"\n{i['note']}\n")
        if i.get("text"):
            parts.append("\n" + i["text"].rstrip() + "\n")
        else:
            parts.append("\nNo licence text is distributed inside this "
                         "package; see the project URL above.\n")
    return "".join(parts)


def main() -> int:
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "THIRD-PARTY-NOTICES.txt")
    items = collect()
    out.write_text(render(items), encoding="utf-8")
    print(f"{out}: {len(items)} component(s), {out.stat().st_size/1000:.0f} KB")
    missing = [i["name"] for i in items if not i.get("text")]
    if missing:
        print("  no licence text bundled for:", ", ".join(missing))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
