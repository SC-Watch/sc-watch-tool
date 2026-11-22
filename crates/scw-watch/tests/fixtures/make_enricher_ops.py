"""watch.RsiEnricher's own behaviour, for tests/enricher_ops.rs.

The real enricher, run on this thread instead of its own: its one blocking
call, _next(), is replaced by one that takes what is due at a fake time -
the retry heap first, then the queue - and moves the clock to the next
retry when there is nothing else. Everything else is the real code: the
retry ladders, the pause, expired copies, SENTRY checking, the misread
audit with its pictures, flags alerted once, org flags.

The lookup client is scripted per handle (each call takes the next answer;
the last one repeats); the console, alerts, session log and the cache's
failure notes are recorded; the database is a real, temporary sc-watch.db.
After each step: what was said, alerted, logged and noted, the enricher's
own state, the database's tables and the audit pictures.

    python make_enricher_ops.py      (sc-watch's build environment)

Made-up handles and orgs only; fixed times. Writes enricher_ops.json.
"""
import heapq
import json
import sqlite3
import sys
import tempfile
import time
import types
import urllib.error
from datetime import datetime
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import dbopen  # noqa: E402
import imgio  # noqa: E402
import reputation  # noqa: E402
import rsi  # noqa: E402
import watch  # noqa: E402

CLOCK = {"wall": 1_790_000_000.0, "mono": 1000.0}
AT = datetime(2026, 9, 21, 14, 3, 7)


class Fixed(datetime):
    @classmethod
    def now(cls, tz=None):
        return AT


watch.datetime = Fixed
watch.time = types.SimpleNamespace(time=lambda: CLOCK["wall"], monotonic=lambda: CLOCK["mono"])
rsi.time = types.SimpleNamespace(time=lambda: CLOCK["wall"], monotonic=lambda: CLOCK["mono"],
                                 mktime=__import__("calendar").timegm, strptime=time.strptime,
                                 sleep=lambda s: None)
reputation.time = types.SimpleNamespace(time=lambda: CLOCK["wall"], localtime=time.gmtime,
                                        strftime=time.strftime)


def advance(s):
    CLOCK["wall"] += s
    CLOCK["mono"] += s


# ---- the scripted client ------------------------------------------------------------

def citizen(spec):
    """An answer, as the fixture describes it, as rsi builds them."""
    if "error" in spec:
        kind = spec["error"]
        if kind == "http":
            e = urllib.error.HTTPError("x", spec["code"], "x", {}, None)
        elif kind == "down":
            e = rsi.ServiceDown(CLOCK["mono"] + spec["for"], spec.get("service"))
        elif kind == "timeout":
            e = TimeoutError("timed out")
        elif kind == "nokey":
            e = rsi.NoKey("no API key")
        else:
            raise AssertionError(kind)
        if spec.get("service") and kind != "down":
            rsi._tag(e, spec["service"])
        raise e
    c = rsi.Citizen(handle=spec["handle"], exists=spec.get("exists", True),
                    moniker=spec.get("moniker", ""), enlisted=spec.get("enlisted", ""),
                    orgs=[rsi.Org(**o) for o in spec.get("orgs", [])],
                    redacted=spec.get("redacted", False),
                    fetched_at=CLOCK["wall"])
    for k in ("stale_reason", "source", "awaiting_sentry", "checking", "retry_in"):
        if k in spec:
            setattr(c, k, spec[k])
    if "stored_days" in spec:
        c.stored_at = CLOCK["wall"] - spec["stored_days"] * 86400
    return c


SCRIPT: dict[str, list] = {}
NOTES: list = []
SAID: list = []
ALERTS: list = []
EVENTS: list = []
CALLS: list = []
DOWN = {"for": 0.0}


class Cache:
    def note_failure(self, handle, error, attempts, gave_up):
        NOTES.append([handle, error, attempts, bool(gave_up)])


class Client:
    enriches, has_key, provider = True, True, "api"
    sentry = object()
    cache = Cache()

    def citizen(self, handle):
        q = SCRIPT[handle]
        spec = q.pop(0) if len(q) > 1 else q[0]
        CALLS.append([handle, CLOCK["mono"]])
        return citizen(spec)

    def down_for(self):
        return DOWN["for"]


class Con:
    def say(self, text):
        SAID.append(text)

    def status(self, text):
        pass


class Alerts:
    def event(self, kind, **kw):
        ALERTS.append([kind, kw])


class Dbg:
    def event(self, kind, **fields):
        EVENTS.append([kind, fields])


def fake_next(self):
    """_next() without the wait: what is due now, else the queue, else the
    next retry (the clock moves to it), else nothing."""
    while True:
        now = CLOCK["mono"]
        if self._retry and self._retry[0][0] <= now:
            return heapq.heappop(self._retry)[2]
        if not self.q.empty():
            return self.q.get_nowait()
        if self._retry and self._retry[0][0] <= LIMIT["until"]:
            advance(self._retry[0][0] - now)
            continue
        return None


LIMIT = {"until": 0.0}
watch.RsiEnricher._next = fake_next

dbopen.forget()
db = reputation.Database()
db.flag_org("EXMPLORG", "they camp", "Example Org")
db.add_member_manually("EXMPLORG", "EXMPLGOOD", "seen with them")
watch.RsiEnricher.__init__.__defaults__  # noqa: B018 - the real constructor
en = watch.RsiEnricher(Con(), enabled=False, db=db, alerts=Alerts(), audit=True, dbg=Dbg())
en.client = Client()


def crop(h, w, seed):
    """A made-up crop the Rust test makes the same way: BGR, h x w."""
    y, x, c = np.meshgrid(np.arange(h), np.arange(w), np.arange(3), indexing="ij")
    return ((seed * 31 + y * 7 + x * 3 + c * 50) % 256).astype(np.uint8)


ORG = {"sid": "EXMPLORG", "name": "Example Org", "members": 40, "primary_focus": "Piracy",
       "is_main": True, "rank": "Boss"}
SIDE = {"sid": "EXMPLSIDE", "name": "Side", "members": 3, "primary_focus": "Trading"}
STEPS = [
    ("script", "EXMPLGOOD", [{"handle": "EXMPLGOOD", "moniker": "Good", "enlisted": "2026-09-01",
                              "orgs": [ORG, SIDE], "source": "sentry"}]),
    ("submit", "EXMPLGOOD", {"range_km": 2.5, "votes": 3, "total": 3}),
    ("submit", "EXMPLGOOD", {}),                      # already seen: ignored
    ("run", 0),
    ("script", "EXMPLGHOST", [{"handle": "EXMPLGHOST", "exists": False}]),
    ("submit", "EXMPLGHOST", {"crops": {"crop_name": [6, 20, 1], "crop_range": [5, 12, 2]},
                              "range_km": 4.0, "votes": 2, "total": 3,
                              "variants": [["EXMPLGH0ST", 1]]}),
    ("run", 0),
    ("script", "EXMPLFLAKY", [{"error": "http", "code": 500, "service": "SENTRY"},
                              {"error": "timeout", "service": "starcitizen-api.com"},
                              {"error": "http", "code": 502, "service": "SENTRY"},
                              {"handle": "EXMPLFLAKY"}]),
    ("submit", "EXMPLFLAKY", {"range_km": 1.0, "votes": 2, "total": 2}),
    ("run", 0),
    ("run", 20),
    ("run", 100),
    ("submit", "EXMPLFLAKY", {"range_km": 1.0, "votes": 2, "total": 2}),   # asks again
    ("run", 0),
    ("script", "EXMPLNOKEY", [{"error": "nokey"}]),
    ("submit", "EXMPLNOKEY", {}),
    ("run", 0),
    ("script", "EXMPLDOWN", [{"error": "down", "for": 30.0, "service": "SENTRY"},
                             {"handle": "EXMPLDOWN"}]),
    ("script", "EXMPLDOWN2", [{"error": "down", "for": 30.0, "service": "SENTRY"},
                              {"handle": "EXMPLDOWN2", "exists": False}]),
    ("submit", "EXMPLDOWN", {}),
    ("submit", "EXMPLDOWN2", {}),
    ("run", 0),
    ("run", 40),
    ("script", "EXMPLSTALE", [{"handle": "EXMPLSTALE", "orgs": [SIDE], "stored_days": 40,
                               "stale_reason": "SENTRY had an error (HTTP 500)"},
                              {"handle": "EXMPLSTALE", "orgs": [SIDE], "stored_days": 40,
                               "stale_reason": "SENTRY had an error (HTTP 500)"},
                              {"handle": "EXMPLSTALE", "orgs": [SIDE], "source": "sentry"}]),
    ("submit", "EXMPLSTALE", {"range_km": 3.0, "votes": 2, "total": 3}),
    ("run", 0),
    ("down", 100.0),            # longer than the retry it would wait: wins
    ("run", 30),
    ("down", 0.0),
    ("run", 100),
    ("script", "EXMPLWAIT", [{"handle": "EXMPLWAIT", "orgs": [ORG], "awaiting_sentry": True,
                              "stale_reason": "no org focus for EXMPLORG: not available, so whether it is a piracy org is unknown",
                              "stored_days": 0.5}]),
    ("submit", "EXMPLWAIT", {}),
    ("run", 0),
    ("run", 400),
    ("script", "EXMPLCHECK", [{"handle": "EXMPLCHECK", "exists": True, "checking": "queued",
                               "retry_in": 20.0, "stale_reason": "SENTRY is checking RSI for them"},
                              {"handle": "EXMPLCHECK", "checking": "queued_low", "retry_in": 120.0,
                               "stale_reason": "SENTRY is checking RSI for them, behind other new names"},
                              {"handle": "EXMPLCHECK", "checking": "queued", "retry_in": None,
                               "stale_reason": "SENTRY is checking RSI for them"},
                              {"handle": "EXMPLCHECK", "checking": "check_failed", "retry_in": 5.0,
                               "stale_reason": "SENTRY could not check RSI for them just now"}]),
    ("submit", "EXMPLCHECK", {"crops": {"crop_name": [4, 9, 3]}, "range_km": 6.0, "votes": 1,
                              "total": 1}),
    ("run", 0),
    ("run", 1000),
    ("script", "EXMPLCHECK", [{"handle": "EXMPLCHECK", "exists": False}]),
    ("submit", "EXMPLCHECK", {"crops": {"crop_name": [4, 9, 3]}, "range_km": 6.0, "votes": 1,
                              "total": 1}),
    ("run", 0),
    ("script", "EXMPLFLAG2", [{"handle": "EXMPLFLAG2", "orgs": [ORG], "stored_days": 2,
                               "stale_reason": "the lookup service timed out"},
                              {"handle": "EXMPLFLAG2", "orgs": [ORG, {**SIDE, "primary_focus": "Smuggling"}],
                               "enlisted": "2026-09-20"}]),
    ("submit", "EXMPLFLAG2", {"range_km": 0.7, "votes": 2, "total": 2}),
    ("run", 0),
    ("run", 30),
    # an expired copy first keeps the crops for the refresh: no such account
    ("script", "EXMPLLAPSED", [{"handle": "EXMPLLAPSED", "stored_days": 31,
                                "stale_reason": "SENTRY had an error (HTTP 500)"},
                               {"handle": "EXMPLLAPSED", "exists": False}]),
    ("submit", "EXMPLLAPSED", {"crops": {"crop_name": [3, 5, 4], "crop_range": [3, 4, 5]},
                               "range_km": 9.0, "votes": 1, "total": 2}),
    ("run", 0),
    ("run", 20),
    ("crowd", 66),
    ("run", 0),
]


def png_pixels(rel):
    img = imgio.imread(paths.DATA_ROOT / rel)
    return None if img is None else img.tolist()


def tables():
    c = sqlite3.connect(f"file:{paths.DATA_ROOT / 'sc-watch.db'}?mode=ro", uri=True)
    out = {}
    for t, sql in (("audits", "SELECT handle, range_km, votes, total, variants, reason, crop_name, "
                              "crop_range, frame_path FROM audits ORDER BY id"),
                   ("player_orgs", "SELECT handle, sid, is_main, source FROM player_orgs ORDER BY handle, sid"),
                   ("players", "SELECT handle, moniker, enlisted FROM players ORDER BY handle")):
        out[t] = [list(r) for r in c.execute(sql)]
    c.close()
    return out


steps = []
for i, step in enumerate(STEPS):
    op = step[0]
    SAID.clear(), ALERTS.clear(), EVENTS.clear(), NOTES.clear(), CALLS.clear()
    if op == "script":
        SCRIPT[step[1]] = list(step[2])
    elif op == "submit":
        kw = dict(step[2])
        crops = kw.pop("crops", None)
        made = {k: crop(*v) for k, v in crops.items()} if crops else None
        en.submit(step[1], made, kw.get("range_km"), kw.get("votes", 0), kw.get("total", 0),
                  kw.get("variants"))
    elif op == "run":
        LIMIT["until"] = CLOCK["mono"] + step[1]
        en._run()
        if CLOCK["mono"] < LIMIT["until"]:
            advance(LIMIT["until"] - CLOCK["mono"])
    elif op == "down":
        DOWN["for"] = step[1]
    elif op == "crowd":
        # More handles with crops than the enricher holds: the oldest go.
        for n in range(step[1]):
            h = f"EXMPLCROWD{n:02d}"
            SCRIPT[h] = [{"handle": h, "exists": n != 0}]
            en.submit(h, {"crop_name": crop(2, 3, 100 + n)}, 1.0, 1, 1, None)
    t = tables()
    steps.append({
        "op": op, "args": list(step[1:]), "mono": CLOCK["mono"], "wall": CLOCK["wall"],
        "said": list(SAID), "alerts": json.loads(json.dumps(ALERTS, default=list)),
        "events": json.loads(json.dumps(EVENTS, default=list)), "notes": list(NOTES),
        "calls": list(CALLS),
        "state": {"seen": sorted(en.seen), "retry": sorted([d, h] for d, _, h in en._retry),
                  "attempts": dict(sorted(en.attempts.items())), "pending": list(en.pending),
                  "stale_shown": sorted(en._stale_shown), "check_shown": sorted(en._check_shown),
                  "announced_pause": en._announced_pause,
                  "alerted": {k: sorted(v) for k, v in sorted(en._alerted.items())}},
        "tables": t,
        "pictures": {rel: png_pixels(rel) for a in t["audits"] for rel in a[6:9] if rel},
    })

(HERE / "enricher_ops.json").write_text(json.dumps({
    "wall0": 1_790_000_000.0, "mono0": 1000.0, "at": AT.isoformat(), "steps": steps,
}, indent=0), encoding="utf-8", newline="\n")
print(len(steps), "steps,", sum(len(s["calls"]) for s in steps), "lookups,",
      sum(len(s["said"]) for s in steps), "lines said")
