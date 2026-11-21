"""Every read reputation.Database answers - assess, canonical (read-only,
on misread-style variants of each handle too), is_listed, audits_for,
members_of, flagged_orgs, pending reports, stats, recent, open audits -
for every player of a COPY of a sc-watch.db, the Python's against the
port's (examples/read_dump.rs), compared exactly. Prints counts only, and
deletes the copy and both outputs.

    python read_compare.py <work folder> <sc-watch.db> <flags.json>

Run with sc-watch's build environment, after `cargo build --release -p
scw-store --example read_dump`. The database is copied with SQLite's backup
API; the original is opened read-only.
"""
import json
import sqlite3
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
WORK, SRC, FLAGS = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
sys.path.insert(0, str(ROOT))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import dbopen  # noqa: E402
import flags as flags_mod  # noqa: E402
import reputation  # noqa: E402

EXE = ".exe" if sys.platform == "win32" else ""
RUST = ROOT / "target" / "release" / "examples" / f"read_dump{EXE}"


def formula(handle, strict=False):
    v = {0: True, 1: False, 2: None}[sum(ord(c) for c in handle.upper()) % 3]
    return v if strict else bool(v)


def variants(h):
    v = []
    if len(h) > 4:
        v += [h[:-1], h[1:]]
    v += [h.replace("O", "0").replace("I", "1"), h.replace("0", "O").replace("S", "5")]
    return v


WORK.mkdir(parents=True, exist_ok=True)
copy = WORK / "copy.db"
s = sqlite3.connect(f"file:{SRC}?mode=ro", uri=True)
d = sqlite3.connect(copy)
s.backup(d)
d.close()
s.close()
try:
    con = sqlite3.connect(copy)
    now = (con.execute("SELECT MAX(last_seen) FROM players").fetchone()[0] or 0) + 86400.0
    con.close()
    reputation.time.time = lambda: now
    reputation.time.localtime = time.gmtime
    flags_mod.CONFIG_PATH = FLAGS
    dbopen.forget()
    db = reputation.Database(copy, verify=formula)

    def assess_json(a):
        x = asdict(a)
        x["flagged_orgs"] = [list(t) for t in a.flagged_orgs]
        x["former_orgs"] = [list(t) for t in a.former_orgs]
        x.update(score=a.score, alert=a.alert, spoken_all=a.spoken_all(), lines=a.lines())
        return x

    handles = [r[0] for r in db.db.execute("SELECT handle FROM players ORDER BY handle")]
    players = {h: {"assess": assess_json(db.assess(h)), "listed": db.is_listed(h), "audits": db.audits_for(h),
                   "canonical": [[v, *db.canonical(v, mutate=False)] for v in variants(h)]} for h in handles}
    sids = [r[0] for r in db.db.execute("SELECT sid FROM org_flags ORDER BY sid")]
    stats = db.stats()
    stats.pop("bytes")
    py = {"players": players, "flagged_orgs": db.flagged_orgs(), "members": {s: db.members_of(s) for s in sids},
          "pending": [list(p) for p in db.pending_reports()], "stats": stats,
          "recent": [dict(r) for r in db.recent(25)], "open_audits": db.open_audits(), "org_manual": db.org_manual(),
          "unnamed": db.unnamed_flagged()}
    db.close()
    py = json.loads(json.dumps(py))
    rs = json.loads(subprocess.run([str(RUST), str(copy), str(FLAGS), repr(now)], capture_output=True, text=True, encoding="utf-8",
                                   check=True).stdout)

    def close(a, b):
        if isinstance(a, float) and isinstance(b, (int, float)):
            return a == b or abs(a - b) <= 1e-12 * max(abs(a), abs(b))
        if isinstance(a, list) and isinstance(b, list):
            return len(a) == len(b) and all(close(x, y) for x, y in zip(a, b))
        if isinstance(a, dict) and isinstance(b, dict):
            return list(a) == list(b) and all(close(a[k], b[k]) for k in a)
        return a == b

    differ = {}
    for h in handles:
        for part in ("assess", "listed", "audits", "canonical"):
            if not close(py["players"][h][part], rs["players"].get(h, {}).get(part)):
                differ[part] = differ.get(part, 0) + 1
    for part in ("flagged_orgs", "members", "pending", "stats", "recent", "open_audits", "org_manual", "unnamed"):
        if not close(py[part], rs[part]):
            differ[part] = 1
    alerts = sum(1 for h in handles if py["players"][h]["assess"]["alert"])
    merges = sum(1 for h in handles for v, c, f in py["players"][h]["canonical"] if c != v.upper())
    print(f"{len(handles)} players ({alerts} alerting), {len(handles) * 4 - sum(1 for h in handles if len(h) <= 4) * 2}"
          f" canonical() checks ({merges} resolved to another spelling), {len(sids)} flagged orgs: "
          f"{'IDENTICAL' if not differ else 'DIFFERENT: ' + json.dumps(differ)}")
    sys.exit(0 if not differ else 1)
finally:
    for p in WORK.glob("copy.db*"):
        p.unlink()
