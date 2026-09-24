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
One way to read and write a JSON config file.

settings.json, audio.json, housekeeping.json and flags.json each had their own
copy of load and save, and the copies had drifted. Measured against the same
eight bad inputs:

  - housekeeping read with NO encoding - Windows' locale codepage, no BOM
    handling - so a file saved by Notepad was silently ignored. The same bug
    that destroyed settings.json once, still live in the fourth copy.
  - audio accepted any type for any field: `"volume": Infinity` became inf and
    `"volume": "lots"` became the string 'lots'.
  - three of the four wrote non-atomically, straight over the file.
  - and in ALL FOUR, a file that could not be read was replaced by defaults
    and then OVERWRITTEN by the next save. That is how the BOM bug turned a
    parse error into lost settings, and it would have done the same to
    somebody's custom report reasons.

So this module does these things, the same way for every file:

  READS what people actually write: a BOM (Notepad), or the Windows codepage
  (an older editor saving "Café" as ANSI) when the bytes are not UTF-8.

  TREATS NaN and Infinity as "no value". Python's json accepts them; no other
  JSON reader does, and no setting here means infinity.

  NEVER DESTROYS A FILE IT COULD NOT READ. Before write() replaces a file
  that does not parse, the original is copied aside as
  `<name>.unreadable-<time>.json`. Defaults are still used - refusing to start
  over a bad config is worse - but the user's version is recoverable.

  WRITES ATOMICALLY: a temp file in the same folder, then os.replace, so a
  crash or a full disk mid-write leaves the old file rather than half of one.

  SURVIVES BEING READ WHILE IT IS WRITTEN. Windows will not replace a file
  another thread or process has open, and a read that lands mid-replace is
  refused too. With the UI's event stream reading settings while a click
  saved them, measured on the old code: 40% of saves failed with "access
  denied", 5% of reads returned DEFAULTS instead of the file, and two saves
  at once could write those defaults over it. Readers hold the file for
  microseconds, so both sides now retry briefly; writers to one file queue
  behind a lock; and update() - read, change, write - refuses to write at all
  if the file exists but could not be read, rather than saving defaults over
  the top of it.
"""

from __future__ import annotations

import json
import math
import os
import shutil
import sys
import tempfile
import threading
import time
from datetime import datetime
from pathlib import Path

# How long to keep retrying a Windows sharing violation. A reader holds a
# settings file for tens of microseconds; this is generous by three orders of
# magnitude and still short enough that a genuinely locked file fails fast.
RETRY_SECONDS = 0.5

# Problems already reported, keyed on (path, mtime, size): the UI reads
# settings on every request, and one broken file should be one line, not one
# per poll. A new edit is a new key, so it gets reported afresh.
_reported: set = set()
_report_lock = threading.Lock()

# One lock per file, so writers in this process queue instead of colliding.
_file_locks: dict[str, threading.RLock] = {}
_file_locks_guard = threading.Lock()

# Distinguishes "there is no file" from a file that says `null`.
_MISSING = object()


def _lock_for(path: Path) -> threading.RLock:
    key = str(path.resolve()).lower()
    with _file_locks_guard:
        lock = _file_locks.get(key)
        if lock is None:
            lock = _file_locks[key] = threading.RLock()
        return lock


def _retrying(fn):
    """fn(), retried while Windows reports a sharing violation."""
    deadline = time.monotonic() + RETRY_SECONDS
    while True:
        try:
            return fn()
        except PermissionError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.005)


def _no_constant(name):
    """NaN, Infinity and -Infinity parse as None - "no value" - which every
    caller already handles by falling back to that field's default."""
    return None


def _decode(blob: bytes) -> str:
    try:
        return blob.decode("utf-8-sig")
    except UnicodeDecodeError:
        # Not UTF-8, so almost certainly the Windows ANSI codepage - which is
        # what an editor that is not Notepad-on-Windows-11 saves by default.
        # "replace" because cp1252 leaves five byte values undefined.
        return blob.decode("cp1252", errors="replace")


class _Unreadable(Exception):
    """The file exists but could not be opened - not the same as corrupt."""


def _parse(path: Path):
    """(data, problem). data is _MISSING if there is no file.

    Raises _Unreadable if the file exists and cannot be READ even after
    retrying. That is kept apart from "read, but not valid JSON" because the
    two call for opposite things: a corrupt file is replaced by defaults, a
    file we merely could not open must not be.
    """
    try:
        blob = _retrying(path.read_bytes)
    except FileNotFoundError:
        return _MISSING, None
    except OSError as exc:
        raise _Unreadable(exc.strerror or str(exc)) from None
    try:
        return json.loads(_decode(blob), parse_constant=_no_constant), None
    except ValueError as exc:
        return None, f"is not valid JSON ({exc})"


def _wrong_shape(data, expect) -> str | None:
    """Why parsed data is unusable as `expect`, or None if it is fine."""
    if expect is None:
        return None
    if data is None:
        return "is empty (null)"
    if not isinstance(data, expect):
        return f"holds a {type(data).__name__}, not a {expect.__name__}"
    return None


def read(path: Path | str, expect: type | None = dict):
    """The parsed file, or None if it is missing or unusable. Never raises.

    `expect` is the top-level type the caller needs - a dict for settings, a
    list for flags. Anything else counts as unusable, like a parse error.
    Problems are reported once on stderr.
    """
    path = Path(path)
    try:
        data, problem = _parse(path)
    except _Unreadable as exc:
        _report(path, f"{path.name} cannot be opened ({exc}) - using defaults "
                      f"for now.")
        return None
    if data is _MISSING:
        return None
    problem = problem or _wrong_shape(data, expect)
    if problem:
        _report(path, f"{path.name} {problem} - using defaults. Your file is "
                      f"kept as a copy before anything overwrites it.")
        return None
    return data


def write(path: Path | str, data) -> None:
    """Replace the file atomically, keeping a copy of it first if it could
    not be read. Raises on failure - a save that did not happen must say so.
    """
    path = Path(path)
    with _lock_for(path):
        path.parent.mkdir(parents=True, exist_ok=True)
        # "Could not be read" means what read() means by it - including a
        # file that parses but is the wrong shape, like `null`, or a list
        # where an object belongs. read() fell back to defaults for those as
        # well, so overwriting them without a copy would lose them just the
        # same. A file that could not even be OPENED is left to os.replace,
        # which will fail on it too and say so.
        try:
            existing, problem = _parse(path)
        except _Unreadable:
            existing, problem = _MISSING, None
        if existing is not _MISSING and (
                problem or _wrong_shape(existing, type(data))):
            keep_aside(path)
        text = json.dumps(data, indent=2, allow_nan=False)
        # A unique temp name, not `<name>.tmp`: two processes can save.
        fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=path.name + ".",
                                   suffix=".tmp")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                f.write(text)
            _retrying(lambda: os.replace(tmp, path))
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise


def update(path: Path | str, change, expect: type | None = dict):
    """Read, change, write, as one step. Returns what was written.

    `change(current)` receives the parsed file - None if it is missing or
    unusable - and returns the new contents. Everything happens under the
    file's lock, so two saves in this process cannot interleave.

    If the file EXISTS but cannot be opened, this raises instead of calling
    `change` with None: that None would mean "use the defaults", and writing
    defaults over a file that is fine but momentarily locked is precisely how
    concurrent saves used to wipe settings.
    """
    path = Path(path)
    with _lock_for(path):
        try:
            data, problem = _parse(path)
        except _Unreadable as exc:
            raise OSError(f"{path.name} is in use and could not be read "
                          f"({exc}); nothing was saved") from None
        if data is _MISSING or problem or _wrong_shape(data, expect):
            data = None
        new = change(data)
        write(path, new)
        return new


def keep_aside(path: Path) -> Path | None:
    """Copy an unreadable file to <stem>.unreadable-<time><suffix>."""
    dest = path.with_name(f"{path.stem}.unreadable-"
                          f"{datetime.now():%Y%m%d-%H%M%S}{path.suffix}")
    try:
        shutil.copy2(path, dest)
    except OSError:
        return None
    _report(path, f"{path.name} could not be read, so it was kept as "
                  f"{dest.name} before being replaced.", force=True)
    return dest


def _report(path: Path, message: str, force: bool = False):
    try:
        st = path.stat()
        key = (str(path), st.st_mtime_ns, st.st_size, force)
    except OSError:
        key = (str(path), 0, 0, force)
    with _report_lock:
        if key in _reported:
            return
        _reported.add(key)
    print(f"! {message}", file=sys.stderr)


def coerce(default, value):
    """`value` if it has the same type as `default`, else `default`.

    For configs with no schema of their own: the dataclass defaults ARE the
    schema. int and float interchange where that is lossless; bool is strict,
    because True is an int in Python and `"volume": true` is not a volume.
    Non-finite numbers are refused.
    """
    if isinstance(default, bool):
        return value if isinstance(value, bool) else default
    if isinstance(default, int):
        if isinstance(value, bool):
            return default
        if isinstance(value, int):
            return value
        if isinstance(value, float) and math.isfinite(value) and value.is_integer():
            return int(value)
        return default
    if isinstance(default, float):
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            return default
        return float(value) if math.isfinite(value) else default
    if isinstance(default, (str, list, dict)):
        return value if isinstance(value, type(default)) else default
    return value if value is not None else default
