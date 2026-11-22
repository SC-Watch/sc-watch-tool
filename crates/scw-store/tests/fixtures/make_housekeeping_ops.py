"""housekeeping.py's own behaviour, for tests/housekeeping_ops.rs.

A made-up data folder of frames - named, sized and dated here, so the Rust
test can make the same one - swept under several policies, dry and for
real; Policy.load() on legacy files and settings; summarise() on chosen
totals; and the audit pruning on a made-up sc-watch.db, with its images.
After each step: the result, and every file left.

Every frame time is distinct: two frames of the same age are ordered by the
directory listing, which differs between Windows and Linux. The scenario
that needs ties, and Windows' case-blind *.png, is marked windows_only.

    python make_housekeeping_ops.py      (sc-watch's build environment)

Writes housekeeping_ops.json.
"""
import json
import os
import sys
import tempfile
import time
import types
from dataclasses import asdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

ROOT = Path(tempfile.mkdtemp())
paths.DATA_ROOT = ROOT                    # a made-up data folder, never the real one
import dbopen  # noqa: E402
import housekeeping  # noqa: E402
import reputation  # noqa: E402
import settings as settings_mod  # noqa: E402

NOW = [1_790_000_000.0]
housekeeping.time = types.SimpleNamespace(time=lambda: NOW[0])
reputation.time = types.SimpleNamespace(time=lambda: NOW[0], localtime=time.gmtime,
                                        strftime=time.strftime)

H = 3600.0
# (path under the data folder, bytes, hours old)
TREE = [
    *[(f"live_frames/f{i:02d}.png", 1000 + i, 1.0 + i * 3.1) for i in range(12)],
    *[(f"live_frames/g{i:02d}.jpg", 250_000 * (i + 1), 2.0 + i * 7.3) for i in range(5)],
    ("live_frames/notes.txt", 10, 500.0),
    ("live_frames/.png", 7, 400.0),
    ("debug_bursts/b1.png", 1_500_000, 30.0),
    ("debug_bursts/b2.png", 1_000_000, 50.5),
    ("debug_bursts/sentry_requests.log", 99, 900.0),
    ("detections/keep.json", 5, 900.0),
    ("frames/reference.png", 11_000_000, 2000.0),      # not managed: never touched
    ("mine/old.jpg", 42, 100.0),
    ("mine/new.jpg", 43, 0.5),
]
# A folder that looks like a frame is in the Windows scenario: a folder's
# size is 0 there and 4096 on Linux, and a dry run counts it as freed.
DIRS = []


def make(root, tree, dirs):
    for rel in dirs:
        (root / rel).mkdir(parents=True, exist_ok=True)
        os.utime(root / rel, (NOW[0] - 999 * H, NOW[0] - 999 * H))
    for rel, size, hours in tree:
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(b"\0" * size)
        t = NOW[0] - hours * H
        os.utime(p, (t, t))


def files(root):
    out = []
    for p in sorted(root.rglob("*")):
        if p.is_file() and p.parent.name != "__pycache__":
            out.append(p.relative_to(root).as_posix())
    return out


def pol(**kw):
    p = housekeeping.Policy()
    for k, v in kw.items():
        setattr(p, k, v)
    return p


make(ROOT, TREE, DIRS)
POLICIES = [
    ("defaults", {}),
    ("keep two", {"keep_newest": 2}),
    ("cap three", {"keep_newest": 1, "max_files_per_dir": 3, "max_age_hours": 1000.0}),
    ("disabled", {"enabled": False, "keep_newest": 0}),
    ("keep all", {"keep_newest": 99, "max_age_hours": 0.0}),
    ("negative keep", {"keep_newest": -3, "max_age_hours": 0.0}),
    ("custom dir", {"keep_newest": 0, "directories": ["mine", "nowhere", "live_frames"]}),
]
sweeps = []
for label, kw in POLICIES:
    p = pol(**kw)
    res = housekeeping.sweep(p, apply=False)
    sweeps.append({"label": label, "policy": asdict(p), "apply": False,
                   "result": [asdict(r) for r in res],
                   "summary": housekeeping.summarise(res, False), "files": files(ROOT)})
# For real, then again: the second finds what the first left.
for label, kw in (("keep two, applied", {"keep_newest": 2}),
                  ("cap three, applied", {"keep_newest": 1, "max_files_per_dir": 3,
                                          "max_age_hours": 1000.0})):
    p = pol(**kw)
    res = housekeeping.sweep(p, apply=True)
    sweeps.append({"label": label, "policy": asdict(p), "apply": True,
                   "result": [asdict(r) for r in res],
                   "summary": housekeeping.summarise(res, True), "files": files(ROOT)})

# ---- the tie and case scenario (Windows) ------------------------------------------------
CASE = Path(tempfile.mkdtemp())
CASE_TREE = [("live_frames/a.png", 1, 30.0), ("live_frames/b.jpg", 2, 30.0),
             ("live_frames/C.PNG", 3, 30.0), ("live_frames/d.Jpg", 4, 30.0),
             ("live_frames/e.png", 5, 40.0)]
CASE_DIRS = ["live_frames/dir.png"]
make(CASE, CASE_TREE, CASE_DIRS)
case_dry = housekeeping.sweep(pol(keep_newest=2), root=CASE, apply=False)
case_res = housekeeping.sweep(pol(keep_newest=2), root=CASE, apply=True)
case = {"tree": CASE_TREE, "dirs": CASE_DIRS, "dry": [asdict(r) for r in case_dry],
        "result": [asdict(r) for r in case_res], "files": files(CASE)}

# ---- Policy.load ------------------------------------------------------------------
FULL = {"housekeeping_enabled": True, "max_age_hours": 48.0, "keep_newest": 5,
        "max_files_per_dir": 100, "audit_max_age_days": 3.0}
LOADS = [
    ("nothing", None, None),
    ("settings only", None, FULL),
    ("legacy only", '{"enabled": false, "max_age_hours": 12, "keep_newest": 7.0,'
                    ' "directories": ["mine", 3, null, "live_frames"]}', None),
    ("legacy bom", "﻿" + '{"max_files_per_dir": 9, "unknown": 1}', None),
    ("legacy wrong types", '{"enabled": 1, "max_age_hours": "x", "keep_newest": 2.5,'
                           ' "directories": "mine", "audit_max_age_days": true}', None),
    ("legacy not a dict", "[1, 2]", None),
    ("legacy broken", "{nope", None),
    ("both", '{"directories": ["mine"], "keep_newest": 1}', FULL),
    ("settings as text", None, {"housekeeping_enabled": 0, "max_age_hours": "36",
                                "keep_newest": "4", "max_files_per_dir": 2.9,
                                "audit_max_age_days": " 2.5 "}),
    ("settings bool numbers", None, {"housekeeping_enabled": "", "max_age_hours": True,
                                     "keep_newest": True, "max_files_per_dir": False,
                                     "audit_max_age_days": 1}),
    ("settings stops at a bad value", '{"keep_newest": 8}',
     {"housekeeping_enabled": False, "max_age_hours": "abc", "keep_newest": 3,
      "max_files_per_dir": 1, "audit_max_age_days": 1.0}),
    ("settings missing a key", None, {"housekeeping_enabled": False, "max_age_hours": 5}),
    ("settings int from float text", None, dict(FULL, keep_newest="3.0")),
]
loads = []
real_load = settings_mod.load
for label, legacy, cur in LOADS:
    path = ROOT / "housekeeping.json"
    if legacy is None:
        path.unlink(missing_ok=True)
    else:
        path.write_text(legacy, encoding="utf-8")

    def fake(cur=cur):
        if cur is None:
            raise OSError("settings unreadable")
        return cur
    settings_mod.load = fake
    p = housekeeping.Policy.load(path)
    loads.append({"label": label, "legacy": legacy, "settings": cur, "policy": asdict(p)})
settings_mod.load = real_load
for p in ROOT.glob("housekeeping.unreadable-*"):
    p.unlink()
saved = pol(keep_newest=3, directories=["mine"])
saved.save(ROOT / "saved.json")
saved_json = json.loads((ROOT / "saved.json").read_text(encoding="utf-8"))

SUMMARIES = [
    ([], False),
    ([("a", 0, 0, 0, 0)], True),
    ([("a", 3, 2, 500_000, 1), ("b", 1, 1, 1_500_000, 0), ("c", 0, 0, 0, 0)], False),
    ([("a", 9, 4, 2_500_000, 5), ("b", 2, 2, 3_500_000, 0)], True),
    ([("x", 1, 1, 999_999_999_999, 0)], True),
]
summaries = [{"results": r, "applied": a, "line": housekeeping.summarise(
    [housekeeping.Sweep(*t) for t in r], a)} for r, a in SUMMARIES]

# ---- audits ------------------------------------------------------------------------
dbopen.forget()
db = reputation.Database()
audit_dir = ROOT / "audit"
audit_dir.mkdir(exist_ok=True)
AUDITS = [("EXMPLOLD", 10 * 86400.0, "old.png"), ("EXMPLNEW", 1 * 86400.0, "new.png"),
          ("EXMPLGONE", 2 * 86400.0, "gone.png")]
t_now = NOW[0]
for h, age, crop in AUDITS:
    NOW[0] = t_now - age
    (audit_dir / crop).write_bytes(b"png")
    db.record_sighting(h, 2.0, 1, 3)
    db.add_audit(h, 2.0, 1, 3, [h + "X"], "", f"audit/{crop}", "")   # as the watcher writes it
NOW[0] = t_now
db.db.execute("DELETE FROM players WHERE handle='EXMPLGONE'")
db.db.commit()
(audit_dir / "stray.png").write_bytes(b"png")
db.close()
audit_steps = []
for apply in (False, True, True):
    dbopen.forget()
    got = housekeeping.prune_audit_rows(pol(), apply=apply)
    db = reputation.Database()
    rows = [dict(r) for r in db.db.execute("SELECT handle, crop_name FROM audits ORDER BY handle")]
    db.close()
    audit_steps.append({"apply": apply, "result": list(got), "rows": rows,
                        "files": sorted(p.name for p in audit_dir.iterdir())})
run_lines = [housekeeping.run_now(apply=False, policy=pol(keep_newest=0)),
             housekeeping.run_now(apply=True, policy=pol(keep_newest=0))]

(HERE / "housekeeping_ops.json").write_text(json.dumps({
    "now": NOW[0], "tree": TREE, "dirs": DIRS, "managed": list(housekeeping.MANAGED),
    "sweeps": sweeps, "case": case, "loads": loads, "saved": saved_json,
    "summaries": summaries, "audits": AUDITS, "audit_steps": audit_steps,
    "run_now": run_lines}, indent=0), encoding="utf-8", newline="\n")
print(len(sweeps), "sweeps,", len(loads), "loads,", len(summaries), "summaries,",
      len(audit_steps), "audit prunes")
