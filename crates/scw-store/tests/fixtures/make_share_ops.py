"""share.py's own behaviour, for tests/share_ops.rs.

Three made-up installs - Bob, Alice, Carol - each with its own sc-watch.db
and flags.json: Bob exports, Alice previews and imports, Bob changes things
and exports again, Carol's import gets a hand-added member in first, and
Alice finally removes everything from Bob. After each step: the result and
every table of all three databases, and each flags.json. Then check() on
crafted documents, unpack() on crafted zips, and unpack_b64() on text.

    python make_share_ops.py      (sc-watch's build environment)

Made-up names, handles and orgs only; fixed times and install ids. Writes
share_ops.json.
"""
import base64
import io
import json
import sqlite3
import struct
import sys
import tempfile
import time
import types
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import dbopen  # noqa: E402
import flags as flags_mod  # noqa: E402
import reputation  # noqa: E402
import share  # noqa: E402

T0 = 1_790_000_000.0
NOW = [T0]
reputation.time = types.SimpleNamespace(time=lambda: NOW[0], localtime=time.gmtime,
                                        strftime=time.strftime)
share.time = types.SimpleNamespace(time=lambda: NOW[0],
                                   strftime=lambda f: time.strftime(f, time.gmtime(NOW[0])))

ROOT = Path(tempfile.mkdtemp())
WHO = ("bob", "alice", "carol")
INSTALL = {"bob": "b" * 32, "alice": "a" * 32, "carol": "c" * 32}
for w in WHO:
    (ROOT / w).mkdir()


def flags_path(w):
    return ROOT / w / "flags.json"


dbopen.forget()
DBS = {w: reputation.Database(ROOT / w / "sc-watch.db") for w in WHO}

STEPS = [
    *[("sql", w, "INSERT INTO meta (key, value) VALUES ('install', ?)", [INSTALL[w]]) for w in WHO],
    ("flags.save", "bob", [{"key": "griefing", "label": "Griefing", "color": "#e06c75",
                            "polarity": "negative", "say": "a griefer"},
                           {"key": "ram_raider", "label": "Ram raider", "color": "#ff0000",
                            "polarity": "negative", "say": "a ram raider"},
                           {"key": "helpful", "label": "Helpful", "color": "#00aa00",
                            "polarity": "positive", "say": ""}]),
    ("rep", "bob", "record_sighting", ["EXMPLHOSTILE", 2.0, 2, 2]),
    ("rep", "bob", "add_report", ["EXMPLHOSTILE", "griefing", "rammed\tus  at\nport"]),
    ("rep", "bob", "add_report", ["EXMPLPIRATE", "ram_raider", "custom reason"]),
    ("rep", "bob", "add_report", ["EXMPLFRIEND", "helpful", ""]),
    ("rep", "bob", "add_report", ["EXMPLPEND", "pending", "not yet"]),
    ("sql", "bob", "INSERT INTO reports (handle, ts, category, note, evidence, source, origin)"
                   " VALUES ('EXMPLSTRANGER', ?, 'griefing', 'from Zed', '', 'Zed', ?)",
     [T0 - 100, "d" * 32 + ":7"]),
    ("sql", "bob", "INSERT INTO reports (handle, ts, category, note, evidence, source)"
                   " VALUES ('EXMPLOTHER', ?, 'griefing', 'sourced', '', 'Zed')", [T0 - 90]),
    # imported, with no source recorded: its origin alone keeps it out
    ("sql", "bob", "INSERT INTO reports (handle, ts, category, note, evidence, source, origin)"
                   " VALUES ('EXMPLNOSOURCE', ?, 'griefing', 'imported', '', NULL, ?)",
     [T0 - 80, "d" * 32 + ":8"]),
    ("rep", "bob", "flag_org", ["EXMPLORG", "they camp", "Example Org"]),
    ("rep", "bob", "flag_org", ["EXMPLSHARED", "shared by Zed: x", "Shared Org"]),
    ("rep", "bob", "add_member_manually", ["EXMPLORG", "EXMPLHIDDEN", "hides off the roster"]),
    ("rep", "bob", "add_member_manually", ["EXMPLORG", "EXMPLLEFT", ""]),
    ("rep", "bob", "add_member_manually", ["EXMPLORG", "EXMPLLISTED", "also on the roster"]),
    ("sql", "bob", "UPDATE org_membership SET gone_since=? WHERE handle='EXMPLLEFT'", [T0 - 50]),
    ("sql", "bob", "INSERT INTO org_membership (sid, handle, display, rank, stars, first_seen,"
                   " last_seen, gone_since, src, manual_ts, note, manual_src) VALUES"
                   " ('EXMPLORG', 'EXMPLTHEIRS', '', '', 0, ?, ?, NULL, 'manual', ?, '', ?)",
     [T0 - 40, T0 - 40, T0 - 40, "d" * 32]),
    ("build", "bob", "", None),
    ("build", "bob", " Self ", None),
    ("build", "bob", "  Bob\x01 the\t\tMate  ", "b1"),
    ("receive", "b1"),
    ("filename", "b1"),
    # Alice: her own flags, a contact Bob reported, an org she flagged herself
    # and her own hand-added member on it
    ("rep", "alice", "record_sighting", ["EXMPLHOSTILE", 1.5, 3, 3]),
    ("rep", "alice", "flag_org", ["EXMPLORG", "my own note", "Example Org"]),
    ("rep", "alice", "add_member_manually", ["EXMPLORG", "EXMPLHIDDEN", "alice saw them"]),
    # on the roster Alice pulled: taking Bob's mark back must keep them listed
    ("rep", "alice", "record_roster", ["EXMPLORG", [{"handle": "EXMPLLISTED", "display": "Listed",
                                                      "rank": "Crew", "stars": 1}], True, "sentry"]),
    ("preview", "alice", "b1"),
    ("apply", "alice", "b1", True, True, True),
    ("preview", "alice", "b1"),
    # Carol gets a member in first
    ("rep", "carol", "flag_org", ["EXMPLORG", "", "Example Org"]),
    ("rep", "carol", "add_member_manually", ["EXMPLORG", "EXMPLSECOND", "carol's"]),
    ("rep", "carol", "add_report", ["EXMPLNEW", "zz_unknown", "a reason nobody defined"]),
    ("build", "carol", "Carol", "c1"),
    ("receive", "c1"),
    ("apply", "alice", "c1", True, True, True),
    # Bob changes things and exports again
    ("sql", "bob", "DELETE FROM reports WHERE handle='EXMPLHOSTILE'", []),
    ("sql", "bob", "UPDATE reports SET note='edited' WHERE handle='EXMPLPIRATE'", []),
    ("rep", "bob", "add_report", ["EXMPLNEW", "griefing", "new one"]),
    ("rep", "bob", "remove_manual_member", ["EXMPLORG", "EXMPLHIDDEN"]),
    ("rep", "bob", "add_member_manually", ["EXMPLORG", "EXMPLSECOND", "second"]),
    ("rep", "bob", "add_member_manually", ["EXMPLORG", "EXMPLTHIRDM", ""]),
    ("rep", "bob", "flag_org", ["EXMPLORG2", "", "Org Two"]),
    ("rep", "bob", "add_member_manually", ["EXMPLORG2", "EXMPLTWOM", "on org two"]),
    ("build", "bob", "Bob the Mate", "b2"),
    ("receive", "b2"),
    ("preview", "alice", "b2"),
    ("apply", "alice", "b2", True, False, True),     # org two not flagged: skipped
    ("apply", "alice", "b2", True, True, True),
    # your own export, and undoing an import
    ("build", "alice", "Alice", "a1"),
    ("receive", "a1"),
    ("preview", "alice", "a1"),
    ("apply", "alice", "a1", True, True, True),
    ("sources", "alice"),
    ("remove_source", "alice", "nothex"),
    ("remove_source", "alice", "e" * 32),
    ("remove_source", "alice", INSTALL["bob"]),
    ("sources", "alice"),
]


def dump(path):
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    out = {}
    for (t,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"):
        cols = [r[1] for r in db.execute(f"PRAGMA table_info({t})")]
        names = ", ".join(f'"{c}"' for c in cols)
        out[t] = [list(r) for r in db.execute(f'SELECT {names} FROM "{t}" ORDER BY {names}')]
    db.close()
    return out


def state():
    return {w: {"db": dump(ROOT / w / "sc-watch.db"),
                "flags": json.loads(flags_path(w).read_text(encoding="utf-8"))
                if flags_path(w).exists() else None} for w in WHO}


def use_flags(w):
    flags_mod.CONFIG_PATH = flags_path(w)
    flags_mod.invalidate()


docs, received = {}, {}
steps, last = [], None
for i, step in enumerate(STEPS):
    NOW[0] = T0 + 60.0 * i
    op, rest = step[0], step[1:]
    out = None
    try:
        if op == "sql":
            who, sql, args = rest
            DBS[who].db.execute(sql, args)
            DBS[who].db.commit()
        elif op == "flags.save":
            flags_mod.save(rest[1], path=flags_path(rest[0]))
            flags_mod.invalidate()
        elif op == "rep":
            who, method, args = rest
            use_flags(who)
            getattr(DBS[who], method)(*args)
        elif op == "build":
            who, name, label = rest
            use_flags(who)
            doc = share.build(DBS[who], name)
            docs[label] = doc
            out = {"ok": doc}
        elif op == "receive":
            label = rest[0]
            data = share.pack(docs[label])
            with zipfile.ZipFile(io.BytesIO(data)) as z:
                text = z.read("share.json").decode("utf-8")
            received[label] = share.unpack(data)
            out = {"ok": received[label], "zip": data.hex(), "json": text}
        elif op == "filename":
            out = {"ok": share.filename(docs[rest[0]])}
        elif op == "preview":
            who, label = rest
            use_flags(who)
            out = {"ok": share.preview(DBS[who], received[label])}
        elif op == "apply":
            who, label, r, o, m = rest
            use_flags(who)
            out = {"ok": share.apply(DBS[who], received[label], reports=r, orgs=o, roster=m)}
        elif op == "sources":
            out = {"ok": share.sources(DBS[rest[0]])}
        elif op == "remove_source":
            out = {"ok": share.remove_source(DBS[rest[0]], rest[1])}
        else:
            raise AssertionError(op)
    except share.ShareError as e:
        out = {"error": str(e)}
    st = state()
    steps.append({"at": NOW[0], "op": op, "args": list(rest), "result": out,
                  "state": None if st == last else st})
    last = st
for d in DBS.values():
    d.close()

# ---- check() on crafted documents ----------------------------------------------

NOW[0] = T0 + 100_000.0
BASE = json.loads(json.dumps(docs["b1"]))
FUTURE = NOW[0] + 5000.0


def doc(**changes):
    d = json.loads(json.dumps(BASE))
    for k, v in changes.items():
        if v is KeyError:
            d.pop(k, None)
        else:
            d[k] = v
    return d


def report(**changes):
    r = {"id": 1, "handle": "EXMPLR", "ts": T0, "category": "griefing", "note": "n"}
    r.update(changes)
    return [{k: v for k, v in r.items() if v is not KeyError}]


CHECKS = [
    ("not a dict", ["format", "sc-watch-share"]),
    ("wrong format", doc(format="other")),
    ("no version", doc(version=KeyError)),
    ("version text", doc(version="1")),
    ("version float", doc(version=1.0)),
    ("version true", doc(version=True)),
    ("version zero", doc(version=0)),
    ("version newer", doc(version=2)),
    ("no from", doc(**{"from": KeyError})),
    ("from self", doc(**{"from": " SELF "})),
    ("from number", doc(**{"from": 12345})),
    ("from messy", doc(**{"from": "\x00 Dan\x7f\n\n the   man " + "x" * 50})),
    ("bad install", doc(install="B" * 32)),
    ("short install", doc(install="a" * 31)),
    ("no export date", doc(exported_at=KeyError)),
    ("export date true", doc(exported_at=True)),
    ("export date text", doc(exported_at="5")),
    ("export date future", doc(exported_at=FUTURE)),
    ("reports not list", doc(reports={"a": 1})),
    ("reports null", doc(reports=None)),
    ("report not dict", doc(reports=[1])),
    ("missing lists", doc(reports=KeyError, flags=KeyError, orgs=KeyError, roster=KeyError)),
    ("bad handle", doc(reports=report(handle="a b"))),
    ("quote handle", doc(reports=report(handle="it's"))),
    ("both quotes", doc(reports=report(handle="it's \"x\"\\y"))),
    ("long bad handle", doc(reports=report(handle="é" + "z" * 40))),
    ("control handle", doc(reports=report(handle="a\x01\tb"))),
    ("null handle", doc(reports=report(handle=None))),
    ("int handle", doc(reports=report(handle=12))),
    ("lower handle", doc(reports=report(handle="exmpl_low-er"))),
    ("bad category", doc(reports=report(category="Griefing"))),
    ("pending category", doc(reports=report(category="pending"))),
    ("no category", doc(reports=report(category=KeyError))),
    ("no id", doc(reports=report(id=KeyError))),
    ("id true", doc(reports=report(id=True))),
    ("id negative", doc(reports=report(id=-1))),
    ("id float", doc(reports=report(id=1.5))),
    ("duplicate ids", doc(reports=report() + report(handle="EXMPLR2"))),
    ("no report date", doc(reports=report(ts=KeyError))),
    ("future report", doc(reports=report(ts=FUTURE))),
    ("int report date", doc(reports=report(ts=1790000000))),
    ("null note", doc(reports=report(note=None))),
    ("number note", doc(reports=report(note=5))),
    ("long note", doc(reports=report(note="w" * 600))),
    ("control note", doc(reports=report(note="\x00a\x1fb\x7f "))),
    ("bad flag key", doc(flags=[{"key": "Bad Key"}])),
    ("pending flag", doc(flags=[{"key": "pending", "label": "x"}])),
    ("bare flag", doc(flags=[{"key": "k"}])),
    ("messy flag", doc(flags=[{"key": "k2", "label": "  L\x01  ", "color": "#ABCDEF",
                               "polarity": "positive", "say": "s" * 80}])),
    ("flag bad color", doc(flags=[{"key": "k3", "color": "red", "polarity": "evil"}])),
    ("flag label number", doc(flags=[{"key": "k4", "label": 3}])),
    ("bad sid", doc(orgs=[{"sid": "no spaces"}])),
    ("org caps", doc(orgs=[{"sid": "lower", "name": "n" * 100, "note": "t" * 300}])),
    ("org name number", doc(orgs=[{"sid": "S", "name": 1}])),
    ("bad roster", doc(roster=[{"sid": "S", "handle": "x"}])),
    ("roster no ts", doc(roster=[{"sid": "s", "handle": "hh"}])),
    ("roster future", doc(roster=[{"sid": "s", "handle": "hh", "ts": FUTURE}])),
    ("roster ts true", doc(roster=[{"sid": "s", "handle": "hh", "ts": True}])),
    ("roster null ts", doc(roster=[{"sid": "s", "handle": "hh", "ts": None}])),
    ("extra keys", doc(extra={"x": 1})),
    # Where the port refuses what share.py lets through (tests/share_ops.rs
    # DELIBERATE): Python's `$` also matches before a trailing newline, and
    # True passes as an int.
    ("install newline", doc(install="a" * 32 + "\n")),
    ("handle newline", doc(reports=report(handle="EXMPLNL\n"))),
    ("category newline", doc(reports=report(category="griefing\n"))),
    ("color newline", doc(flags=[{"key": "k5", "color": "#abcdef\n"}])),
]
checks = []
for label, d in CHECKS:
    try:
        checks.append({"label": label, "doc": d, "ok": share.check(d)})
    except share.ShareError as e:
        checks.append({"label": label, "doc": d, "error": str(e)})
# Too many rows: made on both sides rather than stored.
try:
    share.check(doc(reports=[{}] * (share.MAX_ROWS + 1)))
except share.ShareError as e:
    too_many = str(e)

# ---- unpack() on crafted zips ----------------------------------------------------


def zipped(members, method=zipfile.ZIP_DEFLATED):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", method) as z:
        for name, data in members:
            z.writestr(name, data)
    return buf.getvalue()


GOOD = json.dumps(BASE).encode()


def patched_central(data, offset, value, fmt):
    """Rewrites a field of the first central-directory entry."""
    at = data.rfind(b"PK\x01\x02")
    b = bytearray(data)
    struct.pack_into(fmt, b, at + offset, value)
    return bytes(b)


def encrypted():
    data = zipped([("share.json", GOOD)])
    at = data.rfind(b"PK\x01\x02")
    flags_ = struct.unpack_from("<H", data, at + 8)[0]
    return patched_central(data, 8, flags_ | 1, "<H")


def dup():
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        import warnings
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            z.writestr("share.json", json.dumps(doc(**{"from": "First"})).encode())
            z.writestr("share.json", json.dumps(doc(**{"from": "Second"})).encode())
    return buf.getvalue()


UNPACKS = [
    ("not a zip", b"not a zip at all"),
    ("empty", b""),
    ("no member", zipped([("other.json", GOOD)])),
    ("encrypted", encrypted()),
    ("declared too big", patched_central(zipped([("share.json", GOOD)]), 24, share.MAX_JSON + 1, "<I")),
    ("bad json", zipped([("share.json", b"{nope")])),
    ("not utf-8", zipped([("share.json", b'{"a": "\xff"}')])),
    ("bom", zipped([("share.json", b"\xef\xbb\xbf" + GOOD)])),
    ("deep", zipped([("share.json", b"[" * 5000 + b"]" * 5000)])),
    ("nan date", zipped([("share.json", GOOD.replace(b'"exported_at": ', b'"exported_at": NaN, "x": '))])),
    ("good", zipped([("share.json", GOOD), ("README.txt", b"hi")])),
    ("stored", zipped([("share.json", GOOD)], zipfile.ZIP_STORED)),
    ("duplicate member", dup()),
    ("subfolder member", zipped([("x/share.json", GOOD)])),
]
unpacks = []
for label, data in UNPACKS:
    try:
        unpacks.append({"label": label, "zip": data.hex(), "ok": share.unpack(data)})
    except share.ShareError as e:
        unpacks.append({"label": label, "zip": data.hex(), "error": str(e)})
try:
    share.unpack(b"\0" * (share.MAX_ZIP + 1))
except share.ShareError as e:
    too_big_zip = str(e)

good_b64 = base64.b64encode(zipped([("share.json", GOOD)])).decode()
B64 = [good_b64, good_b64[:-1], good_b64[:40] + "\n" + good_b64[40:], good_b64 + "=",
       "=" + good_b64, good_b64.replace("+", "-"), "QQ==", "QQ=", "QQ", "QUI=", "QUJD", "", "é",
       "QUJD QUJD", "QUJD===="]
b64 = []
for text in B64:
    try:
        b64.append({"text": text, "ok": share.unpack_b64(text)})
    except share.ShareError as e:
        b64.append({"text": text, "error": str(e)})

(HERE / "share_ops.json").write_text(json.dumps({
    "t0": T0, "installs": INSTALL, "steps": steps,
    "check_at": NOW[0], "checks": checks, "too_many_rows": too_many,
    "unpacks": unpacks, "too_big_zip": too_big_zip, "b64": b64,
    "readme": share.README,
}, indent=0), encoding="utf-8", newline="\n")
print(len(steps), "steps,", len(checks), "documents,", len(unpacks), "zips,", len(b64), "texts")
