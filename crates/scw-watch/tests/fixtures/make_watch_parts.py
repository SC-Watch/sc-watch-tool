"""watch.py's and debuglog.py's small pieces, for tests/watch_parts.rs: the
contact tracker, the announcer, the 'waiting for' label, the binding, chat
region and focus size checks, the sightings CSV, and the session log's
event and summary lines.

    python make_watch_parts.py      (sc-watch's build environment)

Made-up handles only. The log's scrub is given a made-up home folder and
user name; times are fixed. Writes watch_parts.json.
"""
import io
import json
import os
import sys
import tempfile
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
HOME, USER = r"C:\Users\Exmpl Person", "exmplperson"
os.environ["USERPROFILE"] = HOME
os.environ["HOME"] = HOME
os.environ["USERNAME"] = USER
os.environ.pop("USER", None)
import debuglog  # noqa: E402
import consensus  # noqa: E402
import inputs  # noqa: E402
import watch  # noqa: E402

AT = datetime(2026, 9, 21, 14, 3, 7, 123456)


class Fixed(datetime):
    @classmethod
    def now(cls, tz=None):
        return AT


debuglog.datetime = Fixed
watch.datetime = Fixed

# ---- the tracker ---------------------------------------------------------------
TRACK = [
    ([(100, 100)], 0.0), ([(110, 105)], 1.0), ([(400, 100), (110, 105)], 2.0),
    ([(100, 100), (279, 100), (281, 100)], 3.0), ([], 10.0), ([(400, 100)], 22.9),
    ([(400, 100)], 23.0), ([(100.5, 100.5)], 24.0), ([(0, 0), (0, 0)], 50.0),
    # exactly the radius away is NOT the same contact; equidistant, the first track
    ([(500, 500)], 80.0), ([(680, 500)], 81.0), ([(590, 500)], 82.0),
]
tr = watch.Tracker(180.0, 20.0)
tracker = []
for pts, now in TRACK:
    n = tr.update(pts, now)
    tracker.append({"points": pts, "now": now, "new": n,
                    "tracks": [[t.cx, t.cy, t.last_seen] for t in tr.tracks]})

# ---- the announcer ----------------------------------------------------------------
NAMES = ["EXMPLHORNE", "EXMPLHORNET65", "EXMPLHORNE", "EXMPLH0RNET65", "EXMPLSLIVER",
         "EXMPL5LIVER", "EXMPLHORNET65X", "OTHERNAME", "OTHERNAM", "EXMPLSLIVERZZ", "AB"]
an = watch.Announcer(300.0)
announcer = []
for i, n in enumerate(NAMES):
    ok, was = an.should(n, float(i))
    announcer.append({"name": n, "ok": ok, "was": was, "counts": dict(an.counts),
                      "known": list(an.last)})

# ---- bindings and labels ----------------------------------------------------------
class A:
    pass


BINDS = [
    ("0", "9", "off", "off", True), ("0", "off", "off", "off", True),
    ("rctrl+joy0.b24", "rctrl+joy0.b25", "rctrl+joy0.b26", "off", True),
    ("lshift+rctrl+joy0.b1", "rctrl+lshift+joy0.b2", "off", "8", True),
    ("off", "9", "off", "off", True), ("off", "9", "off", "off", False),
    ("0", "0", "off", "off", True), ("0", "nonsense+key", "off", "off", True),
    ("f9", "rctrl+f9", "f9+rctrl", "off", True), ("tab", "joy1.b3", "vid231d:0126.b7", "f10", True),
    ("", "", "", "", False),
    ("rctrl+lshift+joy0.b1", "rctrl+lshift+joy0.b2", "off", "off", True),
]
bindings = []
for key, focus, dual, chat, key_mode in BINDS:
    a = A()
    a.key, a.focus_key, a.dual_key, a.chat_key = key, focus, dual, chat
    try:
        b = watch._parse_bindings(a, key_mode)
        bindings.append({"specs": [key, focus, dual, chat], "key_mode": key_mode,
                         "labels": {m: x.label() for m, x in b.items()},
                         "waiting": watch._waiting_label(b)})
    except watch.ConfigError as e:
        bindings.append({"specs": [key, focus, dual, chat], "key_mode": key_mode,
                         "error": str(e)})

REGIONS = ["auto", "", "  AUTO ", "0.15,0.30,0.35,0.45", "0,0,1,1", "0.1,0.2,0.3",
           "0.1,0.2,0.3,1.5", "a,b,c,d", " 0.1, 0.2 ,0.3,0.4", "1e-1,0,0,0", "0.1,,0.2,0.3"]
regions = []
for s in REGIONS:
    try:
        regions.append({"spec": s, "ok": watch._parse_chat_region(s)})
    except watch.ConfigError as e:
        regions.append({"spec": s, "error": str(e)})
SIZES = ["auto", "", "AUTO", "1200x600", "1200X600", " 800 x 400 ", "800x", "x", "8x8x8",
         "-5x10", "1.5x2", "big"]
sizes = []
for s in SIZES:
    try:
        sizes.append({"spec": s, "ok": list(watch._parse_focus_size(s))})
    except watch.ConfigError as e:
        sizes.append({"spec": s, "error": str(e)})

# ---- the sightings log --------------------------------------------------------------
csvp = paths.DATA_ROOT / "sightings.csv"
log = watch.SightingsLog(csvp)
ROWS = [("EXMPLONE", 1.25, 3, 3), ("EXMPLTWO", 12.35, 1, 3), ("EXMPL,COMMA", 0.05, 2, 3),
        ("EXMPL\"Q", 100.0, 2, 2), ("EXMPLSIX", 2.675, 2, 6)]
for name, km, v, t in ROWS:
    log.write(consensus.Resolved(name, km, v, t, []))
log.close()
log2 = watch.SightingsLog(csvp)                  # appends, no second header
log2.write(consensus.Resolved("EXMPLAGAIN", 3.0, 1, 1, []))
log2.close()
sightings = csvp.read_bytes().decode("utf-8")

# ---- the session log -------------------------------------------------------------------
dl = debuglog.DebugLog(enabled=True, tag="")
EVENTS = [
    ("read", {"file": "", "idx": 0, "mode": "full", "named": 2, "unreadable": 0,
              "discarded": None, "got": [("EXMPLONE", 1.25), ("EXMPLTWO", None)]}),
    ("burst", {"label": "reading", "mode": "full", "frames": 3, "per_frame": [2, 1, 2],
               "result": [("EXMPLONE", 3, 3, 1.25)]}),
    ("audit", {"handle": "EXMPLONE", "reason": "weak_read", "votes": 1, "total": 3, "seen": 0}),
    ("note", {"text": f"saved to {HOME}\\data and {HOME.replace(chr(92), '/')}/x"}),
    ("rsi_failed", {"handle": "EXMPLTWO", "why": "api_key=abc123 refused, token: xyz",
                    "attempt": 1, "retry": True}),
    ("chat", {"handles": [], "region": "auto", "place": "both", "ignored": True}),
    ("discards", {"discarded": {"world_label": 2, "serial": 1}, "pi": 3.141592653589793,
                  "big": 10 ** 19, "neg": -0.0, "uni": "café ✓"}),
    ("user", {"who": "ExmplPerson said hi", "path": "D:/Users/Other/exmplperson.txt"}),
    ("start", {}),
]
for kind, fields in EVENTS:
    dl.event(kind, **fields)
dl.note("a plain note")
dl.exception("burst", ValueError("bad frame " + "x" * 400))
dl.close({"minutes": 1.5, "frames": 10, "triggers": 2, "announced": 3,
          "audio_last_error": "-", "audio_available": {"tones": True, "speech": False},
          "contacts": {"EXMPLONE": 2, "EXMPLTWO": 1}})
lines = dl.path.read_text(encoding="utf-8").splitlines()
events_at = lines.index("-- events") if "-- events" in lines else -1

(HERE / "watch_parts.json").write_text(json.dumps({
    "home": HOME, "user": USER, "at": AT.isoformat(),
    "tracker": tracker, "announcer": announcer, "bindings": bindings,
    "key_names": sorted(inputs.VK_CODES)[:18],
    "regions": regions, "sizes": sizes, "rows": ROWS, "sightings": sightings,
    "events": [[k, f] for k, f in EVENTS], "log": lines[events_at + 1:] if events_at >= 0 else lines,
    "log_name": dl.path.name,
}, indent=0, ensure_ascii=False), encoding="utf-8", newline="\n")
print(len(tracker), "tracker steps,", len(announcer), "announcements,", len(bindings),
      "bindings,", len(regions) + len(sizes), "specs,", len(lines), "log lines")
