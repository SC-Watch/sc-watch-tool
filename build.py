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
Build the distributable. Fetches everything it needs.

    python build.py                # sterile copy, venv, PyInstaller, zip
    python build.py --skip-deps    # reuse the build venv, skip pip
    python build.py --clean        # delete build/ and dist/ first

WHAT IT PRODUCES

    dist/sc-watch/                 one folder, two executables, no Python
    dist/sc-watch-<version>.zip    that folder, zipped

IT BUILDS ITS OWN TOOLCHAIN

A build that says "first install these six things" is a build that stops
working the week nobody remembers which six. So this creates `.venv-build`,
installs the PINNED requirements plus PyInstaller into it, and builds there.
The versions in requirements.txt are the ones every measurement in the
engineering log was taken with; OCR output is not stable across recogniser
versions, so a build that quietly floats to a newer rapidocr is a build whose
accuracy nobody has measured.

IT BUILDS FROM THE STERILE COPY, NOT FROM HERE

make_sterile.py already answers "which files are the program" with an
allowlist. Reusing it means the build cannot accidentally freeze the corpus,
the database, or an API key into a folder that gets published - a denylist
would have to remember each of those, and this one cannot forget.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
# Derived, not hardcoded. This file is published, and an absolute path off one
# person's disk in a public repo is both broken for everyone else and a small
# piece of information about the author nobody asked for. Override with
# SC_WATCH_DIST if the distribution folder lives somewhere else.
STERILE = Path(os.environ.get("SC_WATCH_DIST") or (HERE.parent / "SC-Watch-V1"))
VENV = HERE / ".venv-build"
BUILD, DIST = HERE / "build", HERE / "dist"

# This script runs in two places and must do the right thing in both.
#
#   THE DEV TREE, which also holds the corpus, the measurement harness and a
#   database about real people. Building here directly would risk freezing any
#   of that into a published folder, so it rebuilds the sterile copy first and
#   builds from there.
#
#   THE DISTRIBUTION REPO, which is what GitHub Actions checks out. That is
#   already the sterile set - it was produced by make_sterile.py - so there is
#   nothing to strip and nowhere to strip it from.
#
# make_sterile.py is the thing that distinguishes them: it exists only in the
# dev tree, because a distribution has nothing to sterilise.
IN_DEV_TREE = (HERE / "make_sterile.py").exists()

# No Inno Setup here any more. Distribution went standalone-only on
# 2026-09-20: the installer and the zip were the same PyInstaller folder, and
# the installer only copied it under %LOCALAPPDATA%\Programs and made
# shortcuts. What testers wanted was the folder itself. The script, the search
# for it and the workflow step that ran it are gone rather than left lying
# around to be rebuilt by accident; git history has them if it is reversed.


def run(cmd: list[str], **kw) -> None:
    print("  $", " ".join(str(c) for c in cmd))
    subprocess.run([str(c) for c in cmd], check=True, **kw)


def version() -> str:
    """The version string, from VERSION if present, else a dev marker.

    A single file so the zip name, the tag check and the About line cannot
    disagree, which is the usual way a release ends up mislabelled.
    """
    f = HERE / "VERSION"
    if f.exists():
        v = f.read_text(encoding="utf-8").strip()
        if re.fullmatch(r"\d+\.\d+\.\d+", v):
            return v
        print(f"! VERSION contains {v!r}, which is not x.y.z")
    return "0.0.0"


def venv_python(skip_deps: bool) -> Path:
    """The interpreter to build with, and its toolchain.

    Always the build venv. --skip-deps only skips the pip work, because that is
    the slow part on a rebuild; it does NOT fall back to the system
    interpreter, which was the first version of this flag and which failed
    with a stack trace because PyInstaller is deliberately not installed there.
    """
    py = VENV / "Scripts" / "python.exe"
    if skip_deps:
        if not py.exists():
            raise SystemExit(
                f"--skip-deps needs an existing {VENV.name}. Run build.py once "
                f"without it.")
        print(f"\nreusing {VENV.name} as-is")
        return py
    if not VENV.exists():
        print(f"\ncreating {VENV.name}")
        run([sys.executable, "-m", "venv", str(VENV)])
    print("\ninstalling the pinned build toolchain")
    run([py, "-m", "pip", "install", "--quiet", "--upgrade", "pip"])
    run([py, "-m", "pip", "install", "--quiet",
         "-r", str(HERE / "requirements.txt")])
    # Not pinned: PyInstaller is a build tool, not a dependency of the running
    # program, and its bootloader benefits from being current.
    run([py, "-m", "pip", "install", "--quiet", "pyinstaller"])
    return py


def source_dir() -> Path:
    """The folder to freeze. See IN_DEV_TREE above."""
    if not IN_DEV_TREE:
        print("\nbuilding in place: this is already a distribution checkout")
        return HERE
    print("\nrebuilding the sterile copy")
    r = subprocess.run([sys.executable, str(HERE / "make_sterile.py")],
                       cwd=str(HERE))
    if r.returncode == 2:
        raise SystemExit(
            "make_sterile refused: something in the distribution folder looks "
            "like real data.\nCheck it, then re-run make_sterile.py --force.")
    if r.returncode == 3:
        raise SystemExit(
            "the distribution folder still holds files something else has "
            "open.\nStop any watcher or UI running from it and try again.")
    if r.returncode:
        raise SystemExit("make_sterile.py failed")
    return STERILE


def freeze(py: Path, src: Path) -> Path:
    # The spec must sit in the folder being frozen, because SPECPATH is how it
    # finds km_template.npy. In a distribution checkout it is
    # already there; in the dev tree it is copied across.
    spec = src / "sc-watch.spec"
    if spec.resolve() != (HERE / "sc-watch.spec").resolve():
        shutil.copy2(HERE / "sc-watch.spec", spec)
    print("\nfreezing")
    run([py, "-m", "PyInstaller", "--noconfirm", "--clean",
         "--distpath", str(DIST), "--workpath", str(BUILD),
         str(spec)], cwd=str(src))
    out = DIST / "sc-watch"
    if not (out / "sc-watch.exe").exists():
        raise SystemExit("PyInstaller finished but sc-watch.exe is missing")
    return out


def legal(py: Path, src: Path, folder: Path) -> None:
    """Put the licence and the third-party notices in the shipped folder.

    Generated with the BUILD interpreter, because the notices must describe the
    packages actually frozen into this bundle rather than whatever happens to
    be installed on the machine running the build script.
    """
    shutil.copy2(src / "LICENSE", folder / "LICENSE")
    run([py, str(src / "third_party.py"),
         str(folder / "THIRD-PARTY-NOTICES.txt")])
    # The user guide travels with the build, so the folder somebody unzips is
    # self-contained: the tool, its notices and the instructions in one place.
    shutil.copy2(src / "README.md", folder / "README.md")
    # No docs/ here. The engineering log is deliberately unpublished; see the
    # note in make_sterile.py. Copying whatever happens to be in a docs folder
    # would quietly undo that the moment one reappeared.


def launcher(folder: Path) -> None:
    """The double-clickable entry point, mirroring sc-watch.bat.

    Two windows, because they are two processes. Written here rather than
    shipped in the sterile copy because it names the executables, which only
    exist after freezing.
    """
    (folder / "sc-watch.bat").write_text(
        "@echo off\r\n"
        "REM Start both halves. Close either window to stop that half.\r\n"
        'cd /d "%~dp0"\r\n'
        'start "sc-watch UI" "%~dp0sc-watch-ui.exe"\r\n'
        "timeout /t 2 /nobreak >nul\r\n"
        'start "sc-watch watcher" "%~dp0sc-watch.exe" --verbose %*\r\n',
        # newline="" or the CRLFs above are written as \r\r\n: write_text
        # translates \n on Windows too. Every shipped copy of this launcher
        # has had doubled carriage returns - cmd has tolerated it, and the
        # file people double-click should still be a well-formed batch file.
        encoding="ascii", newline="")


def portable(folder: Path) -> None:
    """Make the built folder keep its own data, with nothing to switch on.

    `paths.py` puts DATA beside the executable when a file named
    portable.txt sits next to it, and otherwise in %LOCALAPPDATA%. Since
    distribution went standalone-only that default is the wrong way round:
    the thing people asked for was one folder they can find, zip and send
    back, and it arrived needing them to create a file first - which Windows,
    with extensions hidden, is apt to save as portable.txt.txt.

    So the build writes it. Deleting the file is how you opt OUT, and the
    file says so in case somebody opens it wondering what it is.
    """
    (folder / "portable.txt").write_text(
        "Keep sc-watch's data in this folder, in a subfolder called data.\r\n"
        "\r\n"
        "Delete this file to store it in %LOCALAPPDATA%\\sc-watch instead,\r\n"
        "where it survives replacing this folder with a newer one.\r\n",
        # newline="": write_text translates \n to \r\n on Windows as well,
        # so the explicit CRLFs above would have been written as \r\r\n and
        # opened in Notepad with a blank line between every line.
        encoding="ascii", newline="")


def zip_up(folder: Path, ver: str) -> Path:
    out = DIST / f"sc-watch-{ver}.zip"
    print(f"\nzipping -> {out.name}")
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for f in sorted(folder.rglob("*")):
            if f.is_file():
                z.write(f, Path("sc-watch") / f.relative_to(folder))
    return out


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--skip-deps", action="store_true",
                    help="reuse the build venv without "
                         "reinstalling into it")
    ap.add_argument("--clean", action="store_true")
    a = ap.parse_args()

    if a.clean:
        for d in (BUILD, DIST):
            shutil.rmtree(d, ignore_errors=True)
        print("cleaned build/ and dist/")

    ver = version()
    print(f"building sc-watch {ver}")
    src = source_dir()
    py = venv_python(a.skip_deps)
    folder = freeze(py, src)
    legal(py, src, folder)
    launcher(folder)
    portable(folder)

    size = sum(f.stat().st_size for f in folder.rglob("*") if f.is_file())
    print(f"\n{folder}  {size/1e6:.0f} MB, "
          f"{sum(1 for f in folder.rglob('*') if f.is_file())} files")

    z = zip_up(folder, ver)
    print(f"  {z.name}  {z.stat().st_size/1e6:.0f} MB")
    print("\nSmoke-test before publishing:")
    print(f'  "{folder}\\sc-watch.exe" --where')
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
