"""reputation.Database's own behaviour on a script of operations, for
tests/reputation_ops.rs to replay: after every operation, its return value
and every table of the database (and which audit image files remain).

    python make_reputation_ops.py      (sc-watch's build environment)

Deterministic: each operation runs at its own fixed time (time.time()
patched), "is this a real account" is a scripted answer per handle, and
local time is UTC so dates read the same on any machine. Made-up handles
and orgs only (EXMPL...). Writes reputation_ops.json.
"""
import json
import sqlite3
import sys
import tempfile
import time
from dataclasses import asdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())      # never the real data folder
import dbopen  # noqa: E402
import flags as flags_mod  # noqa: E402
import reputation  # noqa: E402

T0 = 1_790_000_000.0                       # a fixed "now" (2026-09)
DAY = 86400.0
NOW = [T0]
reputation.time.time = lambda: NOW[0]
reputation.time.localtime = time.gmtime    # dates in UTC, the same everywhere

# Which handles RSI says are real (True), are not (False), or cannot tell (None).
REAL = {"EXMPLONAME": True, "EXMPL0NAME": False, "EXMPLHANDLE": True, "EXMPLHANDLEX": False,
        "EXMPLTWO": True, "EXMPLTW0": True, "EXMPLBIG": None, "EXMPLB1G": False,
        "EXMPL_IO": True, "EXMPLIO": False, "EXMPLLONGNAME": False, "EXMPLLONG": False,
        "EXMPLSOLO": True}


def verify(handle, strict=False):
    v = REAL.get(handle.upper())
    return v if strict else bool(v)


class Org:
    def __init__(self, sid, is_main=False):
        self.sid, self.is_main = sid, is_main


def member(h, rank="Member", stars=1, display=None):
    return {"handle": h, "display": display or h.title(), "rank": rank, "stars": stars}


ROSTER1 = [member("EXMPLHANDLE"), member("EXMPLR1", "Officer", 3), member("EXMPLR2"),
           member("REDACTED"), {"handle": ""}, member("EXMPLR3", stars=0)]
ROSTER2 = [member("EXMPLHANDLE"), member("EXMPLR2", "Leader", 5), member("EXMPLR4")]

# (days after T0, method, args, kwargs)
OPS = [
    (0.0, "stats", [], {}),
    (0.1, "record_sighting", ["EXMPL0NAME", 2.4, 1, 1], {}),
    (0.2, "add_audit", ["exmpl0name", 2.4, 1, 1, [["EXMPL0NAME", 1]], "audit/f1.jpg", "audit/n1.png",
                        "audit/r1.png"], {}),
    (0.3, "add_audit", ["EXMPL0NAME", 2.5, 3, 3, [], "", "audit/n2.png", ""], {"reason": "no_rsi_account"}),
    (0.31, "add_audit", ["EXMPL0NAME", 2.5, 3, 3, [], "", "", ""], {"reason": "no_rsi_account"}),
    (0.4, "audits_for", ["exmpl0name"], {}),
    (0.5, "record_sighting", ["EXMPLONAME", 2.6, 2, 2], {}),     # the real account reads alike: a merge
    (0.6, "record_sighting", ["EXMPLHANDLE", 4.0, 3, 3], {}),
    (0.7, "record_sighting", ["EXMPLHANDLEX", 4.0, 1, 1], {}),   # a misread with a letter too many
    (0.8, "record_sighting", ["EXMPLTWO", 1.0, 2, 2], {}),
    (0.9, "record_sighting", ["EXMPLTW0", 1.0, 2, 2], {}),       # both real: kept apart
    (1.0, "record_sighting", ["EXMPLBIG", None, 1, 1], {}),
    (1.1, "record_sighting", ["EXMPLB1G", None, 1, 1], {}),      # one cannot be told: separate
    (1.2, "record_sighting", ["EXMPLLONG", 9.0, 1, 1], {}),
    (1.3, "record_sighting", ["EXMPLLONGNAME", 9.0, 1, 1], {}),  # both misreads, the longer extends
    (1.4, "record_sighting", ["EXMPLSOLO", 0.5, 4, 4], {}),
    (1.5, "canonical", ["exmpl0name"], {"mutate": False}),
    (1.6, "ensure_player", ["EXMPLNEVER"], {}),
    (1.61, "ensure_player", ["EXMPLNEVER"], {}),
    (1.7, "update_profile", ["exmplhandle", "Handle Moniker", "2014-05-01"], {}),
    (1.8, "add_report", ["EXMPLHANDLE", "camping", "spawn camper"], {}),
    (1.9, "add_report", ["EXMPLHANDLE", "pending"], {}),
    (2.0, "add_report", ["EXMPLHANDLE", "pending", "", "clip.mp4"], {}),
    (2.1, "pending_reports", [], {}),
    (2.2, "resolve_pending", ["EXMPLHANDLE", "unprovoked_attack", "two of them"], {}),
    (2.3, "add_report", ["exmplstranger", "harassment", "told by a friend"], {"exact": True}),
    (2.4, "add_report", ["EXMPLSOLO", "friendly", "helped me"], {}),
    (2.5, "add_report", ["EXMPLSOLO", "note"], {"source": "EXMPL_FRIEND"}),
    (2.6, "edit_report", [1], {"note": "spawn camper, again"}),
    (2.61, "edit_report", [99], {"note": "nobody"}),
    (2.62, "edit_report", [1], {}),
    (2.7, "delete_report", [5], {}),
    (2.71, "delete_report", [5], {}),
    (2.8, "add_ignored", ["Constructionsalvage", "the crate"], {}),
    (2.9, "is_ignored", ["CONSTRUCTI0NSALVAGE"], {}),
    (2.91, "is_ignored", ["constructionsalvage"], {}),
    (2.92, "is_ignored", ["EXMPLHANDLE"], {}),
    (3.0, "ignored_list", [], {}),
    (3.1, "flag_org", ["exmplorg", "pirates"], {"name": "Example Org"}),
    (3.2, "flag_org", ["EXMPLTWOORG"], {}),
    (3.3, "unnamed_flagged", [], {}),
    (3.4, "set_flag_name", ["exmpltwoorg", "  Second Org  "], {}),
    (3.41, "set_flag_name", ["EXMPLORG", "Renamed"], {}),
    (3.5, "record_roster", ["EXMPLORG", ROSTER1], {}),
    (3.6, "members_of", ["EXMPLORG"], {}),
    (13.6, "record_roster", ["EXMPLORG", ROSTER2], {}),                     # R1 and R3 leave, R4 joins
    (14.6, "record_roster", ["EXMPLORG", [member("EXMPLR1")]], {"complete": False}),
    (15.6, "record_roster", ["EXMPLORG", ROSTER1], {"source": "sentry"}),    # another service
    (16.6, "record_roster", ["EXMPLORG", ROSTER2], {"source": "sentry"}),
    (16.7, "add_member_manually", ["exmplorg", "EXMPLSOLO", "  seen with them  "], {}),
    (16.71, "add_member_manually", ["EXMPLORG", "EXMPLR1", "back on it"], {}),
    (16.72, "add_member_manually", ["EXMPLNOTFLAGGED", "EXMPLSOLO"], {}),
    (16.8, "record_roster", ["EXMPLORG", ROSTER2], {"source": "sentry"}),    # manual ones stay
    (16.9, "members_of", ["EXMPLORG"], {}),
    (17.0, "set_orgs", ["EXMPLTWO", [Org("exmpltwoorg", True), Org("EXMPLOTHER")]], {}),
    (17.1, "flagged_orgs", [], {}),
    (17.2, "set_org_manual", ["exmplother", "Other Org", "Piracy", ""], {}),
    (17.3, "org_manual", [], {}),
    (17.4, "set_manual_enlisted", ["exmplsolo", "2020-01-02"], {}),
    (17.5, "manual_enlisted", [["EXMPLSOLO", "exmplhandle", "EXMPLNOBODY"]], {}),
    (17.6, "is_listed", ["exmplhandle"], {}),
    (17.61, "is_listed", ["EXMPLR2"], {}),
    (17.62, "is_listed", ["EXMPLNOBODY"], {}),
    (17.63, "is_listed", ["  "], {}),
    (18.0, "assess", ["EXMPLHANDLE"], {}),
    (18.1, "assess", ["EXMPLSOLO"], {}),
    (18.2, "assess", ["EXMPLR1"], {}),
    (18.3, "assess", ["EXMPLR3"], {}),
    (18.4, "assess", ["exmplstranger"], {"exact": True}),
    (18.5, "assess", ["EXMPLTWO"], {}),
    (18.6, "assess", ["EXMPL0NAME"], {}),
    (200.0, "assess", ["EXMPLHANDLE"], {}),
    (800.0, "assess", ["EXMPLHANDLE"], {}),
    (800.1, "add_audit", ["EXMPLTWO", 1.0, 1, 1, [["EXMPLTWO", 1], ["EXMPLTW0", 1]], "audit/f3.jpg",
                          "audit/n3.png", "audit/r3.png"], {}),
    (800.2, "confirm_audit", [3, "EXMPLTW0"], {}),                         # both real: asks first
    (800.3, "confirm_audit", [3, "EXMPLTW0"], {"force": True}),
    (800.4, "add_audit", ["EXMPLGONE", None, 1, 1, [], "", "", ""], {}),
    (800.5, "confirm_audit", [4, "constructionsalvage"], {}),               # an ignored label
    (800.6, "confirm_audit", [4, "EXMPLADOPTED"], {}),                      # no record: adopted
    (800.7, "confirm_audit", [99, "X"], {}),
    (800.8, "add_audit", ["EXMPLSOLO", 0.5, 1, 1, [], "audit/f4.jpg", "", ""], {}),
    (800.9, "dismiss_audit", [5], {}),
    (800.91, "dismiss_audit", [5], {}),
    (801.0, "add_audit", ["EXMPLNOTHERE", 1.0, 1, 1, [], "audit/f5.jpg", "", ""], {}),
    (801.1, "orphan_audits", [], {}),
    (801.2, "audit_files_in_use", [], {}),
    (801.3, "prune_orphan_audit_files", [], {}),
    (801.4, "orphan_audits", [], {"apply": True}),
    (801.5, "prune_orphan_audit_files", [], {"apply": True}),
    (801.6, "add_audit", ["EXMPLSOLO", 0.5, 1, 1, [], "", "", ""], {}),
    (820.0, "prune_audits", [], {}),
    (820.1, "open_audits", [], {}),
    (820.2, "forget", ["exmplnever"], {}),
    (820.3, "forget", ["EXMPLNOBODY"], {}),
    (820.4, "record_sighting", ["EXMPLIO", 1.0, 1, 1], {}),
    (820.5, "record_sighting", ["EXMPLHANDLEX", 1.0, 1, 1], {}),
    (820.6, "consolidate", [], {}),
    (820.7, "remove_manual_member", ["EXMPLORG", "EXMPLSOLO"], {}),
    (820.71, "remove_manual_member", ["EXMPLORG", "EXMPLR1"], {}),
    (820.72, "remove_manual_member", ["EXMPLORG", "EXMPLR2"], {}),
    (820.8, "set_org_manual", ["EXMPLOTHER"], {}),
    (820.9, "set_manual_enlisted", ["EXMPLSOLO"], {}),
    (821.0, "remove_ignored", ["CONSTRUCTIONSALVAGE"], {}),
    (821.1, "remove_ignored", ["nothing"], {}),
    (821.2, "unflag_org", ["exmplorg"], {}),
    (821.3, "recent", [3], {}),
    (821.4, "stats", [], {}),
]
AUDIT_FILES = ["f1.jpg", "n1.png", "r1.png", "n2.png", "f3.jpg", "n3.png", "r3.png", "f4.jpg", "f5.jpg",
               "stray.png"]


def to_json(x):
    if isinstance(x, reputation.Assessment):
        d = asdict(x)
        d["flagged_orgs"] = [list(t) for t in x.flagged_orgs]
        d["former_orgs"] = [list(t) for t in x.former_orgs]
        d.update(score=x.score, alert=x.alert, spoken_all=x.spoken_all(), lines=x.lines())
        return d
    if isinstance(x, sqlite3.Row):
        return dict(x)
    if isinstance(x, Org):
        return {"sid": x.sid, "is_main": x.is_main}
    if isinstance(x, (list, tuple)):
        return [to_json(v) for v in x]
    if isinstance(x, set):
        return sorted(x)
    if isinstance(x, dict):
        return {k: to_json(v) for k, v in x.items()}
    return x


def dump(path: Path, root: Path):
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    out = {}
    for (t,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"):
        cols = [r[1] for r in db.execute(f"PRAGMA table_info({t})")]
        names = ", ".join(f'"{c}"' for c in cols)
        out[t] = [list(r) for r in db.execute(f'SELECT {names} FROM "{t}" ORDER BY {names}')]
    db.close()
    out["audit files"] = sorted(p.name for p in (root / "audit").glob("*"))
    return out


root = Path(tempfile.mkdtemp())
(root / "audit").mkdir()
for f in AUDIT_FILES:
    (root / "audit" / f).write_bytes(b"x")
dbopen.forget()
flags_path = root / "flags.json"            # the defaults: no file
flags_mod.CONFIG_PATH = flags_path
db = reputation.Database(root / "sc-watch.db", verify=verify)
steps = []
for days, name, args, kwargs in OPS:
    NOW[0] = T0 + days * DAY
    result = getattr(db, name)(*args, **kwargs)
    if name == "stats":
        result.pop("bytes")                  # page use differs between SQLite builds
    steps.append({"at": NOW[0], "op": name, "args": to_json(args), "kwargs": to_json(kwargs),
                  "result": to_json(result), "dump": dump(root / "sc-watch.db", root)})
db.close()
(HERE / "reputation_ops.json").write_text(json.dumps({"real": REAL, "audit_files": AUDIT_FILES, "steps": steps},
                                                     indent=0), encoding="utf-8", newline="\n")
print(len(steps), "operations")
