"""reputation.py's migration and the port's on the same starting databases,
compared by full dumps (tools/dbdump.py). Each database is COPIED with
SQLite's backup API into the work folder; the originals are opened
read-only, and the copies are deleted at the end. Prints statistics only.

    python migrate_compare.py <work folder> [sc-watch.db ...]

Run with sc-watch's build environment, from anywhere; it finds the
repository and the port's `migrate` example (cargo build -p scw-store
--example migrate) itself. With no databases, only a fresh one is compared.
"""
import json
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
WORK = Path(sys.argv[1])
SOURCES = [Path(p) for p in sys.argv[2:]]
sys.path.insert(0, str(ROOT))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import dbopen  # noqa: E402
import reputation  # noqa: E402

EXE = ".exe" if sys.platform == "win32" else ""
RUST = ROOT / "target" / "debug" / "examples" / f"migrate{EXE}"
DUMP = Path(__file__).resolve().parent / "dbdump.py"


def copy(src: Path, dst: Path):
    s = sqlite3.connect(f"file:{src}?mode=ro", uri=True)
    d = sqlite3.connect(dst)
    s.backup(d)
    d.close()
    s.close()


def dump(p: Path):
    return json.loads(subprocess.run([sys.executable, "-I", str(DUMP), str(p)], capture_output=True,
                                     text=True, check=True).stdout)


WORK.mkdir(parents=True, exist_ok=True)
ok = True
for i, src in enumerate([None] + SOURCES):
    name = "fresh" if src is None else f"database {i}"
    a, b = WORK / f"case{i}_py.db", WORK / f"case{i}_rs.db"
    if src is not None:
        copy(src, a)
        copy(src, b)
    before = dump(a) if src is not None else None
    dbopen.forget()
    reputation.Database(a).close()
    subprocess.run([str(RUST), str(b)], capture_output=True, check=True)
    da, dr = dump(a), dump(b)
    differ = [t for t in da["tables"] if da["tables"][t] != dr["tables"].get(t)]
    same = da["schema"] == dr["schema"] and not differ and set(da["tables"]) == set(dr["tables"])
    ok &= same
    rows = sum(len(t["rows"]) for t in da["tables"].values())
    changed = "" if before is None else \
        f", the migration changed {sum(1 for t in da['tables'] if da['tables'][t] != before['tables'].get(t))} tables"
    print(f"{name}: {len(da['schema'])} schema entries, {len(da['tables'])} tables, {rows} rows{changed}: "
          f"{'IDENTICAL' if same else 'DIFFERENT - tables ' + ', '.join(differ)}")
for p in WORK.glob("case*.db*"):
    p.unlink()
sys.exit(0 if ok else 1)
