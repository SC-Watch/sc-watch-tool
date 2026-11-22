"""housekeeping.py against the port (examples/housekeeping_dump.rs): a DRY
RUN over a real data folder - the policy loaded from its settings (a copy),
what the sweep would delete in each managed directory, the line it would
print, and how many audits and orphan images the pruning would remove,
counted against a COPY of sc-watch.db. Nothing is deleted on either side.
Prints counts only.

    python housekeeping_compare.py <work folder> <data folder>

Run with sc-watch's build environment, after `cargo build --release -p
scw-store --example housekeeping_dump`.
"""
import json
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import types
from dataclasses import asdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
WORK, DATA = Path(sys.argv[1]), Path(sys.argv[2])
sys.path.insert(0, str(ROOT))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import dbopen  # noqa: E402
import housekeeping  # noqa: E402
import reputation  # noqa: E402
import settings as settings_mod  # noqa: E402

EXE = ".exe" if sys.platform == "win32" else ""
RUST = ROOT / "target" / "release" / "examples" / f"housekeeping_dump{EXE}"
NOW = float(int(time.time()))
housekeeping.time = types.SimpleNamespace(time=lambda: NOW)
reputation.time = types.SimpleNamespace(time=lambda: NOW, localtime=time.localtime,
                                        strftime=time.strftime)


def main():
    WORK.mkdir(parents=True, exist_ok=True)
    sets, db_copy, cur_json, out = (WORK / "settings.json", WORK / "a.db",
                                    WORK / "cur.json", WORK / "rs.json")
    db_rs = WORK / "b.db"
    legacy = WORK / "housekeeping.json"
    if (DATA / "housekeeping.json").exists():
        shutil.copyfile(DATA / "housekeeping.json", legacy)
    made = [sets, db_copy, db_rs, cur_json, out, legacy]
    try:
        shutil.copyfile(DATA / "settings.json", sets)
        for dst in (db_copy, db_rs):
            s = sqlite3.connect(f"file:{DATA / 'sc-watch.db'}?mode=ro", uri=True)
            d = sqlite3.connect(dst)
            s.backup(d)
            d.close()
            s.close()
        cur = settings_mod.load(sets)
        cur_json.write_text(json.dumps(cur), encoding="utf-8")
        settings_mod.load = lambda *a, **k: cur
        policy = housekeeping.Policy.load(legacy)
        swept = housekeeping.sweep(policy, root=DATA, apply=False)
        dbopen.forget()
        db = reputation.Database(db_copy)
        db.root = DATA                    # its audit/ listed, never touched: apply=False
        reputation.Database = lambda *a, **k: db
        audits = housekeeping.prune_audit_rows(policy, apply=False)
        # run_now() sweeps paths.DATA_ROOT: the data folder for this one dry
        # run, then the scratch folder again.
        scratch, paths.DATA_ROOT = paths.DATA_ROOT, DATA
        try:
            line = housekeeping.run_now(apply=False, policy=policy)
        finally:
            paths.DATA_ROOT = scratch
        py = {"policy": asdict(policy), "sweep": [asdict(r) for r in swept],
              "summary": housekeeping.summarise(swept, False), "audits": list(audits),
              "run_now": line}
        db.close()

        subprocess.run([str(RUST), str(DATA), str(legacy), str(cur_json), str(db_rs), str(out),
                        repr(NOW)], check=True)
        rs = json.loads(out.read_text(encoding="utf-8"))
        problems = [k for k in py if py[k] != rs[k]]
        print("; ".join(f"{r.directory}: {r.examined} frames, {r.deleted} due"
                        for r in swept) + f"; audits due {audits[0]}, orphan images {audits[1]}")
        print("IDENTICAL" if not problems else "DIFFERENT: " + ", ".join(problems))
        if "run_now" in problems:          # counts and folder names only
            print("  python:", py["run_now"])
            print("  rust:  ", rs["run_now"])
        return 0 if not problems else 1
    finally:
        dbopen.forget()
        for p in made + [Path(str(p) + s) for p in (db_copy, db_rs) for s in ("-wal", "-shm")]:
            try:
                p.unlink()
            except OSError:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
