"""reputation.py's RSI-dependent work - find_ghosts, purge_ghosts,
fetch_roster - and the client as the database's identity verifier, for
tests/upkeep_ops.rs. On the scripted network and fake clock of fakenet.py;
after each step the result, the requests sent, and every table of both
sc-watch.db and rsi_cache.db.

    python make_upkeep_ops.py      (sc-watch's build environment)

Made-up handles and orgs only. Writes upkeep_ops.json.
"""
import json
import sqlite3
import sys
import time
import types
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from fakenet import *  # noqa: E402,F401,F403
from fakenet import _sleep  # noqa: E402
import reputation  # noqa: E402

reputation.time = types.SimpleNamespace(time=lambda: CLOCK["wall"], localtime=time.gmtime,
                                        strftime=time.strftime)


def S(op, *args, routes=None, **kwargs):
    return (op, list(args), kwargs, routes)


def scrape_off(h):
    return c_url(h, TTL, False)


def pages(sid, *pages_):
    """SENTRY roster pages: each a list of (handle, visibility)."""
    n = len(pages_)
    return {f"sentry:/orgs/{sid}/memberships?per_page=100&page={i + 1}": [{"json": {
        "memberships": {"pages": n, "total": total, "members": [
            {"citizen_handle": h, "display_name": h.title(), "member_rank": "Crew",
             "rank_number": 2, "visibility": vis} for h, vis in rows]},
        "metadata": {"last_fetched": iso(1.0)}}}] for i, (total, rows) in enumerate(pages_)}


STEPS = [
    S("client", "A", api_key=KEY, sentry=True),
    *[S("rep.record_sighting", h, 2.0, 2, 2) for h in (
        "EXMPLREAL", "EXMPLGONE", "EXMPLGONE2", "EXMPLGONE2", "EXMPLREPORTED", "EXMPLAUDITED",
        "EXMPLBOTH", "EXMPLERR", "EXMPLCACHED", "EXMPLUNSEEN")],
    S("rep.add_report", "EXMPLREPORTED", "griefing", "rammed us"),
    S("rep.add_report", "EXMPLBOTH", "piracy"),
    S("rep.add_audit", "EXMPLAUDITED", 3.0, 1, 3, ["EXMPLAUDITEO"]),
    S("rep.add_audit", "EXMPLBOTH", 3.0, 1, 3, ["EXMPLB0TH"]),
    S("cache.put_gone", "EXMPLCACHED"),
    S("find_ghosts", routes=lambda: {
        scrape_off("EXMPLAUDITED"): [http(404, X_Lookup_State="not_found")],
        scrape_off("EXMPLBOTH"): [http(404, X_Lookup_State="not_found")],
        scrape_off("EXMPLERR"): [http(404)], u_url("EXMPLERR"): [http(500)],
        scrape_off("EXMPLGONE"): [http(404, X_Lookup_State="not_found")],
        scrape_off("EXMPLGONE2"): [http(404)], u_url("EXMPLGONE2"): [http(404)],
        scrape_off("EXMPLREAL"): [player("EXMPLREAL", 2.0)],
        scrape_off("EXMPLREPORTED"): [http(404, X_Lookup_State="not_found")],
        scrape_off("EXMPLUNSEEN"): [http(404, X_Lookup_State="not_checked")],
        u_url("EXMPLUNSEEN"): [sci_user("Unseen")]}),
    S("find_ghosts", limit=1),                   # from the cache now
    S("client", "B", api_key=None, sentry=True),
    S("advance", 120.0),
    S("rep.record_sighting", "EXMPLNEWFACE", 1.0, 3, 3),     # nobody can check them
    S("find_ghosts", routes=lambda: {scrape_off("EXMPLERR"): [{"error": "timeout"}],
                                     scrape_off("EXMPLNEWFACE"): [http(404, X_Lookup_State="not_checked")]}),
    S("verify", "EXMPLREAL"),
    S("verify", "EXMPLGONE"),
    S("verify", "EXMPLNOBODY", routes=lambda: {
        c_url("EXMPLNOBODY", None, False): [http(404, X_Lookup_State="not_checked")]}),
    S("use", "A"),
    S("purge_ghosts", routes=lambda: {u_url("EXMPLNEWFACE"): [http(404)]}),
    S("purge_ghosts", apply=True),
    # rosters
    S("fetch_roster", ""),
    S("fetch_roster", "exmplnotflagged"),
    S("rep.flag_org", "EXMPLCREW", "they gank", "Crew"),
    S("rep.flag_org", "EXMPLEMPTY"),
    S("rep.flag_org", "EXMPLBROKE"),
    S("rep.flag_org", "EXMPLNOTOTAL"),
    S("rep.flag_org", "EXMPLHALF"),
    S("client", "L", provider="link", sentry=True),
    S("fetch_roster", "EXMPLCREW"),
    S("use", "A"),
    S("fetch_roster", " exmplcrew ", routes=lambda: pages(
        "EXMPLCREW", (9, [("EXMPLREAL", "visible"), ("EXMPLM1", "visible"), ("[HIDDEN]", "hidden")]),
        (9, [("EXMPLM2", "visible"), ("[REDACTED]", "redacted")]))),
    S("advance", 3600.0),
    S("fetch_roster", "EXMPLCREW", routes=lambda: pages(
        "EXMPLCREW", (8, [("EXMPLREAL", "visible"), ("EXMPLM3", "visible")]))),
    S("fetch_roster", "EXMPLEMPTY", routes=lambda: {
        "sentry:/orgs/EXMPLEMPTY/memberships?per_page=100&page=1": [http(404)],
        "sci:live/organization_members/EXMPLEMPTY?page=1": [{"json": {"success": 1, "data": []}}]}),
    S("fetch_roster", "EXMPLNOTOTAL", routes=lambda: {
        **pages("EXMPLNOTOTAL", (0, [("EXMPLM4", "visible")])),
        o_url("EXMPLNOTOTAL"): [oprof("No Total", 33, "Piracy")],
        org_url("EXMPLNOTOTAL"): [sci_org("No Total", 33, "Syndicate")]}),
    S("fetch_roster", "EXMPLHALF", routes=lambda: {
        **pages("EXMPLHALF", (12, [("EXMPLM5", "visible")]), (12, [])),
        "sentry:/orgs/EXMPLHALF/memberships?per_page=100&page=2": [http(404)]}),
    S("use", "B"),
    S("fetch_roster", "EXMPLBROKE", routes=lambda: {
        "sentry:/orgs/EXMPLBROKE/memberships?per_page=100&page=1": [http(500)]}),
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


def plain(x):
    """The result as JSON; an error in find_ghosts by its type only (the
    port words them with describe_failure, not Python's exception text)."""
    if isinstance(x, dict) and "errors" in x:
        x = dict(x, errors=[[h, e.split(":")[0]] for h, e in x["errors"]])
    return json.loads(json.dumps(x))


root = Path(tempfile.mkdtemp())
dbopen.forget()
cache = rsi.Cache(root / "rsi_cache.db")
rep = reputation.Database(root / "sc-watch.db")
clients, cur = {}, None
steps, last = [], None
for op, args, kwargs, routes in STEPS:
    given = routes() if routes else {}
    ROUTES.update({k: list(v) for k, v in given.items()})
    SENT.clear()
    at = (CLOCK["wall"], CLOCK["mono"])
    result = None
    if op == "client":
        clients[args[0]] = cur = rsi.Client(cache=cache, api_key=kwargs.get("api_key"),
                                            provider=kwargs.get("provider", "api"),
                                            sentry=kwargs["sentry"], min_interval=1.0)
    elif op == "use":
        cur = clients[args[0]]
    elif op == "advance":
        _sleep(args[0])
    elif op == "cache.put_gone":
        cache.put_citizen(rsi.Citizen(handle=args[0], exists=False, fetched_at=CLOCK["wall"]))
    elif op.startswith("rep."):
        result = getattr(rep, op[4:])(*args, **kwargs)
    elif op == "verify":
        result = cur.exists(args[0], strict=True)
    else:
        result = getattr(rep, op)(*args, client=cur, **kwargs)
    tables = {"rep": dump(root / "sc-watch.db"), "rsi": dump(root / "rsi_cache.db")}
    steps.append({"op": op, "args": args, "kwargs": kwargs, "routes": given, "wall": at[0],
                  "mono": at[1], "result": plain(result), "sent": list(SENT),
                  "dump": None if tables == last else tables})
    last = tables
rep.close()

(HERE / "upkeep_ops.json").write_text(json.dumps({
    "key": KEY, "wall0": 1_790_000_000.0, "mono0": 5000.0, "steps": steps}, indent=0),
    encoding="utf-8", newline="\n")
print(len(steps), "steps,", sum(len(s["sent"]) for s in steps), "requests")
