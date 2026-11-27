"""watch.Session's own behaviour, for tests/session_ops.rs.

The real Session - bursts, the vote, announcing, settling into the
database, audits with their pictures, sightings.csv, the key and auto loops,
ping delay and queue, restart, housekeeping, shutdown and summary - with
what it is given faked:

  frames      small made-up images; pixel (0,0) is the frame's number
  detector    Session._detect returns the contacts scripted for that frame
  scan        the always-on scan returns the positions scripted for it
  keys        Binding.pressed fires at the scripted grab counts, and stops
              the session (KeyboardInterrupt, as Ctrl+C) at the last one
  chat        chat.read_located returns the lines scripted for the frame
              (or raises, for a failed read)
  clock       fake: sleeping moves it; dates in UTC
  the rest    alerts, console, session log and the lookup queue recorded;
              a real, temporary sc-watch.db and sightings.csv

Per scenario: the exit code, every line said and printed, the status line
as it changed, alerts, log events, lookups submitted, what was detected,
the tables, sightings.csv and the audit pictures.

    python make_session_ops.py      (sc-watch's build environment)

Made-up handles only. Writes session_ops.json.
"""
import contextlib
import io
import json
import sqlite3
import sys
import tempfile
import time
import types
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import chat  # noqa: E402
import dbopen  # noqa: E402
import housekeeping  # noqa: E402
import imgio  # noqa: E402
import inputs  # noqa: E402
import reputation  # noqa: E402
import sc_detector as sd  # noqa: E402
import settings as settings_mod  # noqa: E402
import watch  # noqa: E402

CLOCK = {"wall": 0.0, "mono": 0.0}


def sleep(s):
    if s > 0:
        CLOCK["wall"] += s
        CLOCK["mono"] += s
    if CLOCK["mono"] > 1000.0 + 600:
        raise SystemExit("runaway scenario: ten fake minutes")


watch.time = types.SimpleNamespace(time=lambda: CLOCK["wall"], perf_counter=lambda: CLOCK["mono"],
                                   monotonic=lambda: CLOCK["mono"], sleep=sleep)
housekeeping.time = types.SimpleNamespace(time=lambda: CLOCK["wall"])
reputation.time = types.SimpleNamespace(time=lambda: CLOCK["wall"], localtime=time.gmtime,
                                        strftime=time.strftime)


class Fixed(datetime):
    @classmethod
    def now(cls, tz=None):
        return datetime.fromtimestamp(CLOCK["wall"], timezone.utc).replace(tzinfo=None)


watch.datetime = Fixed
W, H = 64, 40


def frame(i):
    """Frame i: made-up pixels the Rust test makes the same way."""
    y, x, c = np.meshgrid(np.arange(H), np.arange(W), np.arange(3), indexing="ij")
    f = ((i * 40 + y * 3 + x + c * 7) % 256).astype(np.uint8)
    f[0, 0, 0] = i
    return f


REC = {}


def reset():
    REC.clear()
    REC.update(said=[], status=[], alerts=[], events=[], submits=[], detects=[], released=0,
               summary=None, grabs=0, chats=[])


class Con:
    def say(self, text):
        REC["said"].append(text)

    def status(self, text):
        if not REC["status"] or REC["status"][-1] != text:
            REC["status"].append(text)

    def done(self):
        pass


class Alerts:
    def __init__(self, health):
        self.failures = health.get("failures", 0)
        self.repairs = health.get("repairs", 0)
        self.last_error = health.get("last_error", "")
        self.available = {"tones": True, "speech": health.get("speech", True)}
        if "expired" in health:
            self.expired = health["expired"]

    def event(self, kind, **kw):
        REC["alerts"].append([kind, kw])

    def burst_result(self, payload):
        REC["alerts"].append(json.loads(json.dumps(["burst", payload])))

    def close(self):
        REC["alerts"].append(["close"])


class Dbg:
    path = None

    def event(self, kind, **fields):
        # copied now: the session passes its live burst dicts, and the real
        # log writes each event the moment it is made
        REC["events"].append(json.loads(json.dumps([kind, fields])))

    def header(self, *a, **k):
        pass

    def close(self, summary=None):
        REC["summary"] = summary


class FakeQ:
    """Lookups still queued at shutdown: n at the first look, one fewer
    every `every` (fake) seconds after it."""

    def __init__(self, n, every):
        self.n0, self.every, self.t0 = n, every, None

    def qsize(self):
        if self.t0 is None:
            self.t0 = CLOCK["mono"]
        return max(0, self.n0 - int((CLOCK["mono"] - self.t0) / self.every))

    def empty(self):
        return self.qsize() == 0


class Enricher:
    def __init__(self, queued, every):
        import queue
        self.client = None if queued is None else object()
        self.q = queue.Queue() if queued is None else FakeQ(queued, every)

    def submit(self, handle, crops=None, range_km=None, votes=0, total=0, variants=None):
        # each crop as [key, height, width, sum of its bytes]
        held = [[k, int(v.shape[0]), int(v.shape[1]), int(v.astype(np.int64).sum())]
                for k, v in sorted((crops or {}).items())]
        REC["submits"].append([handle, held or None, range_km, votes, total,
                               [list(v) for v in (variants or [])]])

    def stop(self):
        pass


class Src:
    replay = False

    def __init__(self, ids, loop):
        self.ids, self.loop, self.i, self.exhausted = list(ids), loop, 0, False

    def grab(self):
        if self.i >= len(self.ids) and not self.loop:
            self.exhausted = True
            return None
        n = self.ids[self.i % len(self.ids)]
        self.i += 1
        REC["grabs"] += 1
        if SC.get("restart_at") is not None and REC["grabs"] == SC["restart_at"]:
            watch.RESTART_FLAG.touch()
        return frame(n)

    def release(self):
        REC["released"] += 1


SC: dict = {}


def pressed(self):
    n = REC["grabs"]
    if SC.get("stop_at") is not None and n >= SC["stop_at"]:
        # Ctrl+C, after `linger` more polls (0: at once, even mid-burst)
        REC["stop_polls"] = REC.get("stop_polls", 0) + 1
        if REC["stop_polls"] > SC.get("linger", 0):
            raise KeyboardInterrupt
    for at in SC.get("press", {}).get(self.spec, []):
        if at == n and (self.spec, at) not in SC["fired"]:
            SC["fired"].add((self.spec, at))
            return True
    return False


inputs.Binding.pressed = pressed
inputs.Binding.drain = lambda self: None
inputs.Binding.partial = lambda self: (0, len(self.tokens), [])


def detect(self, f, mode):
    i = int(f[0, 0, 0])
    REC["detects"].append([i, mode])
    out = []
    for c in CONTACTS.get(i, []):
        nb, rb = sd.Box(*c["nb"], 0), sd.Box(*c["rb"], 0)
        out.append(sd.Contact(name_box=nb, range_box=rb, range_km=c.get("km"), name=c["name"],
                              kind=c["kind"]))
    return out


watch.Session._detect = detect


def read_located(f, ocr, region=None):
    """chat.read_located: the lines scripted for this frame, and where."""
    i = int(f[0, 0, 0])
    REC["chats"].append([i, None if region is None else list(region)])
    got = SC.get("chat", {}).get(i)
    if got == "error":
        raise RuntimeError("the OCR fell over")
    lines, place = got if got else ([], chat.PLACE_BOTH if region is None else chat.PLACE_SET)
    return [chat.ChatLine(handle=h, text=t, conf=c) for h, t, c in lines], place


watch.chat.read_located = read_located


class Scan:
    def scan(self, f):
        return [tuple(p) for p in SC.get("scan", {}).get(int(f[0, 0, 0]), [])]


def execv(*a):
    raise OSError("simulated execv failure")


watch.os.execv = execv


def P(name, km, nb, rb=None, kind="player"):
    rb = rb or [nb[0], nb[1] + nb[3] + 2, nb[2], 6]
    return {"name": name, "km": km, "kind": kind, "nb": nb, "rb": rb}


KEY = ["--trigger", "key", "--key", "0", "--focus-key", "9", "--dual-key", "7", "--chat-key", "off",
       "--burst", "3", "--burst-spacing", "0.05", "--ping-delay", "0", "--reader", "iris",
       "--rsi-min-votes", "2"]
ONE, TWO, KNOWN = P("EXMPLONE", 2.5, [4, 4, 20, 6]), P("EXMPLTWO", 4.25, [30, 10, 24, 6]), \
    P("EXMPLKNOWN", 0.85, [10, 22, 18, 5])
CONTACTS = {
    1: [ONE, TWO, P("GOLD", 3.0, [40, 25, 10, 5], kind="world_label")],
    2: [ONE, P("EXMPLTW0", 4.25, [30, 10, 24, 6]), KNOWN],
    3: [ONE, TWO, P("EXMPLIGNORE", 9.0, [2, 30, 12, 4]), {"name": None, "km": 1.0, "kind": "unreadable",
                                                          "nb": [50, 2, 8, 4], "rb": [50, 8, 8, 4]},
        P("XY-9876-ZZ", 8.0, [44, 2, 14, 5], kind="ship_serial")],
    4: [P("EXMPLONEX", 2.5, [4, 4, 22, 6]), P("XY-1234-AB", 7.0, [44, 30, 14, 5], kind="ship_serial")],
    5: [P("EXMPLFAR", None, [4, 30, 16, 5])],
    6: [KNOWN], 7: [KNOWN, ONE], 8: [], 9: [TWO], 10: [ONE],
}
SCENARIOS = [
    ("key", KEY + ["--verbose", "--queue-max", "2"], {"ids": list(range(1, 11)), "loop": True,
     "press": {"0": [0, 1], "9": [6], "7": [9]}, "stop_at": 12, "linger": 30}),
    ("auto", ["--trigger", "auto", "--replay", "x", "--burst", "2", "--hz", "5", "--verbose",
              "--reader", "iris", "--rsi-min-votes", "1"],
     {"ids": [1, 2, 3, 4, 6, 7, 8, 9, 10], "loop": False,
      "scan": {1: [(4, 10, 20, 6), (30, 18, 24, 6)], 2: [(4, 10, 20, 6), (30, 18, 24, 6)],
               3: [(5, 11, 20, 6)], 4: [(4, 10, 22, 6), (44, 37, 14, 5)], 6: [(10, 29, 18, 5)],
               7: [(10, 29, 18, 5), (4, 10, 20, 6)], 9: [(30, 18, 24, 6)], 10: [(300, 300, 5, 5)]}}),
    ("no-db", KEY + ["--verbose", "--no-db"], {"ids": [1, 2, 3], "loop": True, "press": {"0": [0]},
                                                "stop_at": 3, "no_db": True,
                                                "no_alerts": True}),
    ("dual", KEY + ["--dual-on-read"], {"ids": [2, 6, 7], "loop": True, "press": {"0": [0]},
                                         "stop_at": 3}),
    ("ping", KEY + ["--ping-delay", "0.2", "--queue-max", "2"],
     {"ids": [1, 2, 3, 4, 5, 6], "loop": True, "press": {"0": [0, 1], "9": [0], "7": [0]},
      "stop_at": 7}),
    ("restart", KEY, {"ids": [1, 2, 3], "loop": True, "press": {"0": [0], "9": [3]},
                      "restart_at": 2}),
    ("seconds", KEY + ["--seconds", "1.0", "--verbose"],
     {"ids": [1], "loop": True, "press": {}, "cfg": {"housekeeping_enabled": True,
                                                       "housekeeping_every_min": 0.005}}),
    ("finish", KEY, {"ids": [6, 7, 1], "loop": True, "press": {"0": [0]}, "stop_at": 3,
                     "queued": 3, "health": {"failures": 2, "repairs": 1, "last_error": "engine died",
                                             "expired": 1, "speech": False}}),
    ("slow-finish", KEY, {"ids": [9], "loop": True, "press": {"0": [0]}, "stop_at": 3,
                          "queued": 40, "health": {"failures": 1}}),
    ("stuck-finish", KEY, {"ids": [10], "loop": True, "press": {"0": [0]}, "stop_at": 3,
                           "queued": 2, "drain_every": 5.0}),
    ("chat", KEY + ["--chat-key", "8"],
     {"ids": [11, 12, 13, 14, 1, 6, 7], "loop": True, "press": {"8": [0, 1, 2, 3], "0": [4]},
      "stop_at": 7,
      "chat": {11: ([["ExmplOne", "[GLOBAL] ExmplOne: o7", 0.91],
                     ["ExmplKnown", "[LOCAL] ExmplKnown: hi", 0.875],
                     ["ExmplIgnore", "[GLOBAL] ExmplIgnore: x", 0.5]], "helmet on"),
               13: ([["exmplignore", "[PARTY] exmplignore: y", 0.7]], "helmet off"),
               14: "error"}}),
    ("chat-region", KEY + ["--chat-key", "8", "--chat-region", "0.1,0.2,0.3,0.4"],
     {"ids": [15, 11], "loop": True, "press": {"8": [0, 1]}, "stop_at": 2,
      "chat": {11: ([["ExmplTwo", "(GL0BAL ExmplTwo : o7", 0.66]], "your region")}}),
    ("chat-no-db", KEY + ["--chat-key", "8", "--no-db"],
     {"ids": [11], "loop": True, "press": {"8": [0]}, "stop_at": 1, "no_db": True,
      "no_alerts": True,
      "chat": {11: ([["ExmplKnown", "[LOCAL] ExmplKnown: hi", 0.875]], "helmet on")}}),
    ("audits", KEY + ["--burst", "3", "--rsi-min-votes", "3"],
     {"ids": [8, 1, 8, 1, 8, 8, 8, 6, 1], "loop": True, "press": {"0": [0, 3, 6]}, "stop_at": 9,
      "cfg": {"audit_frame_quality": 70}, "seen_before": {"EXMPLTWO": 3}}),
]


def tables(path):
    if not path.exists():
        return None
    c = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    out = {}
    for t, sql in (("players", "SELECT handle, sightings FROM players ORDER BY handle"),
                   ("sightings", "SELECT handle, range_km, votes, total FROM sightings ORDER BY id"),
                   ("audits", "SELECT handle, range_km, votes, total, variants, reason, crop_name, "
                              "crop_range, frame_path FROM audits ORDER BY id")):
        out[t] = [list(r) for r in c.execute(sql)]
    c.close()
    return out


ARGS = ("trigger", "replay", "burst", "burst_spacing", "ping_delay", "queue_max", "min_votes",
        "rsi_min_votes", "verbose", "debug", "audit", "dual_on_read", "focus_always", "hz",
        "auto_read_delay", "seconds", "radius", "expire", "cooldown")
results = []
for name, argv, sc in SCENARIOS:
    reset()
    SC.clear()
    SC.update(sc, fired=set())
    CLOCK.update(wall=1_790_000_000.0, mono=1000.0)
    paths.DATA_ROOT = Path(tempfile.mkdtemp())
    watch.RESTART_FLAG = paths.data("restart.flag")
    args = watch.parse_args(argv)
    cfg = settings_mod.load()
    cfg.update({"housekeeping_enabled": False}, **sc.get("cfg", {}))
    key_mode = args.trigger == "key" and not args.replay
    binds = watch._parse_bindings(args, key_mode)
    db = None
    if not sc.get("no_db"):
        dbopen.forget()
        db = reputation.Database(paths.DATA_ROOT / "sc-watch.db")
        db.add_report("EXMPLKNOWN", "unprovoked_attack", "rammed us")
        db.add_report("EXMPLKNOWN", "camping", "")
        db.add_ignored("EXMPLIGNORE", "test")
        for h, n in sc.get("seen_before", {}).items():
            for _ in range(n):
                db.record_sighting(h, 1.0, 1, 1)
    log = watch.SightingsLog(paths.DATA_ROOT / "sightings.csv")
    src = Src(sc["ids"], sc["loop"])
    alerts = None if sc.get("no_alerts") else Alerts(sc.get("health", {}))
    session = watch.Session(args, cfg, src, None, None, alerts, Con(), db, Dbg(),
                            Enricher(sc.get("queued"), sc.get("drain_every", 0.5)), log, binds,
                            watch._parse_chat_region(args.chat_region), (None, None))
    session.iris_scan = Scan()
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
        rc = session.run()
    t = tables(paths.DATA_ROOT / "sc-watch.db")
    pictures = {}
    for a in (t or {}).get("audits", []):
        for rel in a[6:9]:
            if rel:
                img = imgio.imread(paths.DATA_ROOT / rel)
                pictures[rel] = (img.tolist() if rel.endswith(".png")
                                 else [int(img.shape[0]), int(img.shape[1])])
    results.append({
        "name": name, "argv": argv, "scenario": {k: v for k, v in sc.items()}, "rc": rc,
        "args": {k: getattr(args, k) for k in ARGS},
        "cfg": {k: cfg[k] for k in ("housekeeping_enabled", "housekeeping_every_min",
                                    "audit_keep_frame", "audit_frame_quality")},
        "said": REC["said"], "status": REC["status"], "printed": buf.getvalue().splitlines(),
        "alerts": json.loads(json.dumps(REC["alerts"])), "events": json.loads(json.dumps(REC["events"])),
        "submits": REC["submits"], "detects": REC["detects"], "grabs": REC["grabs"],
        "chats": REC["chats"], "chat_region": args.chat_region,
        "released": REC["released"], "summary": REC["summary"],
        "counters": [session.frames, session.triggers, session.announced],
        "tables": t, "pictures": pictures,
        "sightings": (paths.DATA_ROOT / "sightings.csv").read_text(encoding="utf-8"),
        "restart_flag_left": watch.RESTART_FLAG.exists(), "end": [CLOCK["wall"], CLOCK["mono"]],
    })
    if db is not None:
        db.close()

(HERE / "session_ops.json").write_text(json.dumps({
    "w": W, "h": H, "contacts": {str(k): v for k, v in CONTACTS.items()}, "scenarios": results,
}, indent=0, default=list), encoding="utf-8", newline="\n")
print(len(results), "scenarios;",
      ", ".join(f"{r['name']}: rc {r['rc']}, {len(r['said'])} said, {r['grabs']} grabs" for r in results))
