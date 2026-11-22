"""The scripted network and fake clock both fixture generators run rsi.py on.

Every request either service is sent is answered from ROUTES - a 200 with
a JSON body and headers, an HTTP error with headers, a timeout, a refused
connection, or a body that is not JSON - and recorded in SENT with its URL,
timeout and when it was sent. Sleeping moves CLOCK, so pacing, pauses and
waits are exact and instant; jitter is the midpoint. Only rsi's view of
time and chance is faked. Importing this points sc-watch at a temporary
data folder and drops any SC_API_KEY first.
"""
import calendar
import email.message
import json
import os
import sqlite3
import sys
import tempfile
import time
import types
import urllib.error
from dataclasses import asdict, fields
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())      # never the real data folder
os.environ.pop("SC_API_KEY", None)              # never a real key
import dbopen  # noqa: E402
import rsi  # noqa: E402

KEY = "EXMPLKEY"
CLOCK = {"wall": 1_790_000_000.0, "mono": 5000.0}


def _sleep(s):
    if s > 0:
        CLOCK["wall"] += s
        CLOCK["mono"] += s


# Only rsi's view of time and chance is faked.
rsi.time = types.SimpleNamespace(time=lambda: CLOCK["wall"], monotonic=lambda: CLOCK["mono"],
                                 sleep=_sleep, mktime=calendar.timegm, strptime=time.strptime)
rsi.random = types.SimpleNamespace(uniform=lambda a, b: (a + b) / 2)


class _Now:
    @staticmethod
    def now():
        return datetime.fromtimestamp(CLOCK["wall"], timezone.utc).replace(tzinfo=None)


rsi.datetime = _Now
rsi.PROCESS = "test"

ROUTES: dict[str, list] = {}
SENT: list = []


class Unscripted(BaseException):
    """Not an Exception, so rsi.py's own handlers cannot swallow it."""


def respond(key, timeout):
    q = ROUTES.get(key)
    if not q:
        raise Unscripted(key)
    r = q.pop(0) if len(q) > 1 else q[0]
    SENT.append({"url": key, "timeout": timeout, "at": CLOCK["wall"]})
    _sleep(r.get("took", 0.25))
    hdrs = email.message.Message()
    for k, v in (r.get("headers") or {}).items():
        hdrs[k] = v
    if "http" in r:
        raise urllib.error.HTTPError(key, r["http"], "x", hdrs, None)
    err = r.get("error")
    if err == "timeout":
        raise TimeoutError("timed out")
    if err == "connect":
        raise urllib.error.URLError("refused")
    if err == "badjson":
        raise json.JSONDecodeError("Expecting value", "<", 0)
    return r["json"], hdrs


def sentry_fetch(self, url, timeout):
    payload, hdrs = respond("sentry:" + url[len(rsi.SENTRY_HOST):], timeout)
    rsi._TL.headers = hdrs
    return payload


def sci_fetch(self, url, timeout=25):
    prefix = f"{rsi.API_HOST}/{self.api_key}/v1/"
    assert url.startswith(prefix), url
    return respond("sci:" + url[len(prefix):], timeout)[0]


rsi.Sentry._fetch = sentry_fetch
rsi.Client._fetch = sci_fetch


# ---- answers --------------------------------------------------------------------

def iso(days):
    t = CLOCK["wall"] - days * 86400
    return datetime.fromtimestamp(t, timezone.utc).replace(tzinfo=None).isoformat()


def player(h, days=1.0, orgs=(), display=None, enlisted="2015-03-01T10:00:00.000000",
           record=12345, location=" Stanton ", shown=None, redirected=None, archived=False,
           headers=None):
    prof = {"handle": shown or h, "display_name": h.title() if display is None else display,
            "enlisted": enlisted, "citizen_record": record, "location": location}
    meta = {"last_fetched": iso(days)}
    if archived:
        meta["is_archived"] = True
    d = {"profile": prof, "metadata": meta,
         "memberships": [{"organization_sid": sid, "is_main": main, "member_rank": rank,
                          "visibility": vis} for sid, main, rank, vis in orgs]}
    if redirected:
        d["redirected_from"] = redirected
    r = {"json": d}
    if headers:
        r["headers"] = headers
    return r


def stub(h, state=None, hint=None):
    meta = {"is_stub": True, "finalized": False}
    if state:
        meta["lookup_state"] = state
    if hint:
        meta["retry_after_hint"] = hint
    return {"json": {"profile": {"handle": h, "is_stub": True}, "metadata": meta, "memberships": []}}


def meta(days):
    return {"json": {"last_fetched": iso(days)}}


def oprof(name, members, prim=None, sec=None):
    return {"json": {"profile": {"name": name, "member_count": members,
                                 "primary_activity": prim, "secondary_activity": sec}}}


def sci_user(display, enlisted="2016-05-01", rid="#777", org=None):
    return {"json": {"success": 1, "data": {"profile": {"display": display, "enlisted": enlisted,
                                                        "id": rid},
                                            "organization": org if org is not None else {"stars": 0}}}}


def sci_org(name, members=10, arch="", commit="", prim="", sec=""):
    return {"json": {"success": 1, "data": {"name": name, "members": members, "archetype": arch,
                                            "commitment": commit,
                                            "focus": {"primary": {"name": prim} if prim else {},
                                                      "secondary": {"name": sec} if sec else {}}}}}


def sci_none():
    return {"json": {"success": 0, "data": None}}


def http(code, **headers):
    return {"http": code, "headers": {k.replace("_", "-"): v for k, v in headers.items()}}


NF = {"X-Lookup-State": "not_found"}
NC = {"X-Lookup-State": "not_checked"}
TTL = rsi.CITIZEN_TTL
FRESH = rsi.FRESH_WINDOW


def c_url(h, max_age=TTL, live=True):
    q = [f"max_age={max_age}"] if max_age is not None else []
    if not live:
        q.append("scrape=false")
    return f"sentry:/citizens/{h}" + ("?" + "&".join(q) if q else "")


def o_url(sid, max_age=None):
    return f"sentry:/orgs/{sid}/profile" + ("" if max_age is None else f"?max_age={max_age}")


def u_url(h):
    return f"sci:auto/user/{h}"


def org_url(sid):
    return f"sci:auto/organization/{sid}"


