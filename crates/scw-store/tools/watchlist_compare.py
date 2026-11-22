"""watchlist.py's answers against the port's (examples/watchlist_dump.rs)
on a COPY of a watchlist.db: lookup() for every entry's handle and every
player's handle, exact and with variants; sources, orgs, stats, reason
coverage; and seeding a fresh database from a starter list, compared by
its rows. Prints counts only, and deletes the copies and both outputs.

    python watchlist_compare.py <work folder> <watchlist.db> <sc-watch.db> <starter_list.json>

Run with sc-watch's build environment, after `cargo build --release -p
scw-store --example watchlist_dump`. Copies are made with SQLite's backup
API; the originals are opened read-only.
"""
import json
import sqlite3
import subprocess
import sys
import tempfile
from dataclasses import asdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
WORK, WL, REP, STARTER = (Path(p) for p in sys.argv[1:5])
sys.path.insert(0, str(ROOT))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import dbopen  # noqa: E402
import watchlist  # noqa: E402

EXE = ".exe" if sys.platform == "win32" else ""
RUST = ROOT / "target" / "release" / "examples" / f"watchlist_dump{EXE}"


def copy(src, dst):
    s = sqlite3.connect(f"file:{src}?mode=ro", uri=True)
    d = sqlite3.connect(dst)
    s.backup(d)
    d.close()
    s.close()


def rows(path):
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    out = {}
    for (t,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"):
        cols = [r[1] for r in db.execute(f"PRAGMA table_info({t})")]
        names = ", ".join(f'"{c}"' for c in cols)
        # Times differ between the two runs; everything else must not.
        keep = [i for i, c in enumerate(cols) if not c.endswith("_ts")]
        out[t] = sorted([[r[i] for i in keep] for r in db.execute(f'SELECT {names} FROM "{t}"')], key=repr)
    db.close()
    return out


WORK.mkdir(parents=True, exist_ok=True)
a, b = WORK / "wl_py.db", WORK / "wl_rs.db"
fa, fb = WORK / "fresh_py.db", WORK / "fresh_rs.db"
hfile = WORK / "handles.txt"
try:
    copy(WL, a)
    copy(WL, b)
    con = sqlite3.connect(f"file:{REP}?mode=ro", uri=True)
    players = [r[0] for r in con.execute("SELECT handle FROM players")]
    con.close()
    con = sqlite3.connect(a)
    entries = [r[0] for r in con.execute("SELECT handle FROM entries")]
    con.close()
    handles = sorted(set(players) | set(entries))
    hfile.write_text("\n".join(handles), encoding="utf-8")
    name = str((watchlist.read_starter(STARTER) or {}).get("name") or "")
    watchlist._starter_name = name
    dbopen.forget()
    wl = watchlist.Watchlist(a)
    js = lambda hs: [dict(asdict(x), describe=x.describe()) for x in hs]
    stats = wl.stats()
    stats.pop("path")
    py = {"lookups": {h: [js(wl.lookup(h, variants=False)), js(wl.lookup(h, variants=True))] for h in handles},
          "sources": wl.sources(), "orgs": wl.orgs_flagged(), "stats": stats, "coverage": list(wl.reason_coverage())}
    wl.close()
    py["seeded"] = watchlist.seed_starter(STARTER, fa)
    py = json.loads(json.dumps(py))
    rs = json.loads(subprocess.run([str(RUST), str(b), str(hfile), str(STARTER), str(fb), name], capture_output=True,
                                   text=True, encoding="utf-8", check=True).stdout)
    differ = {k: 1 for k in ("sources", "orgs", "stats", "coverage", "seeded") if py[k] != rs[k]}
    bad_lookups = sum(1 for h in handles if py["lookups"][h] != rs["lookups"].get(h))
    if bad_lookups:
        differ["lookups"] = bad_lookups
    if rows(fa) != rows(fb):
        differ["seeded rows"] = 1
    hits = sum(1 for h in handles if py["lookups"][h][0])
    variants = sum(1 for h in handles if len(py["lookups"][h][1]) > len(py["lookups"][h][0]))
    print(f"{len(handles)} handles looked up ({hits} with exact hits, {variants} with variant hits), "
          f"{len(py['sources'])} sources, starter seeding {py['seeded']['seeded']} entries: "
          f"{'IDENTICAL' if not differ else 'DIFFERENT: ' + json.dumps(differ)}")
    sys.exit(0 if not differ else 1)
finally:
    for p in list(WORK.glob("wl_*.db*")) + list(WORK.glob("fresh_*.db*")) + [hfile]:
        p.unlink(missing_ok=True)
