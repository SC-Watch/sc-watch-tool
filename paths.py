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


def describe() -> str:
    """One block of text saying where everything is, for the banner and --where.

    Worth printing, because "my settings did not save" and "where did my
    database go" are the two questions an installed build invites, and both are
    answered by knowing these three lines.
    """
    mode = ("installed" if FROZEN and not PORTABLE else
            "portable" if PORTABLE else "source")
    return (f"  mode      {mode}\n"
            f"  program   {RESOURCE_ROOT}\n"
            f"  your data {DATA_ROOT}")


if __name__ == "__main__":
    print(describe())
