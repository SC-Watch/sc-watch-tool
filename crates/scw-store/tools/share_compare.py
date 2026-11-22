"""share.py against the port (examples/share_dump.rs) on a COPY of
sc-watch.db and its flags.json: the export built from it, share.json's
text, the export read back from its zip, and that export previewed and
imported into a fresh empty database - result, sources, every row, and the
reasons added. Prints counts only; deletes the copies and both outputs.

    python share_compare.py <work folder> <sc-watch.db> <flags.json>

Run with sc-watch's build environment, after `cargo build --release -p
scw-store --example share_dump`. The original database is opened read-only
and copied with SQLite's backup API; if it has no install id yet, both
copies are given the same made-up one.
"""
import json
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import types
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
WORK, SRC, FLAGS = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
sys.path.insert(0, str(ROOT))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import dbopen  # noqa: E402
import flags as flags_mod  # noqa: E402
import reputation  # noqa: E402
import share  # noqa: E402

EXE = ".exe" if sys.platform == "win32" else ""
RUST = ROOT / "target" / "release" / "examples" / f"share_dump{EXE}"
NOW = float(int(time.time()))
reputation.time = types.SimpleNamespace(time=lambda: NOW, localtime=time.localtime,
                                        strftime=time.strftime)
share.time = types.SimpleNamespace(time=lambda: NOW, strftime=time.strftime)


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
        out[t] = sorted([list(r) for r in db.execute(f'SELECT * FROM "{t}"')], key=json.dumps)
    db.close()
    return out


def main():
    WORK.mkdir(parents=True, exist_ok=True)
    a, b, ta, tb = WORK / "a.db", WORK / "b.db", WORK / "ta.db", WORK / "tb.db"
    fa, fb, tfa, tfb = WORK / "fa.json", WORK / "fb.json", WORK / "tfa.json", WORK / "tfb.json"
    out = WORK / "rs.json"
    made = [a, b, ta, tb, fa, fb, tfa, tfb, out]
    try:
        copy(SRC, a)
        copy(SRC, b)
        shutil.copyfile(FLAGS, fa)
        shutil.copyfile(FLAGS, fb)
        for p in (a, b):
            c = sqlite3.connect(p)
            c.execute("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT)")
            c.execute("INSERT OR IGNORE INTO meta (key, value) VALUES ('install', ?)", ("f" * 32,))
            c.commit()
            c.close()

        dbopen.forget()
        flags_mod.CONFIG_PATH = fa
        flags_mod.invalidate()
        db = reputation.Database(a)
        doc = share.build(db, "Compare")
        text = json.dumps(doc, indent=1)
        checked = share.unpack(share.pack(doc))
        db.close()
        flags_mod.CONFIG_PATH = tfa
        flags_mod.invalidate()
        target = reputation.Database(ta)
        py = {"doc": doc, "text": text, "checked": checked,
              "preview": share.preview(target, checked),
              "applied": share.apply(target, checked), "sources": share.sources(target)}
        target.close()
        py["rows"] = rows(ta)
        py["flags"] = json.loads(tfa.read_text(encoding="utf-8")) if tfa.exists() else None
        py = json.loads(json.dumps(py))

        subprocess.run([str(RUST), str(b), str(fb), str(tb), str(tfb), str(out), repr(NOW)],
                       check=True)
        rs = json.loads(out.read_text(encoding="utf-8"))
        problems = [k for k in py if py[k] != rs[k]]
        print(f"export of {len(doc['reports'])} reports, {len(doc['flags'])} reasons, "
              f"{len(doc['orgs'])} orgs, {len(doc['roster'])} hand-added members; "
              f"imported into an empty database: {py['applied']['reports_added']} reports, "
              f"{len(py['applied']['orgs_flagged'])} orgs, {py['applied']['roster_added']} members, "
              f"{len(py['applied']['reasons_added'])} reasons")
        print("IDENTICAL" if not problems else "DIFFERENT: " + ", ".join(problems))
        return 0 if not problems else 1
    finally:
        dbopen.forget()
        for p in made + [Path(str(p) + s) for p in (a, b, ta, tb) for s in ("-wal", "-shm")]:
            try:
                p.unlink()
            except OSError:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
