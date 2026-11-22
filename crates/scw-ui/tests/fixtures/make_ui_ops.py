"""ui_server.py itself, for tests/ui_ops.rs: the real server, on a socket,
asked a script of requests - the page's files, the state, and every write
the page can make - on a fake clock and a made-up network.

The network is a WORLD: made-up profiles, orgs and member lists, each
answered as SENTRY answers (the exact replies are recorded here, so the
Rust test serves the same ones). Background jobs run inline, the folder
button and the voice are recorders, the joysticks are made up, the trusted
starter list is never read. Per step: the status, the content type and
the body (JSON parsed; files by hash), and at checkpoints every table of
all three databases and the files the server writes.

    python make_ui_ops.py      (sc-watch's build environment)

Made-up handles and orgs only. Writes ui_ops.json.
"""
import base64
import contextlib
import hashlib
import http.client as httpclient
import io
import json
import shutil
import sqlite3
import sys
import tempfile
import threading
import time
import types
from datetime import date, datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[2] / "scw-rsi" / "tests" / "fixtures"))
import fakenet  # noqa: E402  (points sc-watch at a temporary data folder first)
from fakenet import CLOCK, NC, NF, http, iso, oprof, player  # noqa: E402

import paths  # noqa: E402
import audio  # noqa: E402
import flags as flags_mod  # noqa: E402
import housekeeping  # noqa: E402
import imgio  # noqa: E402
import inputs  # noqa: E402
import numpy as np  # noqa: E402
import reputation  # noqa: E402
import rsi  # noqa: E402
import share  # noqa: E402
import ui_server  # noqa: E402
import watchlist  # noqa: E402

DATA = paths.DATA_ROOT
PROGRAM = str(paths.RESOURCE_ROOT)
watchlist.STARTER_PATH = DATA / "no-starter-list-here.json"   # never the bundled list
rsi.PROCESS = "page"


def _sleep(s):
    fakenet._sleep(s)
    if s == 0.4:                      # the stream poll: one frame, then stop
        SRV._shutting_down = True


def _gmstrftime(fmt, t=None):
    return time.strftime(fmt, time.gmtime(CLOCK["wall"] if t is None else t))


ui_server.time = types.SimpleNamespace(time=lambda: CLOCK["wall"], monotonic=lambda: CLOCK["mono"],
                                       sleep=_sleep)
reputation.time = types.SimpleNamespace(time=lambda: CLOCK["wall"], localtime=time.gmtime,
                                        strftime=time.strftime)
watchlist.time = types.SimpleNamespace(time=lambda: CLOCK["wall"])
share.time = types.SimpleNamespace(time=lambda: CLOCK["wall"], strftime=_gmstrftime)
housekeeping.time = types.SimpleNamespace(time=lambda: CLOCK["wall"])
flags_mod.time = types.SimpleNamespace(monotonic=lambda: CLOCK["mono"])


class FakeDate(date):
    @classmethod
    def today(cls):
        d = datetime.fromtimestamp(CLOCK["wall"], timezone.utc).date()
        return cls(d.year, d.month, d.day)


sys.modules["datetime"].date = FakeDate


class InlineThread:
    def __init__(self, target=None, args=(), daemon=None, name=None):
        self.target, self.args = target, args

    def start(self):
        self.target(*self.args)


ui_server.threading = types.SimpleNamespace(Thread=InlineThread, Lock=threading.Lock)
REVEALED, SAID = [], []
ui_server._reveal = lambda p: REVEALED.append(str(p))


class Voice:
    def say(self, text):
        SAID.append(text)


ui_server._preview_voice = lambda: (Voice(), "")

DEVICE = inputs.Device(index=0, vid=0x231D, pid=0x0127, name="EXMPL stick", buttons=32, axes=5)
HELD = {"keys": {0xA3, 0x11, 0x41}, "mask": (1 << 0) | (1 << 23)}
inputs.devices = lambda: [DEVICE]
inputs.key_down = lambda vk: vk in HELD["keys"]
inputs.buttons_down = lambda i: HELD["mask"] if i == 0 else 0

# ---- the world ---------------------------------------------------------------------

ANSWERS = {}           # "citizen:H" / "org:SID" / "roster:SID:N" -> the reply


def world():
    for h, orgs, enlisted in (
            ("EXMPLONE", [("EXMPLORG", True, "Member", "visible")], "2015-03-01T10:00:00.000000"),
            ("EXMPLTWO", [], "2016-07-01T10:00:00.000000"),
            ("EXMPLNEW", [], iso(30)),
            ("EXMPLCREWA", [("EXMPLCREW", True, "Crew", "visible")], "2014-01-01T10:00:00.000000"),
            ("EXMPLAUDX", [("EXMPLCREW", False, "Crew", "visible")], "2017-01-01T10:00:00.000000"),
            ("EXMPLNEWBIE", [], "2018-01-01T10:00:00.000000"),
            ("EXMPLBACK", [], "2019-01-01T10:00:00.000000")):
        ANSWERS[f"citizen:{h}"] = player(h, days=2.0, orgs=orgs, enlisted=enlisted)
        ANSWERS[f"meta:{h}"] = {"json": {"last_fetched": iso(2.0)}}
    for h in ("EXMPLGHOST", "EXMPLNOONE", "EXMPLCREWC"):
        ANSWERS[f"citizen:{h}"] = http(404, **{"X-Lookup-State": "not_found"})
    ANSWERS["citizen:EXMPLFAIL"] = http(500)
    ANSWERS["org:EXMPLORG"] = oprof("Example Org", 10, "Piracy", "Trading")
    ANSWERS["org:EXMPLCREW"] = oprof("Example Crew", 3, "Security", None)
    ANSWERS["roster:EXMPLCREW:1"] = {"json": {
        "memberships": {"pages": 1, "total": 3, "members": [
            {"citizen_handle": h, "display_name": h.title(), "member_rank": "Crew",
             "rank_number": 2, "visibility": "visible"}
            for h in ("EXMPLCREWA", "EXMPLCREWB", "EXMPLCREWC")]},
        "metadata": {"last_fetched": iso(1.0)}}}


def answer_key(key):
    """Which world answer a request is for."""
    if not key.startswith("sentry:"):
        raise fakenet.Unscripted(key)
    path, _, query = key[len("sentry:"):].partition("?")
    bits = path.strip("/").split("/")
    if bits[0] == "citizens" and len(bits) == 2:
        return f"citizen:{bits[1].upper()}"
    if bits[0] == "citizens" and len(bits) == 3 and bits[2] == "metadata":
        return f"meta:{bits[1].upper()}"
    if bits[0] == "orgs" and len(bits) == 3 and bits[2] == "profile":
        return f"org:{bits[1].upper()}"
    if bits[0] == "orgs" and len(bits) == 3 and bits[2] == "memberships":
        page = dict(p.split("=") for p in query.split("&") if "=" in p).get("page", "1")
        return f"roster:{bits[1].upper()}:{page}"
    raise fakenet.Unscripted(key)


DEFAULTS = {"citizen": http(404, **{"X-Lookup-State": "not_checked"}), "org": http(404),
            "roster": http(404), "meta": http(404)}
_respond = fakenet.respond


def respond(key, timeout):
    k = answer_key(key)
    fakenet.ROUTES[key] = [ANSWERS.get(k, DEFAULTS[k.split(":")[0]])]
    return _respond(key, timeout)


fakenet.respond = respond

# ---- the steps ---------------------------------------------------------------------

PNG = imgio.imencode if hasattr(imgio, "imencode") else None


def png_bytes(seed):
    y, x, c = np.meshgrid(np.arange(6), np.arange(10), np.arange(3), indexing="ij")
    img = ((seed * 31 + y * 7 + x * 3 + c * 50) % 256).astype(np.uint8)
    p = Path(tempfile.mkdtemp()) / "x.png"
    imgio.imwrite(p, img)
    return p.read_bytes()


FILES = {"audit/20260920_101010_EXMPLAUD_crop_name.png": png_bytes(1),
         "audit/20260920_101010_EXMPLAUD_crop_range.png": png_bytes(2),
         "audit/20260920_101010_EXMPLAUD_frame.jpg": png_bytes(3) + bytes(200_000),
         "audit/.hidden.png": png_bytes(4)}
INSTALL = "0123456789abcdef0123456789abcdef"
MATE = "fedcba9876543210fedcba9876543210"


def mate_share():
    """An org mate's export, as the page would be handed it."""
    root = Path(tempfile.mkdtemp())
    db = reputation.Database(root / "mate.db")
    db.db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('install', ?)", (MATE,))
    db.db.commit()
    db.add_report("EXMPLMATEPICK", "camping", "at the pad")
    db.add_report("EXMPLONE", "unprovoked_attack", "")
    db.flag_org("EXMPLMATEORG", "they pirate", "Mate Org")
    db.add_member_manually("EXMPLMATEORG", "EXMPLMATEMEMBER", "seen with them")
    doc = share.build(db, "EXMPL Mate")
    db.close()
    return base64.b64encode(share.pack(doc)).decode("ascii")


SETUP = [
    ["sql", "INSERT OR REPLACE INTO meta (key, value) VALUES ('install', ?)", [INSTALL]],
    ["sight", "EXMPLONE", 2.5, 3, 3], ["advance", 60],
    ["sight", "EXMPLONE", 2.0, 2, 3], ["advance", 60],
    ["sight", "EXMPLTWO", 4.0, 1, 1], ["advance", 30],
    ["sight", "EXMPLGHOST", 1.5, 1, 1], ["sight", "EXMPLGHOST", 1.5, 1, 1], ["advance", 30],
    ["sight", "EXMPLAUD", 0.8, 1, 1], ["advance", 30],
    ["sight", "EXMPLCREWA", 3.0, 2, 2], ["sight", "EXMPLNEW", 5.0, 2, 2],
    ["sight", "EXMPLFAIL", 6.0, 2, 2], ["sight", "EXMPLBACK", 6.5, 3, 3], ["advance", 30],
    ["audit", "EXMPLAUD", 0.8, 1, 1, [["EXMPLAVD", 1]],
     "audit/20260920_101010_EXMPLAUD_frame.jpg", "audit/20260920_101010_EXMPLAUD_crop_name.png",
     "audit/20260920_101010_EXMPLAUD_crop_range.png"],
    ["audit", "EXMPLONE", 2.5, 1, 2, [], "", "audit/gone_crop_name.png", ""],
    ["files"],
]


def J(path, body=None, **kw):
    return {"method": "POST", "path": path, "body": body if body is not None else {}, **kw}


def G(path, **kw):
    return {"method": "GET", "path": path, **kw}


STEPS = [
    G("/"), G("/index.html"), G("/app.js"), G("/app.css"), G("/nothing"),
    G("/", host="evil.example"), G("/api/contacts", host=None),
    J("/api/flag", {"handle": "EXMPLONE"}, ctype="text/plain"),
    J("/api/not-a-route"),
    J("/api/flag", raw="", length="99999999"),
    J("/api/flag", raw="", length="-5"),
    J("/api/flag", raw="", length="abc"),
    J("/api/flag", raw="[1, 2]"),
    G("/api/contacts"),
    {"op": "tables"},
    G("/api/contacts?view=audits"), G("/api/contacts?view=reports&arg=self"),
    G("/api/contacts?view=reports&arg=bogus"), G("/api/contacts?view=nope"),
    J("/api/settings", {"values": {"max_contacts": 50, "rsi_min_votes": 1, "bogus": 3}}),
    J("/api/flag", {"handle": "exmplone"}),
    J("/api/flag", {"handle": "bad handle!"}), J("/api/flag", {"handle": None}),
    J("/api/flag", {}),
    J("/api/reason", {"handle": "EXMPLONE", "category": "unprovoked_attack", "note": "rammed us"}),
    J("/api/reason", {"handle": "EXMPLONE", "category": "nonsense"}),
    J("/api/report/add", {"handle": "EXMPLTWO", "category": "camping", "note": "at the pad"}),
    J("/api/report/add", {"handle": "EXMPLNEVERMET", "category": "camping", "note": ""}),
    J("/api/report/add", {"handle": "EXMPLTWO", "category": "nonsense"}),
    J("/api/report/edit", {"id": 2, "category": "unprovoked_attack", "note": "edited"}),
    J("/api/report/edit", {"id": 2, "note": "only the note"}),
    J("/api/report/delete", {"id": 3}),
    J("/api/report/delete", {"id": "abc"}), J("/api/report/delete", {"id": 999}),
    J("/api/ignore", {"handle": "EXMPLJUNK", "note": "a crate"}),
    J("/api/unignore", {"label": "EXMPLJUNK"}), J("/api/unignore", {"label": "EXMPLNOTHING"}),
    J("/api/player/enlisted", {"handle": "EXMPLTWO", "enlisted": "2016-05-01"}),
    J("/api/player/enlisted", {"handle": "EXMPLTWO", "enlisted": "2099-01-01"}),
    J("/api/player/enlisted", {"handle": "EXMPLTWO", "enlisted": "2010-01-01"}),
    J("/api/player/enlisted", {"handle": "EXMPLTWO", "enlisted": "not a date"}),
    J("/api/player/enlisted", {"handle": "EXMPLGHOST", "enlisted": "20170203"}),
    J("/api/player/enlisted", {"handle": "EXMPLGHOST", "enlisted": ""}),
    {"op": "tables"},
    J("/api/lookup", {"handle": "exmplone"}),
    J("/api/lookup", {"handle": "EXMPLNOONE"}),
    J("/api/lookup", {"handle": "EXMPLFAIL"}),
    J("/api/lookup", {"handle": "EXMPLNEWBIE"}),
    J("/api/lookup", {"handle": "EXMPLNEWBIE", "add": True}),
    J("/api/lookup", {"handle": "!!"}),
    J("/api/lookup/background", {"handle": "EXMPLTWO"}),
    J("/api/lookup/background", {"handle": "EXMPLNEW"}),
    J("/api/lookup/background", {"handle": ""}),
    J("/api/lookup/background", {"handle": "EXMPLTWO"}),
    G("/api/contacts"),
    G("/audit/20260920_101010_EXMPLAUD_crop_name.png"),
    G("/audit/../sc-watch.db"), G("/audit/missing.png"), G("/audit/.hidden.png"),
    G("/audit/notes.txt"), G("/audit/20260920_101010_EXMPLAUD_frame.jpg"),
    G("/audit/sub/20260920_101010_EXMPLAUD_crop_name.png"),
    J("/api/audit/confirm", {"id": 1, "name": "EXMPLAUDX"}),
    J("/api/audit/confirm", {"id": 99, "name": "EXMPLAUDX"}),
    J("/api/audit/confirm", {"id": 2, "name": "no good"}),
    J("/api/audit/dismiss", {"id": 2}),
    {"op": "tables"},
    J("/api/org/flag", {"sid": "exmplcrew", "note": "they camp"}),
    J("/api/org/flag", {"sid": "EXMPLORG", "roster": False}),
    J("/api/org/flag", {"sid": "bad sid!"}),
    J("/api/org/flag", {"sid": "EXMPLCREW", "note": "they camp, still", "roster": True}),
    J("/api/org/members", {"sid": "EXMPLCREW"}),
    J("/api/org/members", {"sid": "EXMPLNOTFLAGGED"}),
    J("/api/org/lookup", {"sid": "EXMPLCREW"}),
    J("/api/org/lookup", {"sid": "EXMPLCREW"}),
    J("/api/org/stop", {"sid": "EXMPLCREW"}),
    J("/api/org/member/add", {"sid": "EXMPLCREW", "handle": "EXMPLTWO", "note": "seen with them"}),
    J("/api/org/member/add", {"sid": "EXMPLCREW", "handle": "EXMPLCREWA"}),
    J("/api/org/member/add", {"sid": "EXMPLCREW", "handle": "EXMPLSTRANGER"}),
    J("/api/org/member/remove", {"sid": "EXMPLCREW", "handle": "EXMPLTWO"}),
    J("/api/org/member/remove", {"sid": "EXMPLCREW", "handle": "EXMPLCREWA"}),
    J("/api/org/manual", {"sid": "EXMPLORG", "primary": "Piracy", "secondary": "Piracy",
                          "name": "Example"}),
    G("/api/contacts"),
    J("/api/org/manual", {"sid": "EXMPLORG", "primary": "Knitting"}),
    J("/api/org/manual", {"sid": "EXMPLORG"}),
    J("/api/org/roster", {"sid": "EXMPLCREW"}),
    J("/api/org/roster", {"sid": "EXMPLNOTFLAGGED"}),
    G("/api/contacts"),
    J("/api/org/unflag", {"sid": "EXMPLORG"}),
    {"op": "tables"},
    J("/api/watchlist/list"),
    J("/api/watchlist/preview", {"text": "EXMPLWATCH - rammed a station\nEXMPL2 camping\n???\n"}),
    J("/api/watchlist/import", {"text": "EXMPLWATCH - rammed a station", "name": ""}),
    J("/api/watchlist/import", {"text": "EXMPLWATCH - rammed a station\nEXMPLTWO - griefing",
                                "name": "EXMPL list", "by": "EXMPL Friend", "unsure": True}),
    J("/api/watchlist/list"),
    G("/api/contacts"),
    J("/api/watchlist/enable", {"id": 1, "on": False}),
    J("/api/watchlist/enable", {"id": 1, "on": True}),
    J("/api/watchlist/forget", {"id": 1}),
    J("/api/watchlist/nope"),
    J("/api/share/export", {"name": "EXMPL Me"}),
    J("/api/share/export", {"name": ""}),
    J("/api/share/sources"),
    J("/api/share/preview", {"data": "MATE"}),
    J("/api/share/preview", {"data": "bm90IGEgemlw"}),
    J("/api/share/import", {"data": "MATE", "include": {"roster": False}}),
    J("/api/share/sources"),
    G("/api/contacts?view=reports"),
    G(f"/api/contacts?view=reports&arg={MATE}"),
    J("/api/share/remove", {"install": MATE}),
    {"op": "tables"},
    J("/api/audio/phrases"),
    J("/api/audio/phrases", {"values": {"known": "Known. {handle}. {reason}"}, "muted": ["nothing", "bogus"]}),
    J("/api/audio/phrases", {"values": {"known": "{nope}"}}),
    J("/api/audio/preview", {"key": "known", "text": "Known {handle}"}),
    J("/api/audio/preview", {"key": "known", "text": "{bad}"}),
    J("/api/audio/preview", {"key": "nothing", "text": ""}),
    J("/api/flags", {"flags": "nope"}),
    J("/api/flags", {"flags": [{"key": "camping", "label": "Camping a spot", "color": "#ff8800",
                               "polarity": "negative", "say": "camping"},
                              {"key": "helpful", "label": "Helped us", "color": "#00ff88",
                               "polarity": "positive"}]}),
    G("/api/contacts"),
    J("/api/restart-watcher"),
    J("/api/open-folder", {"key": "data"}), J("/api/open-folder", {"key": "nope"}),
    J("/api/open-folder", {"key": "program"}), J("/api/open-folder", {"key": "audit"}),
    J("/api/maintenance", {"job": "usage"}),
    J("/api/maintenance", {"job": "frames"}),
    J("/api/maintenance", {"job": "frames", "apply": True}),
    J("/api/maintenance", {"job": "backfill"}),
    J("/api/maintenance", {"job": "backfill", "apply": "yes"}),
    J("/api/maintenance", {"job": "backfill", "apply": True}),
    J("/api/maintenance", {"job": "ghosts"}),
    J("/api/maintenance", {"job": "ghosts", "apply": True}),
    J("/api/maintenance", {"job": "nope"}),
    {"op": "tables"},
    G("/api/inputs"),
    {"op": "advance", "s": 7200},
    {"op": "sweep"},
    {"op": "stream"},
    J("/api/apikey", {"key": "short"}),
    J("/api/apikey", {"key": "EXMPLKEY12345678"}),
    J("/api/apikey", {"remove": True}),
    J("/api/settings", {"values": {"watchlist_enabled": False}}),
    G("/api/contacts"),
    J("/api/forget", {"handle": "EXMPLTWO"}),
    G("/api/contacts"),
    {"op": "tables"},
]


def norm(v):
    """Folder paths as <DATA> and <PROGRAM>, so either side's compare."""
    if isinstance(v, str):
        for real, tag in ((str(DATA), "<DATA>"), (PROGRAM, "<PROGRAM>")):
            v = v.replace(real, tag)
        return v.replace("\\", "/")
    if isinstance(v, list):
        return [norm(x) for x in v]
    if isinstance(v, dict):
        return {k: norm(x) for k, x in v.items()}
    return v


def dump(path):
    if not path.exists():
        return None
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    out = {}
    for (t,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"):
        cols = [r[1] for r in db.execute(f"PRAGMA table_info({t})")]
        names = ", ".join(f'"{c}"' for c in cols)
        out[t] = [list(r) for r in db.execute(f'SELECT {names} FROM "{t}" ORDER BY {names}')]
    db.close()
    return out


def files():
    out = {}
    for rel in ("restart.flag", "sc_api_key.txt", "settings.json", "flags.json", "audio.json"):
        p = DATA / rel
        out[rel] = p.read_text(encoding="utf-8") if p.exists() else None
    out["audit"] = sorted(p.name for p in (DATA / "audit").glob("*")) if (DATA / "audit").exists() else []
    return out


SRV = None
ERRORS = []


def _hook(a):
    import traceback
    ERRORS.append("".join(traceback.format_exception(a.exc_type, a.exc_value, a.exc_traceback)))


threading.excepthook = _hook


def request(step, mate_b64):
    conn = httpclient.HTTPConnection("127.0.0.1", SRV.server_address[1], timeout=60)
    headers = {}
    host = step.get("host", "127.0.0.1:8731")
    body = None
    if step["method"] == "POST":
        if "raw" in step:
            body = step["raw"].encode()
        else:
            payload = json.loads(json.dumps(step["body"]).replace('"MATE"', json.dumps(mate_b64)))
            body = json.dumps(payload).encode()
        headers["Content-Type"] = step.get("ctype", "application/json")
        headers["Content-Length"] = step.get("length", str(len(body)))
    conn.putrequest(step["method"], step["path"], skip_host=True, skip_accept_encoding=True)
    if host is not None:
        conn.putheader("Host", host)
    for k, v in headers.items():
        conn.putheader(k, v)
    conn.endheaders()
    if body and int(headers.get("Content-Length", "0") if headers.get("Content-Length", "0").lstrip("-").isdigit() else 0) > 0 \
            and int(headers["Content-Length"]) < 10_000_000:
        conn.send(body)
    resp = conn.getresponse()
    ctype = resp.getheader("Content-Type") or ""
    if ctype == "text/event-stream":
        # The stream says keep-alive, so the connection stays open: one event.
        lines = []
        while True:
            line = resp.fp.readline().decode()
            lines.append(line)
            if line.strip() == "" and len(lines) > 1:
                break
        data = "".join(lines).encode()
    else:
        data = resp.read()
    conn.close()
    out = {"status": resp.status, "ctype": ctype, "cache": resp.getheader("Cache-Control")}
    if ctype == "application/json":
        out["json"] = norm(json.loads(data))
    elif ctype == "text/event-stream":
        frames = [f for f in data.decode().split("\n\n") if f]
        out["events"] = [norm(json.loads(f[len("data: "):])) if f.startswith("data: ") else f
                         for f in frames]
    elif resp.status == 200:
        out["sha1"] = hashlib.sha1(data).hexdigest()
        out["bytes"] = len(data)
    return out


def setup(db):
    for op in SETUP:
        kind = op[0]
        if kind == "sql":
            db.db.execute(op[1], op[2])
            db.db.commit()
        elif kind == "sight":
            db.record_sighting(*op[1:])
        elif kind == "advance":
            fakenet._sleep(op[1])
        elif kind == "audit":
            h, km, v, t, var, frame, cn, cr = op[1:]
            db.add_audit(h, km, v, t, var, frame_path=frame, crop_name=cn, crop_range=cr)
        elif kind == "files":
            for rel, data in FILES.items():
                p = DATA / rel
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_bytes(data)


def main():
    global SRV
    world()
    mate_b64 = mate_share()
    db = reputation.Database()
    setup(db)
    db.close()
    SRV = ui_server.ThreadingHTTPServer(("127.0.0.1", 0), ui_server.Handler)
    SRV._shutting_down = False
    SRV.daemon_threads = True
    threading.Thread(target=SRV.serve_forever, daemon=True).start()
    results = []
    for step in STEPS:
        fakenet.SENT.clear()
        REVEALED.clear()
        SAID.clear()
        op = step.get("op")
        if op == "tables":
            out = {"op": "tables", "db": dump(DATA / "sc-watch.db"),
                   "cache": dump(DATA / "rsi_cache.db"), "watchlist": dump(DATA / "watchlist.db"),
                   "files": files()}
            results.append(norm(out))
            continue
        if op == "advance":
            fakenet._sleep(step["s"])
            results.append({"op": "advance", "s": step["s"]})
            continue
        if op == "sweep":
            said = []
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                client = ui_server._client()
                with rsi.why("bg-focus"):
                    got = client.complete_partials(ask=True)
                if got["completed"] or got["filled"]:
                    said.append(f"  org focus check: {got['completed']} profile(s) completed, "
                                f"{got['filled']} filled in part")
                with rsi.why("bg-names"):
                    named = ui_server._fill_flag_names(client, True)
                if named:
                    said.append(f"  org focus check: named {named} flagged org(s)")
                with rsi.why("bg-retry"):
                    again = ui_server._retry_failed_lookups(client, True)
                if again["found"]:
                    said.append(f"  background check: {again['found']} failed lookup(s) "
                                f"answered ({again['live']} needed a live fetch)")
            results.append({"op": "sweep", "said": said, "sent": list(fakenet.SENT)})
            continue
        if op == "stream":
            r = request(G("/api/stream"), mate_b64)
            SRV._shutting_down = False
            results.append({"op": "stream", **r, "sent": list(fakenet.SENT)})
            continue
        try:
            r = request(step, mate_b64)
        except Exception:
            print("FAILED AT", step)
            time.sleep(1.0)
            print("\n".join(ERRORS)[-3000:])
            raise SystemExit(1)
        r["sent"] = [s["url"] for s in fakenet.SENT]
        if REVEALED:
            r["revealed"] = norm(list(REVEALED))
        if SAID:
            r["said"] = list(SAID)
        results.append({"step": step, **r})
    SRV.shutdown()
    (HERE / "ui_ops.json").write_text(json.dumps({
        "wall0": 1_790_000_000.0, "mono0": 5000.0, "answers": ANSWERS, "defaults": DEFAULTS,
        "setup": SETUP, "files": {k: base64.b64encode(v).decode() for k, v in FILES.items()},
        "mate": mate_b64, "device": {"index": 0, "vid": DEVICE.vid, "pid": DEVICE.pid,
                                     "name": DEVICE.name, "buttons": 32, "axes": 5},
        "held": {"keys": sorted(HELD["keys"]), "mask": HELD["mask"]},
        "results": results}, indent=0), encoding="utf-8", newline="\n")
    print(len(results), "steps;", sum(1 for r in results if r.get("status") not in (None, 200)),
          "non-200")


main()
