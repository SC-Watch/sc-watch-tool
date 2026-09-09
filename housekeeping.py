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
Retention for captured frames.

Live captures are ~11MB each and accumulate fast - the frame directories hit
4GB during development. Everything here is expressed as a policy object rather
than hardcoded, so the UI can edit and persist it later without touching code.

    python housekeeping.py            # report what would be deleted
    python housekeeping.py --apply    # actually delete
"""

from __future__ import annotations

import argparse
import json
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
import paths

CONFIG_PATH = paths.data("housekeeping.json")

# Directories the tool writes frames into. Anything not listed is left alone,
# and the omissions are deliberate:
#
#   frames/     converted JXR corpus - reference data, not capture output
#   corpus/     the 48-frame validation set every change is checked against
#   evidence/   frames backing measured claims in the README
#   glyphs*/    training samples
#
# Those four are inputs to the work, not exhaust from it. Deleting them would
# cost measurements that cannot be retaken.
MANAGED = ("live_frames", "debug_bursts", "probe_frames", "detections")

# Both extensions: crops land as .png, full frames as .jpg, and a sweep that
# only globbed *.png would quietly leave half a directory behind.
PATTERNS = ("*.png", "*.jpg")


@dataclass
class Policy:
    """Retention settings. Serialised to housekeeping.json for the UI."""

    enabled: bool = True
    max_age_hours: float = 24.0
    # A hard floor so a busy session can't fill the disk before age kicks in.
    # 0 disables it.
    max_files_per_dir: int = 0
    # Never delete below this many, however old - keeps the most recent frames
    # around for diagnosing whatever just went wrong.
    keep_newest: int = 20
    directories: list[str] = field(default_factory=lambda: list(MANAGED))
    # Audits are pruned by DATABASE ROW, not by scanning a directory: the row
    # owns its images, and deleting one without the other leaves either an
    # entry with no picture to look at or files nothing references. A week,
    # because that is roughly how long a read stays worth checking - and a
    # confirmed one is deleted the moment you confirm it, so this only ever
    # catches the ones nobody got round to.
    audit_max_age_days: float = 7.0

    @classmethod
    def load(cls, path: Path | None = None) -> "Policy":
        """Retention settings, from settings.json.

        settings.py is the single source now that the UI edits these. The old
        housekeeping.json is still read FIRST as a fallback for an install that
        predates the settings tab, then overlaid - so upgrading does not
        silently reset someone's retention to the defaults, but anything they
        touch in the UI afterwards wins.
        """
        path = Path(path) if path else CONFIG_PATH
        pol = cls()
        if path.exists():                       # legacy file, pre-settings tab
            try:
                pol = cls(**json.loads(path.read_text()))
            except Exception:
                pol = cls()
        try:
            import settings as settings_mod
            cur = settings_mod.load()
            pol.enabled = bool(cur["housekeeping_enabled"])
            pol.max_age_hours = float(cur["max_age_hours"])
            pol.keep_newest = int(cur["keep_newest"])
            pol.max_files_per_dir = int(cur["max_files_per_dir"])
            pol.audit_max_age_days = float(cur["audit_max_age_days"])
        except Exception:
            pass                                # settings unreadable: defaults
        # A legacy file carries whatever directory list was current when it was
        # written, so a directory added since would never be swept. Union, so
        # new managed directories are picked up without discarding a custom one.
        pol.directories = list(dict.fromkeys(list(pol.directories) + list(MANAGED)))
        return pol

    def save(self, path: Path | None = None):
        path = Path(path) if path else CONFIG_PATH
        path.write_text(json.dumps(asdict(self), indent=2))


@dataclass
class Sweep:
    directory: str
    examined: int = 0
    deleted: int = 0
    bytes_freed: int = 0
    kept: int = 0


def sweep(policy: Policy, root: Path | None = None, apply: bool = False) -> list[Sweep]:
    root = root or paths.DATA_ROOT
    results = []
    if not policy.enabled:
        return results

    cutoff = time.time() - policy.max_age_hours * 3600
    for name in policy.directories:
        d = root / name
        if not d.is_dir():
            continue
        # stat() ONCE per file, up front, and tolerate the file being gone.
        # This runs while the watcher may be writing a burst into the same
        # directory, so a name from glob() can be deleted before it is
        # measured. The old code called p.stat() inside the sort key, which
        # meant one vanished frame raised FileNotFoundError out of sorted()
        # and run_now() reported "frame sweep failed" - housekeeping quietly
        # stopped doing anything for the rest of that session.
        stats = []
        for pat in PATTERNS:
            for f in d.glob(pat):
                try:
                    st = f.stat()
                except OSError:
                    continue                # gone already; nothing to free
                stats.append((f, st.st_mtime, st.st_size))
        stats.sort(key=lambda t: t[1], reverse=True)
        res = Sweep(directory=name, examined=len(stats))

        # Newest N are always safe, whatever the age rule says.
        protected = {f for f, _, _ in stats[:policy.keep_newest]}

        for i, (f, mtime, size) in enumerate(stats):
            if f in protected:
                res.kept += 1
                continue
            too_old = mtime < cutoff
            too_many = policy.max_files_per_dir and i >= policy.max_files_per_dir
            if not (too_old or too_many):
                res.kept += 1
                continue
            if apply:
                try:
                    f.unlink()
                except OSError:
                    res.kept += 1
                    continue
            res.deleted += 1
            res.bytes_freed += size
        results.append(res)
    return results


def summarise(results: list[Sweep], applied: bool) -> str:
    if not results:
        return "housekeeping: nothing to do"
    total = sum(r.bytes_freed for r in results)
    n = sum(r.deleted for r in results)
    verb = "freed" if applied else "would free"
    parts = [f"{r.directory}: {r.deleted}/{r.examined} ({r.bytes_freed/1e6:.0f} MB)"
             for r in results if r.examined]
    return f"housekeeping {verb} {total/1e6:.0f} MB across {n} file(s) | " + \
           " | ".join(parts)


def prune_audit_rows(policy: Policy, apply: bool = False) -> tuple[int, int]:
    """Delete stale audits and any image no row points at.

    Audits are pruned by DATABASE ROW, never by scanning audit/ - the row owns
    its images, and deleting one without the other leaves either an entry with
    no picture to look at or files nothing references. Returns
    (audits removed, orphan files removed).
    """
    import reputation
    db = reputation.Database()
    orphans = db.prune_orphan_audit_files(apply=apply)
    # Audits whose player row is gone are uncountable work: the header tallies
    # them and no tile can offer a button for them.
    dangling = db.orphan_audits(apply=apply)
    if apply:
        removed = db.prune_audits(policy.audit_max_age_days)["removed"]
    else:
        cutoff = time.time() - policy.audit_max_age_days * 86400.0
        removed = sum(1 for a in db.open_audits() if a["ts"] < cutoff)
    return removed + len(dangling), len(orphans)


def run_now(apply: bool = True, policy: Policy | None = None) -> str:
    """One sweep of everything, as a single line of text.

    Wrapped so the watcher can call it at startup without a failure here
    stopping a session from starting: a full disk is a problem, but so is a
    tool that will not launch because a file was locked.
    """
    policy = policy or Policy.load()
    try:
        results = sweep(policy, apply=apply)
        line = summarise(results, apply)
    except Exception as exc:
        return f"housekeeping: frame sweep failed ({type(exc).__name__}: {exc})"
    try:
        audits, orphans = prune_audit_rows(policy, apply=apply)
        if audits or orphans:
            line += f" | audits: {audits} pruned, {orphans} orphan file(s)"
    except Exception as exc:
        line += f" | audits skipped ({type(exc).__name__})"
    return line


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--apply", action="store_true", help="actually delete")
    ap.add_argument("--max-age-hours", type=float)
    ap.add_argument("--keep-newest", type=int)
    ap.add_argument("--max-files", type=int, dest="max_files_per_dir")
    ap.add_argument("--save", action="store_true",
                    help="persist the given values to housekeeping.json")
    args = ap.parse_args()

    policy = Policy.load()
    for attr in ("max_age_hours", "keep_newest", "max_files_per_dir"):
        val = getattr(args, attr, None)
        if val is not None:
            setattr(policy, attr, val)
    if args.save:
        policy.save()
        print(f"saved {CONFIG_PATH.name}")

    print(f"policy: enabled={policy.enabled} max_age={policy.max_age_hours}h "
          f"keep_newest={policy.keep_newest} max_files={policy.max_files_per_dir or 'off'}")
    results = sweep(policy, apply=args.apply)
    print(summarise(results, args.apply))

    # Audits live in the database with their images alongside, so they are
    # pruned through reputation.py rather than by scanning audit/ - the row and
    # its files have to go together or you are left with an entry that has no
    # picture to look at.
    try:
        import reputation
        db = reputation.Database()
        n = len(db.open_audits())
        dangling = db.orphan_audits(apply=False)
        cutoff = time.time() - policy.audit_max_age_days * 86400.0
        stale = sum(1 for a in db.open_audits() if a["ts"] < cutoff)
        verb = "removed" if args.apply else "would be removed"
        if dangling:
            print(f"audits whose player is gone: {len(dangling)} {verb} "
                  f"({', '.join(a['read_as'] for a in dangling[:6])})")
        got, orphans = prune_audit_rows(policy, apply=args.apply)
        if orphans:
            print(f"audit files with no row: {orphans} {verb}")
        if args.apply:
            print(f"audits: {got} pruned, {n - got} left")
        else:
            print(f"audits: {stale} of {n} older than "
                  f"{policy.audit_max_age_days:g}d would be pruned")
    except Exception as exc:
        print(f"audits: skipped ({type(exc).__name__}: {exc})")
    if not args.apply and any(r.deleted for r in results):
        print("(dry run - pass --apply to delete)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
