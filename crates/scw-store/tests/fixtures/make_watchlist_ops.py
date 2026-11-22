"""watchlist.py's own behaviour on a script of operations, for
tests/watchlist_ops.rs: after each, the return value and every table.
Also parse_lines on pasted text, and reputation.Database.assess with a
watchlist attached (hits in its lines and alert).

    python make_watchlist_ops.py      (sc-watch's build environment)

Made-up handles, orgs, sources and starter lists only. Fixed times.
Writes watchlist_ops.json.
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
import reputation  # noqa: E402
import watchlist  # noqa: E402

T0 = 1_790_000_000.0
NOW = [T0]
watchlist.time.time = lambda: NOW[0]
reputation.time.time = lambda: NOW[0]
reputation.time.localtime = time.gmtime

root = Path(tempfile.mkdtemp())
STARTERS = {
    "starter_v1.json": {"name": "EXMPL bundled list", "version": 1, "note": "made up",
                        "entries": [{"handle": "EXMPLBAD", "claim": "ganked a hauler", "posted": "2026-01-02"},
                                    {"handle": "EXMPLBAD", "claim": "", "posted": "2026-02-03"},
                                    {"handle": "EXMPLHEDGED", "confidence": "unconfirmed"},
                                    {"handle": " exmplspaced ", "confidence": "weird"}, "not a dict",
                                    {"handle": ""}, {"handle": None, "claim": "x"}, {"handle": 7, "claim": 3.5}],
                        "orgs": [{"sid": "exmplorg", "claim": "griefers"}, {"claim": "no sid"}, "x"]},
    "starter_v2.json": {"name": "EXMPL bundled list", "version": "2",
                        "entries": [{"handle": "EXMPLBAD", "claim": "ganked a hauler", "posted": "2026-01-02"},
                                    {"handle": "EXMPLNEW", "claim": "new in v2"}]},
    "starter_bom.json": None, "starter_bad.json": None, "starter_noentries.json": None,
}
for name, doc in STARTERS.items():
    if doc is not None:
        (root / name).write_text(json.dumps(doc), encoding="utf-8")
(root / "starter_bom.json").write_bytes(b"\xef\xbb\xbf" + json.dumps(
    {"name": "EXMPL bom list", "entries": [{"handle": "EXMPLBOM"}]}).encode())
(root / "starter_bad.json").write_text("{ nope", encoding="utf-8")
(root / "starter_noentries.json").write_text(json.dumps({"name": "x", "entries": "no"}), encoding="utf-8")

PASTE = """# a comment
// another
1. EXMPLONE - camped the station
- EXMPLTWO: rammed us
* EXMPLTHREE | griefing,  repeatedly
EXMPLFOUR  two spaces then a reason
https://robertsspaceindustries.com/citizens/EXMPLFIVE  - killed on sight
https://robertsspaceindustries.com/en/citizens/EXMPL_SIX
see https://robertsspaceindustries.com/orgs/EXMPLORG for the org
https://robertsspaceindustries.com/orgs/EXMPLTWOORG
not a handle!!! - at all
x
EXMPLONE - said twice
• EXMPLBULLET – en dash
EXMPLTAB\t-\ttabbed
"""
PARSE = [PASTE, "", "   \n\n", "EXMPLA\r\nEXMPLB\rEXMPLC EXMPLD", "EXMPL " * 3, "a" * 41, "ab"]

OPS = [
    ("add_source", ["EXMPL source", "https://example.invalid/a", "first"], {}),
    ("add_source", ["EXMPL source", "https://example.invalid/b", "second"], {}),
    ("add_entry", [1, "EXMPLBAD"], {"claim": "rammed", "msg_id": "m1", "posted_ts": "2026-03-04"}),
    ("add_entry", [1, "EXMPLBAD"], {"claim": "again", "msg_id": "m1"}),
    ("add_entry", [1, "EXMPLBAD"], {"claim": "another day", "msg_id": "m2"}),
    ("add_entry", [1, "  "], {}),
    ("add_entry", [1, "EXMPL0NAME"], {"confidence": "unconfirmed", "claim": "maybe"}),
    ("add_entry", [1, "EXMPLDISPUTED"], {"confidence": "disputed", "alt_of": "EXMPLBAD", "reported_by": "EXMPL_ME",
                                         "msg_url": "https://example.invalid/m"}),
    ("add_org", [1, " exmplorg "], {"claim": "pirates"}),
    ("add_org", [1, "EXMPLORG"], {"claim": "dup"}),
    ("mark_synced", [1], {}),
    ("lookup", ["exmplbad"], {"variants": False}),
    ("lookup", ["EXMPLONAME"], {"variants": True}),
    ("lookup", ["EXMPLONAME"], {"variants": False}),
    ("lookup", ["EXMPLONAME"], {}),
    ("lookup", ["  "], {}),
    ("reason_coverage", [], {}),
    ("stats", [], {}),
    ("orgs_flagged", [], {}),
    ("sources", [], {}),
    ("seed", ["starter_v1.json"], {}),
    ("seed", ["starter_v1.json"], {}),
    ("seed", ["starter_v2.json"], {}),
    ("seed", ["starter_bom.json"], {}),
    ("seed", ["starter_bad.json"], {}),
    ("seed", ["starter_noentries.json"], {}),
    ("seed", ["missing.json"], {}),
    ("sources", [], {}),
    ("lookup", ["EXMPLBAD"], {}),
    ("import_text", ["  EXMPL pasted  ", PASTE], {"url": "javascript:alert(1)", "note": " n ", "reported_by": "EXMPL_FRIEND",
                                                  "posted_ts": "2026-05-06T07:08:09.123456789Z"}),
    ("import_text", ["", "!!!\n???"], {}),
    ("import_text", ["", "EXMPLURL"], {"url": "HTTPS://example.invalid/list"}),
    ("set_enabled", [1, False], {}),
    ("lookup", ["EXMPLBAD"], {}),
    ("orgs_flagged", [], {}),
    ("set_enabled", [1, True], {}),
    ("forget_source", [2], {}),
    ("forget_source", [99], {}),
    ("sources", [], {}),
    ("stats", [], {}),
]


def dump(path: Path):
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    out = {}
    for (t,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"):
        cols = [r[1] for r in db.execute(f"PRAGMA table_info({t})")]
        names = ", ".join(f'"{c}"' for c in cols)
        out[t] = [list(r) for r in db.execute(f'SELECT {names} FROM "{t}" ORDER BY {names}')]
    db.close()
    return out


def js(x):
    if isinstance(x, watchlist.Hit):
        d = asdict(x)
        d["describe"] = x.describe()
        return d
    if isinstance(x, sqlite3.Row):
        return dict(x)
    if isinstance(x, (list, tuple)):
        return [js(v) for v in x]
    if isinstance(x, dict):
        return {k: js(v) for k, v in x.items()}
    return x


dbopen.forget()
wl_path = root / "watchlist.db"
wl = watchlist.Watchlist(wl_path)
steps = []
for i, (name, args, kwargs) in enumerate(OPS):
    NOW[0] = T0 + 60.0 * i
    watchlist._starter_name = "EXMPL bundled list"
    if name == "seed":
        result = watchlist.seed_starter(root / args[0], wl_path)
    elif name == "import_text":
        result = watchlist.import_text(args[0], args[1], path=wl_path, **kwargs)
    else:
        result = getattr(wl, name)(*args, **kwargs)
    if name == "stats":
        result.pop("path")
    steps.append({"at": NOW[0], "op": name, "args": args, "kwargs": kwargs, "result": js(result),
                  "dump": dump(wl_path)})
wl.close()

# reputation with this watchlist attached
dbopen.forget()
NOW[0] = T0 + 86400.0
wl = watchlist.Watchlist(wl_path)
rep = reputation.Database(root / "sc-watch.db", watchlist=wl)
rep.record_sighting("EXMPLBAD", 2.0, 2, 2)
rep.record_sighting("EXMPLONAME", 2.0, 2, 2)
assessed = []
for h, variants in (("EXMPLBAD", None), ("EXMPLONAME", True), ("EXMPLONAME", False), ("EXMPLNEW", None),
                    ("EXMPLNOBODY", None), ("EXMPLHEDGED", True)):
    a = rep.assess(h, variants=variants)
    d = asdict(a)
    d["flagged_orgs"] = [list(t) for t in a.flagged_orgs]
    d["former_orgs"] = [list(t) for t in a.former_orgs]
    d.update(score=a.score, alert=a.alert, spoken_all=a.spoken_all(), lines=a.lines())
    assessed.append({"handle": h, "variants": variants, "assess": d, "listed": rep.is_listed(h)})
rep.close()
wl.close()
(HERE / "watchlist_ops.json").write_text(json.dumps({
    "starters": {k: (root / k).read_bytes().hex() for k in STARTERS},
    "steps": steps,
    "parse": [{"text": t, "result": watchlist.parse_lines(t)} for t in PARSE],
    "safe_url": [[u, watchlist.safe_url(u)] for u in ["https://x", " HTTP://Y ", "ftp://z", "javascript:a", "", "h" * 400]],
    "assess_at": NOW[0], "assessed": assessed}, indent=0), encoding="utf-8", newline="\n")
print(len(steps), "watchlist operations,", len(PARSE), "pasted texts,", len(assessed), "assessments")
