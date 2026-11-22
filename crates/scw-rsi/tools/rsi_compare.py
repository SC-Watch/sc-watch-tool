"""rsi.py against the port (examples/rsi_dump.rs) on a COPY of an
rsi_cache.db: every profile and org as read back (any age, and whether
still fresh), the JSON each writes back, partial profiles, member lists,
stats; every cached handle looked up with the network refused - without a
key and with one - and the identity check; the cache-only step of
complete_partials(); the request log; and then every row. Prints counts
only, and deletes the copies and both outputs.

    python rsi_compare.py <work folder> <rsi_cache.db> [<now, epoch seconds>]

Run with sc-watch's build environment, after `cargo build --release -p
scw-rsi --example rsi_dump`. Copies are made with SQLite's backup API; the
original is opened read-only. Nothing is sent anywhere: both sides refuse
every request.
"""
import calendar  # noqa: F401
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
import types
import urllib.error
from dataclasses import asdict, fields
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
WORK, SRC = Path(sys.argv[1]), Path(sys.argv[2])
sys.path.insert(0, str(ROOT))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
os.environ.pop("SC_API_KEY", None)
import dbopen  # noqa: E402
import rsi  # noqa: E402

EXE = ".exe" if sys.platform == "win32" else ""
RUST = ROOT / "target" / "release" / "examples" / f"rsi_dump{EXE}"


def copy(src, dst):
    s = sqlite3.connect(f"file:{src}?mode=ro", uri=True)
    d = sqlite3.connect(dst)
    s.backup(d)
    d.close()
    s.close()


def rows(path):
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    out = {}
    for (t,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"):
        got = [list(r) for r in db.execute(f'SELECT * FROM "{t}"')]
        out[t] = sorted(got, key=lambda r: json.dumps(r))
    db.close()
    return out


def main():
    WORK.mkdir(parents=True, exist_ok=True)
    a, b, out = WORK / "rsi_py.db", WORK / "rsi_rs.db", WORK / "rsi_rs.json"
    try:
        copy(SRC, a)
        copy(SRC, b)
        src = sqlite3.connect(f"file:{a}?mode=ro", uri=True)
        newest = max([r[0] or 0 for r in src.execute("SELECT fetched_at FROM citizens")] + [0])
        src.close()
        now = float(sys.argv[3]) if len(sys.argv) > 3 else float(int(newest) + 3600)
        clock = {"wall": now, "mono": 1000.0}

        def sleep(s):
            if s > 0:
                clock["wall"] += s
                clock["mono"] += s

        rsi.time = types.SimpleNamespace(time=lambda: clock["wall"], monotonic=lambda: clock["mono"],
                                         sleep=sleep, mktime=time.mktime, strptime=time.strptime)
        rsi.random = types.SimpleNamespace(uniform=lambda x, y: (x + y) / 2)

        class Now:
            @staticmethod
            def now():
                return datetime.fromtimestamp(clock["wall"], timezone.utc).replace(tzinfo=None)

        rsi.datetime = Now
        rsi.PROCESS = "compare"

        def refuse(*_a, **_k):
            raise urllib.error.URLError("refused")

        rsi.Sentry._fetch = refuse
        rsi.Client._fetch = refuse
        cfields = {f.name for f in fields(rsi.Citizen)}

        def cjs(c):
            d = asdict(c)
            d["extras"] = {k: v for k, v in sorted(vars(c).items())
                           if k not in cfields and not (v is None or v is False or v == [])}
            d["summary"] = c.summary()
            d["flags"] = c.flags
            d["enlisted_days"] = c.enlisted_days
            return d

        def raised(e, described=True):
            return {"raised": type(e).__name__,
                    "describe": rsi.describe_failure(e) if described else None,
                    "transient": rsi.is_transient(e)}

        dbopen.forget()
        cache = rsi.Cache(a)
        db = sqlite3.connect(f"file:{a}?mode=ro", uri=True)
        handles = [r[0] for r in db.execute("SELECT handle FROM citizens ORDER BY handle")]
        sids = [r[0] for r in db.execute("SELECT sid FROM orgs ORDER BY sid")]
        members = [r[0] for r in db.execute("SELECT DISTINCT handle FROM org_members ORDER BY handle")]
        db.close()
        py = {"citizens": [], "orgs": []}
        for h in handles:
            try:
                c = cache.get_citizen(h, any_age=True)
                anyc, text = (cjs(c), json.dumps(asdict(c))) if c else (None, None)
            except Exception as e:
                anyc, text = raised(e, False), None
            try:
                fresh = cache.get_citizen(h) is not None
            except Exception:
                fresh = False
            py["citizens"].append({"handle": h, "any": anyc, "text": text, "fresh": fresh})
        for s in sids:
            try:
                o = cache.get_org(s, any_age=True)
                anyo, text = (asdict(o), json.dumps(asdict(o))) if o else (None, None)
            except Exception as e:
                anyo, text = raised(e, False), None
            try:
                fresh = cache.get_org(s) is not None
            except Exception:
                fresh = False
            py["orgs"].append({"sid": s, "any": anyo, "text": text, "fresh": fresh})
        py["partials"] = [list(p) for p in cache.partials()]
        py["member_orgs"] = {h: cache.member_orgs(h) for h in members}
        st = cache.stats()
        st.pop("bytes")
        py["stats"] = st
        py["offline"] = {}
        for name, key in (("nokey", None), ("key", "EXMPLKEY")):
            client = rsi.Client(cache=cache, api_key=key, sentry=True, min_interval=0.0)
            got = []
            for h in handles:
                try:
                    c = cjs(client.citizen(h))
                except Exception as e:
                    c = raised(e)
                try:
                    x = client.exists(h, strict=True)
                except Exception as e:
                    x = raised(e)
                got.append({"handle": h, "citizen": c, "exists": x})
            try:
                done = client.complete_partials(ask=False)
            except Exception as e:
                done = raised(e)
            py["offline"][name] = {"lookups": got, "complete": done}
        logp = paths.DATA_ROOT / "debug_bursts" / "sentry_requests.log"
        py["log"] = logp.read_text(encoding="utf-8").splitlines() if logp.exists() else []
        cache.db.close()            # or Windows will not let the copy be deleted
        dbopen.forget()
        py["rows"] = rows(a)

        subprocess.run([str(RUST), str(b), str(out), repr(now)], check=True)
        rs = json.loads(out.read_text(encoding="utf-8"))
        rs["rows"] = {t: sorted(v, key=lambda r: json.dumps(r)) for t, v in rs["rows"].items()}

        problems = []
        for part in ("citizens", "orgs", "partials", "member_orgs", "stats", "log", "rows"):
            if py[part] != rs[part]:
                if isinstance(py[part], list) and isinstance(rs[part], list) and len(py[part]) == len(rs[part]):
                    n = sum(1 for x, y in zip(py[part], rs[part]) if x != y)
                    problems.append(f"{part}: {n} of {len(py[part])} differ")
                elif isinstance(py[part], dict) and isinstance(rs[part], dict):
                    n = sum(1 for k in set(py[part]) | set(rs[part]) if py[part].get(k) != rs[part].get(k))
                    problems.append(f"{part}: {n} entries differ")
                else:
                    problems.append(f"{part}: differ ({len(py[part])} vs {len(rs[part])})")
        for name in ("nokey", "key"):
            p, r = py["offline"][name], rs["offline"][name]
            n = sum(1 for x, y in zip(p["lookups"], r["lookups"]) if x != y)
            if n or len(p["lookups"]) != len(r["lookups"]):
                problems.append(f"offline {name}: {n} of {len(p['lookups'])} lookups differ")
            if p["complete"] != r["complete"]:
                problems.append(f"offline {name}: complete_partials differs")
        kinds = {}
        for c in py["offline"]["key"]["lookups"]:
            v = c["citizen"]
            k = ("raised " + v["raised"]) if "raised" in v else (
                "unverified" if v["unverified"] else
                "old copy" if v["extras"].get("stale_reason") else "cached")
            kinds[k] = kinds.get(k, 0) + 1
        print(f"{len(handles)} profiles, {len(sids)} orgs, {len(py['partials'])} partial, "
              f"{len(members)} roster members, {len(py['log'])} log lines; "
              f"offline lookups with a key: {kinds}")
        print("IDENTICAL" if not problems else "DIFFERENT:\n  " + "\n  ".join(problems))
        return 0 if not problems else 1
    finally:
        for p in (a, b, out, Path(str(a) + "-wal"), Path(str(a) + "-shm"),
                  Path(str(b) + "-wal"), Path(str(b) + "-shm")):
            try:
                p.unlink()
            except OSError:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
