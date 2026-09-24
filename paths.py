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
Where things live, which is not one answer.

    import paths
    paths.data("sc-watch.db")        # something we WRITE
    paths.resource("km_template.npy")  # something we only READ

THE PROBLEM THIS SOLVES

Every path in this tool used to be `Path(__file__).with_name(...)`: the
database, the settings, the audio config, the audit screenshots, the restart
flag, all sitting next to the scripts. That is exactly right while the tool is
a folder of .py files you run from, and it is wrong the moment it is installed.

An installed app lives somewhere the user cannot write. Program Files is
read-only for a normal account, so the FIRST thing sc-watch does on a fresh
install - create sc-watch.db - fails, and nothing after it works either. A
frozen build also unpacks its bundled files into a NEW temporary directory on
every launch, so anything written next to them is silently discarded when the
process exits: settings that never save, a database that resets every time.

So there are three roots, not one:

  RESOURCE   files that ship with the tool and are only read - the OCR models,
             km_template.npy, the tones. Inside the bundle when frozen, beside
             the source when not.

  DATA       everything the tool writes about you - database, settings, cache,
             audit crops, logs. Per-user and outside the install directory.

  CAPTURE    frames, which are large and disposable. Kept under DATA so one
             retention policy still covers them, and so an uninstall does not
             leave a gigabyte behind somewhere else.

RUNNING FROM SOURCE CHANGES NOTHING

When not frozen, every root is the source directory, exactly as before. The
corpus lives there, the measurement scripts assume it, and a development tree
that suddenly kept its database in AppData would be a worse tool to work on.
`python watch.py` behaves identically to how it always has.

PORTABLE INSTALLS

A file named `portable.txt` next to the executable puts DATA back beside it, in
`data/`. That is the arrangement people expect from a game utility on a USB
stick or in a folder they sync themselves, and it costs one check.
"""

from __future__ import annotations

import functools
import os
import sys
from pathlib import Path

# PyInstaller sets sys.frozen and unpacks a one-folder build's bundled data
# next to the executable, a one-file build's into sys._MEIPASS.
FROZEN = getattr(sys, "frozen", False)

if FROZEN:
    EXE_DIR = Path(sys.executable).parent
    RESOURCE_ROOT = Path(getattr(sys, "_MEIPASS", EXE_DIR))
else:
    EXE_DIR = RESOURCE_ROOT = Path(__file__).resolve().parent

PORTABLE = FROZEN and (EXE_DIR / "portable.txt").exists()


def _data_root() -> Path:
    if not FROZEN:
        return RESOURCE_ROOT              # development tree: unchanged
    if PORTABLE:
        return EXE_DIR / "data"
    # LOCALAPPDATA rather than APPDATA: this is a cache and a local database,
    # not something that should follow a roaming profile between machines. A
    # 100 MB frame directory syncing over the network would be a surprise.
    base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
    return Path(base) / "sc-watch"


DATA_ROOT = _data_root()


def resource(*parts: str) -> Path:
    """A file that ships with the tool and is only ever read."""
    return RESOURCE_ROOT.joinpath(*parts)


def data(*parts: str) -> Path:
    """A file the tool writes. The directory is created on demand.

    Created here rather than at each call site because a missing parent is the
    one failure mode every writer shares, and forgetting it in one place is how
    a feature works everywhere except the first run.
    """
    p = DATA_ROOT.joinpath(*parts)
    p.parent.mkdir(parents=True, exist_ok=True)
    return p


def data_dir(*parts: str) -> Path:
    """A directory the tool writes into, created if absent."""
    p = DATA_ROOT.joinpath(*parts)
    p.mkdir(parents=True, exist_ok=True)
    return p


@functools.lru_cache(maxsize=1)
def unused_old_data() -> Path | None:
    """An older install's data that THIS copy is not using, or None.

    Every 0.1.x build kept its data in %LOCALAPPDATA%\\sc-watch; from 0.2.0
    the zip is portable and keeps it in data\\ beside the executables. So the
    first run after an upgrade opens an empty database while the old one sits
    untouched - it looks lost, and a new one starts growing beside it. This is
    only ever REPORTED: moving somebody's database for them is not a thing
    to do silently.

    Quiet once the data here holds at least as many players as the old
    database - which is what a copy looks like - and for an old install
    that never recorded anybody. Answered once per process: the page asks on
    every update, and after copying you restart anyway.
    """
    if not PORTABLE:
        return None
    base = os.environ.get("LOCALAPPDATA")
    if not base:
        return None
    old = Path(base) / "sc-watch"
    try:
        if old.resolve() == DATA_ROOT.resolve() or not (old / "sc-watch.db").is_file():
            return None
    except OSError:
        return None
    try:
        import sqlite3

        def players(db: Path) -> int:
            if not db.is_file():
                return 0
            con = sqlite3.connect(db.resolve().as_uri() + "?mode=ro", uri=True)
            try:
                return con.execute("SELECT COUNT(*) FROM players").fetchone()[0]
            finally:
                con.close()

        if players(DATA_ROOT / "sc-watch.db") >= players(old / "sc-watch.db"):
            return None
    except Exception:
        pass                # cannot tell: say so rather than stay quiet
    return old


def describe() -> str:
    """One block of text saying where everything is, for the banner and --where.

    Worth printing, because "my settings did not save" and "where did my
    database go" are the two questions an installed build invites, and both are
    answered by knowing these three lines.
    """
    mode = ("installed" if FROZEN and not PORTABLE else
            "portable" if PORTABLE else "source")
    out = (f"  mode      {mode}\n"
           f"  program   {RESOURCE_ROOT}\n"
           f"  your data {DATA_ROOT}")
    old = unused_old_data()
    if old:
        out += f"\n  NOT USED  {old} (an older install's data - see Upgrading in the README)"
    return out


if __name__ == "__main__":
    print(describe())
