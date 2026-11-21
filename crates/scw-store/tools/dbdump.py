"""A SQLite file as canonical JSON - every schema entry with its stored SQL
text, every table's columns and rows - for comparing two databases exactly.
Read-only; opens the file with mode=ro.

    python dbdump.py <db> [--schema-only]

The output holds the database's contents: for a real database, keep it
where the database is, never in a repository.
"""
import json
import sqlite3
import sys

path = sys.argv[1]
schema_only = "--schema-only" in sys.argv
db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
out = {"schema": [list(r) for r in db.execute(
    "SELECT type, name, tbl_name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name")],
       "tables": {}}
for (t,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"):
    cols = [list(r) for r in db.execute(f"PRAGMA table_info({t})")]
    entry = {"columns": cols}
    if not schema_only:
        names = ", ".join(f'"{c[1]}"' for c in cols)
        rows = db.execute(f'SELECT {names} FROM "{t}" ORDER BY {names}').fetchall()
        entry["rows"] = [[v.hex() if isinstance(v, bytes) else v for v in r] for r in rows]
    out["tables"][t] = entry
print(json.dumps(out, sort_keys=False))
