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
Session logging, and a support bundle somebody can safely send a stranger.

THIS MODULE'S JOB IS DECIDING WHAT NOT TO INCLUDE
-------------------------------------------------
A support bundle is the only feature here whose whole purpose is to hand your
files to someone else, so the interesting question is never "what would help
debug this" - it is "what would I be sending". Three things are excluded by
construction, not by remembering:

  THE API KEY          sc_api_key.txt and the SC_API_KEY environment variable
                       are never read into the log or the bundle. The log
                       records only whether a key is present and how long it
                       is, which is all a support answer ever needs.

  YOUR JUDGEMENTS      sc-watch.db holds reputation reports about named real
                       players. It is never in a bundle at any setting. Nor is
                       watchlist.db, rsi_cache.db or sightings.csv.

Screenshots ARE included - they are most of the point, since a missed contact
is a question about pixels. But a frame is your WHOLE screen, and it can carry
a QR code, an account panel, a Discord overlay or a second monitor: things
that identify you rather than the bug. So the manifest inside the bundle says
in plain words how many full frames are in there and what they show, and
--no-frames builds a text-only bundle for anyone who would rather not. Nobody
should have to guess what they just sent.

Handles that were DETECTED are logged. That is the tool's entire subject, a
log without them cannot explain a misread, and an RSI handle is a public
account name visible to anyone in the same system. Your own reputation
judgements about those people are the private part, and those stay out.

Paths are rewritten before they are written down: a Windows profile directory
carries a real name surprisingly often.
"""

from __future__ import annotations

import json
import os
import platform
import re
import sys
import threading
import time
import zipfile
from datetime import datetime
from pathlib import Path

import paths

# The folder the user means by "the debug folder". Frames already land here,
# and housekeeping's sweep only matches *.png and *.jpg, so a log file sitting
# beside them survives the retention pass that deletes the images.
DEBUG_DIR = "debug_bursts"

# How many session logs to keep. Rotation is ours to do for the same reason:
# housekeeping will not touch a .log.
KEEP_SESSIONS = 12

# Anything matching these never enters a bundle, whatever else is asked for.
# Matched on the file NAME, so a copy in another directory is caught too.
NEVER_BUNDLE = (
    "sc-watch.db", "sc-watch.db-wal", "sc-watch.db-shm",
    "watchlist.db", "rsi_cache.db", "sightings.csv", "sc_api_key.txt",
)

_SECRET_RE = re.compile(
    r"(api[_-]?key|token|secret|password)\s*[:=]\s*\S+", re.I)


def _scrub(text: str) -> str:
    """Remove the two things that leak identity from a line of text.

    The home directory first, because it is the common one: a Windows profile
    path is often a real first and last name, and it appears in every absolute
    path the tool prints.
    """
    if not text:
        return text
    out = str(text)
    for var in ("USERPROFILE", "HOME"):
        home = os.environ.get(var)
        if home and len(home) > 3:
            out = out.replace(home, "~")
            out = out.replace(home.replace("\\", "/"), "~")
    user = os.environ.get("USERNAME") or os.environ.get("USER")
    if user and len(user) > 2:
        out = re.sub(re.escape(user), "<user>", out, flags=re.I)
    return _SECRET_RE.sub(r"\1=<redacted>", out)


def _versions() -> dict:
    out = {"python": sys.version.split()[0],
           "platform": platform.platform(),
           "machine": platform.machine()}
    for mod, attr in (("cv2", "__version__"), ("numpy", "__version__"),
                      ("onnxruntime", "__version__"),
                      ("rapidocr_onnxruntime", "__version__"),
                      ("dxcam", "__version__")):
        try:
            m = __import__(mod)
            out[mod] = str(getattr(m, attr, "installed"))
        except Exception as exc:
            out[mod] = f"MISSING ({type(exc).__name__})"
    return out


def safe_settings() -> dict:
    """The settings, with anything secret replaced by a description of it.

    A key is reported as present and how long it is. That answers every
    support question a key can cause - missing, empty, truncated by a paste -
    without the key leaving the machine.
    """
    try:
        import settings
        cur = dict(settings.load())
    except Exception as exc:
        return {"_error": f"could not read settings: {exc}"}
    for k in list(cur):
        if _SECRET_RE.match(f"{k}=x") or "key" in k.lower() and "_key" not in k.lower():
            cur[k] = "<redacted>"
    return cur


def api_key_state() -> str:
    """Whether a key exists and how long it is. Never the key."""
    env = os.environ.get("SC_API_KEY", "").strip()
    if env:
        return f"set via SC_API_KEY ({len(env)} chars)"
    try:
        f = paths.data("sc_api_key.txt")
        if f.exists():
            n = len(f.read_text(encoding="utf-8").strip())
            return f"set via sc_api_key.txt ({n} chars)"
    except Exception:
        pass
    return "not set"


class DebugLog:
    """One session's log. Append-only, flushed per line, thread-safe.

    Flushed on every write rather than buffered, because the reason a log
    exists is usually a crash, and a buffered log loses exactly the lines that
    explain one.
    """

    def __init__(self, enabled: bool = True, tag: str = ""):
        self.enabled = enabled
        self.path: Path | None = None
        self._fh = None
        self._lock = threading.Lock()
        self.counts: dict[str, int] = {}
        if not enabled:
            return
        try:
            d = paths.data_dir(DEBUG_DIR)
            stamp = datetime.now().strftime("%Y%m%d_%H%M%S")
            self.path = d / f"session_{stamp}{('_' + tag) if tag else ''}.log"
            self._fh = self.path.open("a", encoding="utf-8", newline="")
            self._rotate(d)
        except Exception:
            # A log that cannot be written must never stop the watcher.
            self.enabled = False
            self._fh = None

    def _rotate(self, d: Path):
        try:
            logs = sorted(d.glob("session_*.log"),
                          key=lambda p: p.stat().st_mtime, reverse=True)
            for old in logs[KEEP_SESSIONS:]:
                old.unlink(missing_ok=True)
        except OSError:
            pass

    # ---- writing -------------------------------------------------------

    def _write(self, line: str):
        if not self.enabled or self._fh is None:
            return
        with self._lock:
            try:
                self._fh.write(line.rstrip() + "\n")
                self._fh.flush()
            except Exception:
                self.enabled = False

    def header(self, args=None, extra: dict | None = None):
        if not self.enabled:
            return
        self._write("=" * 72)
        self._write(f"sc-watch session log   {datetime.now():%Y-%m-%d %H:%M:%S}")
        self._write("=" * 72)
        self._write("")
        self._write("-- environment")
        for k, v in _versions().items():
            self._write(f"   {k:24} {v}")
        self._write(f"   {'api key':24} {api_key_state()}")
        for line in _scrub(paths.describe()).splitlines():
            if line.strip():
                self._write(f"   {line.strip()}")
        for k, v in (extra or {}).items():
            self._write(f"   {k:24} {_scrub(str(v))}")
        self._write("")
        self._write("-- settings (secrets redacted)")
        for k, v in sorted(safe_settings().items()):
            self._write(f"   {k:24} {_scrub(json.dumps(v))}")
        if args is not None:
            self._write("")
            self._write("-- command line overrides in effect")
            try:
                for k, v in sorted(vars(args).items()):
                    self._write(f"   {k:24} {_scrub(json.dumps(v, default=str))}")
            except Exception:
                pass
        self._write("")
        self._write("-- events")

    def event(self, kind: str, **fields):
        """One structured line. Timestamped, greppable, scrubbed."""
        if not self.enabled:
            return
        self.counts[kind] = self.counts.get(kind, 0) + 1
        bits = " ".join(f"{k}={_scrub(json.dumps(v, default=str))}"
                        for k, v in fields.items())
        self._write(f"{datetime.now():%H:%M:%S.%f}"[:-3] + f" {kind:16} {bits}")

    def note(self, text: str):
        self.event("note", text=text)

    def exception(self, where: str, exc: BaseException):
        self.event("error", where=where, type=type(exc).__name__,
                   detail=str(exc)[:300])

    def close(self, summary: dict | None = None):
        if not self.enabled:
            return
        self._write("")
        self._write("-- session summary")
        for k, v in sorted(self.counts.items()):
            self._write(f"   {k:24} {v}")
        for k, v in (summary or {}).items():
            self._write(f"   {k:24} {_scrub(str(v))}")
        with self._lock:
            try:
                self._fh.close()
            except Exception:
                pass
            self._fh = None


# ---- support bundle -----------------------------------------------------

# A bundle nobody can send is not a support feature. Discord's free upload cap
# is 25 MB and most mail is 20-25 MB, so that is the target - not a number
# chosen for comfort. Measured: debug_bursts/ was 810 MB across 165 frames on
# a real machine, and a straight zip of it came to 192.5 MB.
BUNDLE_MB = 24.0

# Full frames are saved as PNG because the watcher may re-read them; for a
# bundle that is the wrong trade. A 5120x1440 burst frame is 4.7 MB as PNG and
# about 0.8 MB as JPEG q88 at the SAME resolution - six times smaller with no
# geometry lost, which is what a "why was this contact missed" question turns
# on. OCR crops are NOT re-encoded: they are a few hundred pixels, they are
# the evidence for a misread, and a JPEG artefact on a 12-pixel glyph is the
# difference between an I and a 1 - the same reason audits store crops
# losslessly in the first place.
BUNDLE_JPEG_QUALITY = 88


def _is_crop(path: Path) -> bool:
    """A lossless OCR crop rather than a full screen grab."""
    return "_crop_" in path.name


def _encoded(path: Path):
    """The bytes to put in the zip, and the name to store them under.

    Crops go in untouched. Full frames are re-encoded to JPEG at their
    original resolution, which is where the size actually comes from.
    """
    if _is_crop(path):
        return path.read_bytes(), path.name
    if path.suffix.lower() in (".jpg", ".jpeg"):
        return path.read_bytes(), path.name           # already compressed
    import cv2
    import imgio
    img = imgio.imread(path)
    if img is None:
        return path.read_bytes(), path.name
    ok, buf = cv2.imencode(".jpg", img,
                           [cv2.IMWRITE_JPEG_QUALITY, BUNDLE_JPEG_QUALITY])
    if not ok:
        return path.read_bytes(), path.name
    return buf.tobytes(), path.with_suffix(".jpg").name


def _frame_sets(root: Path, with_frames: bool) -> list[tuple[Path, str, bytes]]:
    """(source, name-inside-zip, bytes) for every image worth sending."""
    out: list[tuple[Path, str, bytes]] = []
    if not with_frames:
        return out
    picks: list[tuple[float, Path, str]] = []
    for sub in (DEBUG_DIR, "audit"):
        d = root / sub
        if not d.is_dir():
            continue
        for pat in ("*.png", "*.jpg", "*.jpeg"):
            for f in d.glob(pat):
                try:
                    st = f.stat()
                except OSError:
                    continue
                picks.append((st.st_mtime, f, sub))
    # Newest first: a bug someone is reporting now happened recently, and the
    # oldest frames in a 165-frame directory are from a different session.
    # Crops jump the queue - they are tiny and they are the misread evidence.
    picks.sort(key=lambda t: (not _is_crop(t[1]), -t[0]))
    budget = BUNDLE_MB * 1e6
    for _, f, sub in picks:
        try:
            data, name = _encoded(f)
        except OSError:
            continue
        if len(data) > budget:
            continue
        budget -= len(data)
        out.append((f, f"{sub}/{name}", data))
    return out


def build_bundle(dest: Path | None = None, with_frames: bool = True) -> dict:
    """Zip up everything a maintainer needs and nothing they do not.

    Returns a dict describing what went in, which is also what gets printed
    and written into the archive as MANIFEST.txt - so the person sending it
    can read the same list the person receiving it will.
    """
    root = paths.DATA_ROOT
    d = paths.data_dir(DEBUG_DIR)
    stamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    dest = Path(dest) if dest else d / f"sc-watch-support-{stamp}.zip"

    # Old bundles are ~20 MB each and housekeeping only sweeps images, so they
    # would sit in the debug folder forever. Keep the two most recent.
    try:
        for stale in sorted(d.glob("sc-watch-support-*.zip"),
                            key=lambda p: p.stat().st_mtime,
                            reverse=True)[2:]:
            stale.unlink(missing_ok=True)
    except OSError:
        pass

    logs = sorted(d.glob("session_*.log"),
                  key=lambda p: p.stat().st_mtime, reverse=True)[:KEEP_SESSIONS]
    images = _frame_sets(root, with_frames)
    all_imgs = 0
    if with_frames:
        for sub in (DEBUG_DIR, "audit"):
            sd_ = root / sub
            if sd_.is_dir():
                all_imgs += sum(1 for pat in ("*.png", "*.jpg", "*.jpeg")
                                for _ in sd_.glob(pat))
    n_full = sum(1 for src, _, _ in images if not _is_crop(src))
    n_crop = len(images) - n_full

    info = {
        "file": str(dest),
        "logs": len(logs),
        "images": len(images),
        "images_available": all_imgs,
        "full_frames": n_full,
        "crops": n_crop,
        "with_frames": with_frames,
    }

    manifest = [
        "sc-watch support bundle",
        f"created  {datetime.now():%Y-%m-%d %H:%M:%S}",
        "",
        "WHAT IS IN HERE",
        f"  session logs     {len(logs)} - your settings, and what the watcher",
        "                     read from each frame it looked at",
        "  settings.json    your in-tool settings, secrets replaced",
        "  flags.json       your report reasons, colours and polarity",
    ]
    if with_frames:
        manifest += [
            f"  screenshots      {len(images)} of {all_imgs} available"
            f" ({n_full} full frames, {n_crop} OCR crops)",
            "",
            "READ THIS BEFORE YOU SEND IT",
            "  A full frame is your WHOLE screen at the moment of a read. If",
            "  you had a QR code, an account page, a second monitor or a chat",
            "  overlay visible, it is in these images. Open a few and look.",
            "  For a text-only bundle: press 'text only, no screenshots'",
            "  in Settings, or run  watch.py --support-bundle --no-frames",
        ]
    else:
        manifest += ["  screenshots      NONE - built with --no-frames"]
    manifest += [
        "",
        "WHAT IS DELIBERATELY NOT IN HERE",
        "  sc-watch.db      your reputation reports about named real players",
        "  watchlist.db     third-party lists you imported",
        "  rsi_cache.db     cached profiles",
        "  sightings.csv    your encounter log",
        "  sc_api_key.txt   your API key. The logs record only whether a key",
        "                   is set and how many characters it is.",
        "",
        "  Home directory paths and your Windows username are rewritten in",
        "  the logs before they are written to disk.",
    ]

    try:
        with zipfile.ZipFile(dest, "w", zipfile.ZIP_DEFLATED) as z:
            z.writestr("MANIFEST.txt", "\n".join(manifest) + "\n")
            for lg in logs:
                z.write(lg, f"logs/{lg.name}")
            z.writestr("settings.json",
                       json.dumps(safe_settings(), indent=2, sort_keys=True))
            # Report reasons are settings too, and a custom polarity is
            # exactly the kind of thing that explains "why is this person
            # not alerting".
            try:
                import flags as _flags
                from dataclasses import asdict as _asdict
                z.writestr("flags.json", json.dumps(
                    [_asdict(f) for f in _flags.all_flags()], indent=2))
            except Exception:
                pass
            for src, name, data in images:
                if src.name in NEVER_BUNDLE:
                    continue
                z.writestr(name, data)
        info["bytes"] = dest.stat().st_size
        info["mb"] = round(info["bytes"] / 1e6, 1)
        info["ok"] = True
    except Exception as exc:
        info["ok"] = False
        info["error"] = f"{type(exc).__name__}: {exc}"
    info["manifest"] = "\n".join(manifest)
    return info
