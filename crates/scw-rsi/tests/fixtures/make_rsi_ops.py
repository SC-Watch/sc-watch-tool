"""rsi.py's own behaviour on a script of lookups, for tests/rsi_ops.rs.

The network is scripted: every request either service is sent is answered
from ROUTES (a 200 with a JSON body and headers, an HTTP error with headers,
a timeout, a refused connection or a body that is not JSON) and recorded -
its URL, timeout and when it was sent. The clock is fake: sleeping moves it,
so pacing, pauses and waits are exact and instant. Jitter is the midpoint.
After each step: the return value (or the error, its wording and whether it
is transient), the requests sent, the SENTRY request-log lines written, the
pacing state and every table of rsi_cache.db.

    python make_rsi_ops.py      (sc-watch's build environment)

Made-up handles and orgs only; a made-up API key; fixed times. Writes
rsi_ops.json.
"""
import email.message
import json
import sqlite3
import sys
import tempfile
from dataclasses import asdict, fields
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from fakenet import *  # noqa: E402,F401,F403
from fakenet import _sleep  # noqa: E402


# ---- the script -------------------------------------------------------------------
# (op, args, kwargs, routes as a function - evaluated at that step's time)

def S(op, *args, routes=None, why=None, **kwargs):
    return (op, list(args), kwargs, routes, why)


STEPS = [
    S("client", "A", api_key=KEY, sentry=True),
    # a whole SENTRY profile: two orgs with focus, one hidden membership
    S("citizen", "EXMPLONE", routes=lambda: {
        c_url("EXMPLONE"): [player("EXMPLONE", 1.0, [
            ("EXMPLORG", True, "Boss", "visible"), ("EXMPLHID", False, "", "hidden"),
            ("EXMPLTWOORG", False, "Grunt", "visible")])],
        o_url("EXMPLORG"): [oprof("Example Org", 40, "Piracy", "Smuggling")],
        org_url("EXMPLORG"): [sci_org("Example Org", 40, "Syndicate", "Casual", "Piracy")],
        o_url("EXMPLTWOORG"): [oprof("Two Org", 7, "Trading", "Trading")],
        org_url("EXMPLTWOORG"): [sci_none()]}),
    S("citizen", "exmplone "),                   # from the cache: nothing sent
    S("exists", "EXMPLONE"),
    # an org SENTRY has no focus for: re-read asked, the fallback fails
    S("citizen", "EXMPLNOFOCUS", routes=lambda: {
        c_url("EXMPLNOFOCUS"): [player("EXMPLNOFOCUS", 2.0, [("EXMPLQUIET", True, "", "visible")])],
        o_url("EXMPLQUIET"): [oprof("Quiet", 3)],
        o_url("EXMPLQUIET", 0): [oprof("Quiet", 3, "", " ")],
        org_url("EXMPLQUIET"): [http(500)],
        u_url("EXMPLNOFOCUS"): [sci_user("Nofocus", org={"sid": "EXMPLQUIET", "rank": "Member"})]}),
    S("advance", 120.0),
    S("complete_partials", True),                # asked; still no focus
    S("complete_partials", True),                # not due yet
    S("advance", 700.0),
    S("complete_partials", True, routes=lambda: {
        o_url("EXMPLQUIET"): [oprof("Quiet", 3, "Piracy")]}),
    # too old, not fresh: the old copy stands in, marked
    S("citizen", "EXMPLOLD", routes=lambda: {c_url("EXMPLOLD"): [player("EXMPLOLD", 40.0)]}),
    # fresh: waits for the re-read, which lands on the second check
    S("citizen", "EXMPLOLD", fresh=True, routes=lambda: {
        c_url("EXMPLOLD", FRESH): [player("EXMPLOLD", 40.0)],
        "sentry:/citizens/EXMPLOLD/metadata": [meta(40.0), meta(0.0)],
        c_url("EXMPLOLD"): [player("EXMPLOLD", -18 / 86400, enlisted="Mar 3, 2026")]}),
    S("citizen", "EXMPLGONE", routes=lambda: {c_url("EXMPLGONE"): [http(404, X_Lookup_State="not_found")]}),
    S("citizen", "EXMPLNEWBIE", routes=lambda: {
        c_url("EXMPLNEWBIE"): [stub("EXMPLNEWBIE", "queued", 20)],
        u_url("EXMPLNEWBIE"): [sci_user("Newbie", enlisted="2026-11-01")]}),
    S("citizen", "EXMPLFAILED", routes=lambda: {
        c_url("EXMPLFAILED"): [stub("EXMPLFAILED", "check_failed", 120)],
        u_url("EXMPLFAILED"): [http(404)]}),
    S("client", "B", api_key=None, sentry=True),
    S("citizen", "EXMPLWAIT", routes=lambda: {c_url("EXMPLWAIT"): [stub("EXMPLWAIT", "queued_low", 25)]}),
    S("citizen", "EXMPLWAIT", fresh=True, routes=lambda: {
        c_url("EXMPLWAIT", FRESH): [stub("EXMPLWAIT", "queued", 25)],
        c_url("EXMPLWAIT"): [player("EXMPLWAIT", 0.0, record="", location=None)]}),
    S("citizen", "EXMPLSLOW", fresh=True, routes=lambda: {
        c_url("EXMPLSLOW", FRESH): [stub("EXMPLSLOW", "check_failed", 40)]}),
    S("citizen", "EXMPLSILENT", routes=lambda: {c_url("EXMPLSILENT"): [http(404, X_Lookup_State="not_checked")]}),
    S("exists", "EXMPLSILENT", strict=True),     # remembered: nothing sent
    S("exists", "EXMPLSILENT"),
    S("exists", "EXMPLGHOST", routes=lambda: {
        c_url("EXMPLGHOST", None, False): [http(404, X_Lookup_State="not_found")]}),
    S("exists", "EXMPLLATER", routes=lambda: {
        c_url("EXMPLLATER", None, False): [http(404, X_Lookup_State="not_checked")]}),
    S("citizen", "EXMPLLATER", why="lookup", routes=lambda: {
        # the main org listed after an affiliate: it goes first
        c_url("EXMPLLATER"): [player("EXMPLLATER", 3.0, [("EXMPLTWOORG", False, "Grunt", "visible"),
                                                          ("EXMPLORG", True, "Boss", "visible")])]}),
    S("use", "A"),
    S("exists", "EXMPLREDIR", routes=lambda: {
        c_url("EXMPLREDIR", None, False): [player("EXMPLREDIR", 1.0, shown="EXMPLNEWNAME",
                                                  redirected="EXMPLREDIR")],
        u_url("EXMPLREDIR"): [http(404)]}),
    S("citizen", "EXMPLARCH", routes=lambda: {
        c_url("EXMPLARCH"): [player("EXMPLARCH", 9.0, archived=True)],
        u_url("EXMPLARCH"): [http(500)]}),
    # the other service's breaker: three in a row, then paused; the probe
    S("citizen", "EXMPLT1", routes=lambda: {c_url("EXMPLT1"): [http(404)],
                                            u_url("EXMPLT1"): [{"error": "timeout", "took": 25.0}]}),
    S("citizen", "EXMPLT2", routes=lambda: {c_url("EXMPLT2"): [http(404)],
                                            u_url("EXMPLT2"): [{"error": "connect"}]}),
    S("citizen", "EXMPLT3", routes=lambda: {c_url("EXMPLT3"): [http(404)],
                                            u_url("EXMPLT3"): [sci_user("T3")]}),
    S("advance", 70.0),
    S("citizen", "EXMPLT4", routes=lambda: {c_url("EXMPLT4"): [http(404)],
                                            u_url("EXMPLT4"): [{"error": "badjson"}]}),
    S("citizen", "EXMPLT5", routes=lambda: {c_url("EXMPLT5"): [http(404)],
                                            u_url("EXMPLT5"): [http(503, Retry_After="30")]}),
    S("advance", 200.0),
    S("citizen", "EXMPLT6", fresh=True, routes=lambda: {
        c_url("EXMPLT6", FRESH): [http(404)],
        u_url("EXMPLT6"): [sci_user("T6", org={"name": "", "stars": 0})]}),
    # SENTRY's live limit: only live asks stop, and the pause is kept
    S("citizen", "EXMPLLIM1", routes=lambda: {
        c_url("EXMPLLIM1"): [http(429, Retry_After="3600", RateLimit='"live";r=0;t=3600')],
        c_url("EXMPLLIM1", TTL, False): [http(404)],
        u_url("EXMPLLIM1"): [sci_user("Lim1", org={"sid": "EXMPLORG", "rank": "Recruit"})]}),
    S("citizen", "EXMPLLIM2", routes=lambda: {
        c_url("EXMPLLIM2", TTL, False): [http(404)],
        u_url("EXMPLLIM2"): [http(500)]}),
    S("org", "EXMPLNEWORG", routes=lambda: {
        o_url("EXMPLNEWORG"): [http(429, Retry_After="3500")],
        org_url("EXMPLNEWORG"): [sci_org("New Org", 12, "Organization", "Regular", "Security", "Medical")]}),
    S("org", "EXMPLNEWORG2", routes=lambda: {o_url("EXMPLNEWORG2"): [http(404)],
                                             org_url("EXMPLNEWORG2"): [sci_org("New Org 2", 2)]}),
    S("org", "EXMPLNEWORG"),                     # cached now: nothing sent
    S("exists", "EXMPLGONE"),                    # a stored "no such account"
    S("client", "C", api_key=None, sentry=True),     # sees the stored live pause
    S("citizen", "EXMPLBUDGET", routes=lambda: {
        c_url("EXMPLBUDGET", TTL, False): [http(429, Retry_After="37")]}),
    S("citizen", "EXMPLBUDGET2"),                    # refused unsent: SENTRY paused
    S("citizen", "EXMPLBUDGET2", fresh=True),        # told to wait: try now too
    S("advance", 3700.0),
    S("citizen", "EXMPLRATE1", routes=lambda: {
        c_url("EXMPLRATE1"): [player("EXMPLRATE1", 1.0, headers={"RateLimit": '"default";r=4;t=20'})]}),
    S("citizen", "EXMPLRATE2", routes=lambda: {
        c_url("EXMPLRATE2"): [player("EXMPLRATE2", 1.0, headers={"RateLimit-Remaining": "0",
                                                                 "RateLimit-Reset": "30"})]}),
    S("citizen", "EXMPLRATE3", fresh=True),
    S("advance", 40.0),
    S("citizen", "EXMPLJUNK", routes=lambda: {c_url("EXMPLJUNK"): [{"json": ["not", "a", "profile"]}]}),
    S("citizen", "EXMPLJUNK2", routes=lambda: {c_url("EXMPLJUNK2"): [{"error": "badjson"}]}),
    # SENTRY's own breaker, apart from the other service's
    S("client", "E", api_key=KEY, sentry=True),
    S("citizen", "EXMPLS1", routes=lambda: {c_url("EXMPLS1"): [{"error": "timeout", "took": 8.0}],
                                            u_url("EXMPLS1"): [sci_user("S1")]}),
    S("citizen", "EXMPLS2", routes=lambda: {c_url("EXMPLS2"): [{"error": "timeout", "took": 8.0}],
                                            u_url("EXMPLS2"): [sci_user("S2")]}),
    S("citizen", "EXMPLS3", routes=lambda: {c_url("EXMPLS3"): [http(502)],
                                            u_url("EXMPLS3"): [sci_user("S3")]}),
    S("citizen", "EXMPLS4", routes=lambda: {u_url("EXMPLS4"): [sci_user("S4")]}),
    S("exists", "EXMPLS5", routes=lambda: {u_url("EXMPLS5"): [http(404)]}),
    # rosters
    S("use", "A"),
    S("roster", "EXMPLORG", routes=lambda: {
        "sentry:/orgs/EXMPLORG/memberships?per_page=100&page=1": [{"json": {
            "memberships": {"pages": 2, "total": 7, "members": [
                {"citizen_handle": "EXMPLMEMBER", "display_name": "Member", "member_rank": "Recruit",
                 "rank_number": 1, "visibility": "visible"},
                {"citizen_handle": "EXMPLONE", "display_name": "One", "member_rank": "Boss",
                 "rank_number": 5},
                {"citizen_handle": "[HIDDEN]", "visibility": "hidden"},
                {"citizen_handle": "[REDACTED]", "visibility": "visible"}]},
            "metadata": {"last_fetched": iso(2.0)}}}],
        "sentry:/orgs/EXMPLORG/memberships?per_page=100&page=2": [{"json": {
            "memberships": {"pages": 2, "total": 7, "members": [
                {"citizen_handle": " EXMPLTHIRD ", "display_name": None, "member_rank": None,
                 "rank_number": None, "visibility": "Visible"},
                {"citizen_handle": "EXMPLX", "visibility": "redacted"}]}}}]}),
    S("roster", "EXMPLSMALL", routes=lambda: {
        "sentry:/orgs/EXMPLSMALL/memberships?per_page=100&page=1": [http(404)],
        "sci:live/organization_members/EXMPLSMALL?page=1": [{"json": {"success": 1, "data": [
            {"handle": "EXMPLP1", "display": "P1", "rank": "R", "stars": 2}, {"display": "nohandle"}]}}],
        "sci:live/organization_members/EXMPLSMALL?page=2": [{"json": {"success": 1, "data": [
            {"handle": "EXMPLP2"}]}}],
        "sci:live/organization_members/EXMPLSMALL?page=3": [{"json": {"success": 1, "data": []}}]}),
    S("roster", "EXMPLHALF", routes=lambda: {
        "sentry:/orgs/EXMPLHALF/memberships?per_page=100&page=1": [{"json": {
            "memberships": {"pages": 1, "total": 0, "members": []}}}],
        "sci:live/organization_members/EXMPLHALF?page=1": [{"json": {"success": 1, "data": [
            {"handle": "EXMPLH1"}]}}],
        "sci:live/organization_members/EXMPLHALF?page=2": [sci_none()]}),
    S("use", "B"),
    S("roster", "EXMPLBROKEN", routes=lambda: {
        "sentry:/orgs/EXMPLBROKEN/memberships?per_page=100&page=1": [{"json": {
            "memberships": {"pages": 3, "total": 250, "members": [{"citizen_handle": "EXMPLB1"}]}}}],
        "sentry:/orgs/EXMPLBROKEN/memberships?per_page=100&page=2": [http(500)]}),
    S("roster", "EXMPLNONE", routes=lambda: {
        "sentry:/orgs/EXMPLNONE/memberships?per_page=100&page=1": [http(404)]}),
    # the old service only; a member of a pulled roster
    S("client", "D", api_key=KEY, sentry=False),
    S("citizen", "EXMPLMEMBER", routes=lambda: {
        u_url("EXMPLMEMBER"): [sci_user("Member", "Jan 05, 2020", "#9", {"name": "", "stars": 0})]}),
    S("citizen", "EXMPLTHIRD", routes=lambda: {
        u_url("EXMPLTHIRD"): [sci_user("Third", "2019-02-02T03:04:05", "", {"sid": "EXMPLSCIMAIN", "rank": "Lead"})],
        org_url("EXMPLSCIMAIN"): [sci_org("Sci Main", 99, "Syndicate", "Hardcore", "Smuggling", "Infiltration")]}),
    S("org_name", "EXMPLORG"),
    S("use", "A"),
    S("org_name", "EXMPLNAMED", routes=lambda: {o_url("EXMPLNAMED"): [oprof(" Named Org ", 5)]}),
    S("org_name", "EXMPLNAMEDF", routes=lambda: {o_url("EXMPLNAMEDF"): [oprof("Focused", 5, "Social")]}),
    S("org_name", "EXMPLUNASKED", ask=False),
    S("client", "A2", api_key=KEY, sentry=True),
    S("org", "EXMPLTWOORG", routes=lambda: {
        org_url("EXMPLTWOORG"): [sci_org("Two Org", 7, "Organization", "Regular", "Trading")]}),
    # fresh, too old, the re-read never lands: the other service's answer,
    # with SENTRY's affiliates kept
    S("use", "A"),
    S("citizen", "EXMPLAFFS", fresh=True, routes=lambda: {
        c_url("EXMPLAFFS", FRESH): [player("EXMPLAFFS", 5.0, [
            ("EXMPLORG", True, "Boss", "visible"), ("EXMPLTWOORG", False, "Grunt", "visible"),
            ("EXMPLGAP", False, "", "visible")])],
        o_url("EXMPLGAP"): [oprof("Gap", 1)],
        o_url("EXMPLGAP", 0): [oprof("Gap", 1)],
        org_url("EXMPLGAP"): [http(500)],
        "sentry:/citizens/EXMPLAFFS/metadata": [meta(5.0)],
        u_url("EXMPLAFFS"): [sci_user("Affs", org={"sid": "EXMPLSCIMAIN", "rank": "Lead"})]}),
    S("citizen", "EXMPLAFFW", fresh=True, routes=lambda: {
        c_url("EXMPLAFFW", FRESH): [player("EXMPLAFFW", 5.0, [
            ("EXMPLORG", True, "Boss", "visible"), ("EXMPLTWOORG", False, "Grunt", "visible")])],
        "sentry:/citizens/EXMPLAFFW/metadata": [meta(5.0)],
        u_url("EXMPLAFFW"): [sci_user("Affw", org={"sid": "EXMPLNEWORG", "rank": "Lead"})]}),
    # two orgs without a focus; later one is found, then the other
    S("citizen", "EXMPLTWOGAPS", routes=lambda: {
        c_url("EXMPLTWOGAPS"): [player("EXMPLTWOGAPS", 1.0, [
            ("EXMPLGAPA", True, "", "visible"), ("EXMPLGAPB", False, "", "visible")])],
        o_url("EXMPLGAPA"): [oprof("Gap A", 1)], o_url("EXMPLGAPA", 0): [oprof("Gap A", 1)],
        o_url("EXMPLGAPB"): [oprof("Gap B", 1)], o_url("EXMPLGAPB", 0): [oprof("Gap B", 1)],
        org_url("EXMPLGAPA"): [http(500)], org_url("EXMPLGAPB"): [http(500)],
        u_url("EXMPLTWOGAPS"): [http(500)]}),
    S("cache", "partials"),
    S("advance", 700.0),
    S("complete_partials", True, routes=lambda: {
        o_url("EXMPLGAP"): [oprof("Gap", 1, "Exploration")],
        o_url("EXMPLGAPA"): [oprof("Gap A", 1, "Piracy", "Piracy")]}),
    S("complete_partials", False),
    S("advance", 1900.0),
    S("complete_partials", True, routes=lambda: {o_url("EXMPLGAPB"): [{"error": "timeout"}]}),
    S("complete_partials", True),
    S("citizen", "AB-1234"),
    S("exists", "XY-9 "),
    S("client", "L", provider="link", sentry=True),
    S("citizen", "EXMPL LINK/ü"),
    S("client", "N", provider="none", sentry=True),
    S("exists", "EXMPLNONE"),
    # a month later: copies expire; the old service fails; the old copy stands in
    S("advance", 31 * 86400.0),
    S("use", "D"),
    S("citizen", "EXMPLMEMBER", routes=lambda: {u_url("EXMPLMEMBER"): [http(500)]}),
    S("citizen", "EXMPLMEMBER", fresh=True),
    S("org", "EXMPLSCIMAIN", routes=lambda: {org_url("EXMPLSCIMAIN"): [{"error": "timeout"}]}),
    S("cache", "note_failure", "exmplfail", "HTTP 500", 3, True),
    S("cache", "book_slot", "test_slot", 2.0),
    S("cache", "book_slot", "test_slot", 2.0),
    S("cache", "put_stub", "EXMPLSTUB"),
    S("cache", "drop_members", "EXMPLHALF"),
    S("cache", "member_orgs", "exmplone"),
    S("cache", "partials"),
    S("cache", "stats"),
    S("reopen"),                                 # a placeholder profile is dropped
    S("cache", "get_citizen", "EXMPLONE", True),
    S("cache", "get_org", "EXMPLORG"),
    S("cache", "get_org", "EXMPLORG", True),
]


# ---- running it ---------------------------------------------------------------------

CFIELDS = {f.name for f in fields(rsi.Citizen)}


def js(x):
    if isinstance(x, rsi.Citizen):
        d = asdict(x)
        d["extras"] = {k: js(v) for k, v in sorted(vars(x).items())
                       if k not in CFIELDS and not (v is None or v is False or v == [])}
        d["summary"] = x.summary()
        d["flags"] = x.flags
        d["enlisted_days"] = x.enlisted_days
        return d
    if isinstance(x, (rsi.Org, rsi.Roster)):
        return asdict(x)
    if isinstance(x, tuple):
        return [js(v) for v in x]
    if isinstance(x, list):
        return [js(v) for v in x]
    if isinstance(x, dict):
        return {k: js(v) for k, v in x.items()}
    return x


def dump(path):
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    out = {}
    for (t,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"):
        cols = [r[1] for r in db.execute(f"PRAGMA table_info({t})")]
        names = ", ".join(f'"{c}"' for c in cols)
        out[t] = [list(r) for r in db.execute(f'SELECT {names} FROM "{t}" ORDER BY {names}')]
    db.close()
    return out


LOG = paths.DATA_ROOT / "debug_bursts" / "sentry_requests.log"


def log_lines():
    return LOG.read_text(encoding="utf-8").splitlines() if LOG.exists() else []


def state(c):
    if c is None:
        return None
    s = c.sentry
    mono = CLOCK["mono"]
    return {"down": c.down_for(), "trouble": c._trouble, "told": max(0.0, c._told_until - mono),
            "sentry_down": None if s is None else s.down_for(),
            "sentry_told": None if s is None else max(0.0, s._told_until - mono),
            "live": None if s is None else max(0.0, s._live_until - mono),
            "ready": c.ready, "enriches": c.enriches, "has_key": c.has_key}


root = Path(tempfile.mkdtemp())
cache_path = root / "rsi_cache.db"
dbopen.forget()
cache = rsi.Cache(cache_path)
clients, cur = {}, None
steps = []
last_tables = None
for op, args, kwargs, routes, why in STEPS:
    if routes:
        given = routes()
        ROUTES.update({k: list(v) for k, v in given.items()})
    else:
        given = {}
    SENT.clear()
    before = len(log_lines())
    at = (CLOCK["wall"], CLOCK["mono"])
    result = None
    try:
        if op == "client":
            name = args[0]
            clients[name] = rsi.Client(cache=cache, api_key=kwargs.get("api_key"),
                                       provider=kwargs.get("provider", "api"),
                                       sentry=kwargs["sentry"], min_interval=1.0)
            cur = clients[name]
        elif op == "use":
            cur = clients[args[0]]
        elif op == "advance":
            _sleep(args[0])
        elif op == "reopen":
            dbopen.forget()
            cache = rsi.Cache(cache_path)
        elif op == "cache":
            m, rest = args[0], args[1:]
            if m == "put_stub":
                cache.put_citizen(rsi.Citizen(handle=rest[0], fetched_at=CLOCK["wall"]))
            elif m == "stats":
                result = cache.stats()
                result.pop("bytes")
            else:
                result = getattr(cache, m)(*rest)
        else:
            def call():
                return getattr(cur, op)(*args, **kwargs)
            if why:
                with rsi.why(why):
                    result = call()
            else:
                result = call()
        out = {"ok": js(result)}
    except AssertionError:
        raise
    except Exception as e:
        out = {"raised": type(e).__name__, "describe": rsi.describe_failure(e),
               "transient": rsi.is_transient(e), "service": getattr(e, "service", None)}
    tables = dump(cache_path)
    steps.append({"op": op, "args": args, "kwargs": kwargs, "why": why, "routes": given,
                  "wall": at[0], "mono": at[1], "result": out, "sent": list(SENT),
                  "log": log_lines()[before:], "state": state(cur),
                  # every table, when this step changed one
                  "dump": None if steps and tables == last_tables else tables})
    last_tables = tables

PARSE_UTC = ["2026-10-01T12:00:00", "2026-10-01T12:00:00Z", "2026-10-01 12:00:00.5",
             "2026-10-01T12:00:00.123456+00:00", "2026-10-01T12:00:00.1234567Z",
             "2026-10-01T12:00:00-05:00", "2026-10-01T12:00:00+0530", "2026-10-01",
             "2026-10-01T12", "2026-10-01T12:30", "20261001T120000", "2026-10-01T12:00:00,25Z",
             "2026-02-30T00:00:00", "2026-10-01T24:00:00", "nonsense", "", None, 0, 1790000000,
             # corners where CPython's C parser and its Python fallback differ
             "2026-10-01T", "2026-10-01T12.5", "2026-10-01T12:00:00X+00:00", "2026-10-01T1230",
             "2026-10-01T123", "2026-10-01T12:", "2026-10-01T12:00+05", "2026-10-01T12:00+5",
             "2026-10-01T12:00:00:7", "2026-10-01T12:00:00.", "2026-10-01T12:00:00.+01:00",
             "202610011", "2026-10", "2026-10-1", "2026-13-01", "0000-01-01", "2026-10-01T12:00-23:59",
             "2026-10-01T12:00+24:00", "2026-10-01x12:00", "20261001 1200", True, 20261001]
ENLISTED = ["2015-03-01T10:00:00.000000", "2015-03-01T10:00:00", "2015-03-01", "Mar 1, 2015",
            "March 01, 2015", "mar 1,   2015", "2015-3-1", "Sept 1, 2015", "2015-03-01T10:00:00.5",
            " 2015-03-01", "", "garbage"]
RATE = [None, {}, {"RateLimit": '"default";r=80;t=25'}, {"RateLimit": "r=3, t=9"},
        {"RateLimit-Remaining": "5", "RateLimit-Reset": "12"},
        {"RateLimit": "x", "RateLimit-Remaining": "n", "RateLimit-Reset": "1"},
        {"ratelimit": '"default";r=0;t=60'}]


def msg(d):
    if d is None:
        return None
    m = email.message.Message()
    for k, v in d.items():
        m[k] = v
    return m


(HERE / "rsi_ops.json").write_text(json.dumps({
    "key": KEY, "wall0": 1_790_000_000.0, "mono0": 5000.0,
    "steps": steps,
    "parse_utc": [[t, rsi._parse_utc(t)] for t in PARSE_UTC],
    "enlisted": [[t, rsi._enlisted_epoch(t)] for t in ENLISTED],
    "rate_left": [[d, rsi._rate_left(msg(d))] for d in RATE],
    "urls": [[h, rsi.lookup_url(h), rsi.profile_url(h), rsi.org_url(h)]
             for h in ["EXMPLONE", "EXMPL LINK/ü", "a&b?c=d#e", "~-._"]],
}, indent=0), encoding="utf-8", newline="\n")
print(len(steps), "steps,", sum(len(s["sent"]) for s in steps), "requests,",
      sum(len(s["log"]) for s in steps), "log lines")
