# sc-watch - reads player handles off the Star Citizen HUD.
# Copyright (C) 2026 SC-Watch
#
# This program is free software: you can redistribute it and/or modify it
# under the terms of the GNU General Public License as published by the Free
# Software Foundation, either version 3 of the License, or (at your option)
# any later version.
#
# This program is distributed in the hope that it will be useful, but WITHOUT
# ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
# FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
# more details. You should have received a copy of the GNU General Public
# License along with this program. If not, see <https://www.gnu.org/licenses/>.

"""
Sharing your records with someone you trust - an org mate - and taking theirs.

WHAT GOES IN AN EXPORT, AND WHAT NEVER DOES
-------------------------------------------
In:   your own reports (handle, reason, note, when), the definitions of the
      reasons they use, the orgs you flagged, the roster members you added by
      hand, and the name you chose to be shown as.
Out:  sightings - they would say when and where you play - screenshots and
      audit images, settings, your API key, watchlists, contacts nobody
      reported, and anything you imported from someone else. Re-sharing a
      stranger's report under your name would launder who said it.

WHERE THINGS CAME FROM IS KEPT
------------------------------
An imported report is stored with `source` = the name its author gave and
`origin` = "<their install>:<their report id>". So it shows as "from <name>",
and their next export UPDATES what you have from them instead of duplicating
it - and drops the reports they have since deleted. A roster member they added
by hand carries their install in `manual_src`. Everything from one person can
be removed in one go (remove_source), which is what trust going wrong needs.

Imported reports count like your own - they come from someone you chose to
trust, and a flag that did not alert would be decoration. They are matched on
the EXACT handle: nothing is attached to a similar-looking name.

AN EXPORT IS UNTRUSTED INPUT
----------------------------
It arrives as a file somebody sent you. Only share.json inside the zip is
read; its declared size is checked before it is decompressed; every field is
type-checked, length-capped and pattern-checked before anything is written;
and handles and SIDs must look like handles and SIDs. A file that fails any of
that is refused whole - nothing half-imported.
"""

from __future__ import annotations

import base64
import io
import json
import re
import time
import uuid
import zipfile

import flags as flags_mod
import reputation

FORMAT = "sc-watch-share"
VERSION = 1
MEMBER = "share.json"
MAX_ZIP = 12 * 1024 * 1024          # the file as sent
MAX_JSON = 16 * 1024 * 1024         # share.json once decompressed
MAX_ROWS = 50_000
MAX_NAME = 40
SHARED_NOTE = "shared by "          # how an org flag from an import is marked

HANDLE_RE = re.compile(r"^[A-Za-z0-9_-]{2,40}$")
SID_RE = re.compile(r"^[A-Za-z0-9_-]{1,32}$")
INSTALL_RE = re.compile(r"^[0-9a-f]{32}$")
KEY_RE = re.compile(r"^[a-z0-9_]{1,40}$")
COLOR_RE = re.compile(r"^#[0-9a-fA-F]{6}$")
_CONTROL = re.compile(r"[\x00-\x1f\x7f]")

README = """This is an sc-watch share file: somebody's own records, for an org mate.

Import it in sc-watch: Settings -> Sharing -> Import. You will see what is in
it before anything is added, and everything from this person can be removed
again in one click.

It holds their reports (who, why, their note, when), the orgs they flagged,
and roster members they added by hand. It does NOT hold where or when they
played, screenshots, settings, API keys, or anything they imported from
someone else.
"""


class ShareError(ValueError):
    """A share file that cannot be used, with a reason a person can act on."""


def clean_name(raw) -> str:
    name = _CONTROL.sub("", str(raw or "")).strip()
    return re.sub(r"\s+", " ", name)[:MAX_NAME]


def _reserved(name: str) -> bool:
    """"self" is what YOUR reports carry as their source. An import from
    someone who called themselves that stored reports indistinguishable from
    your own - spoken as yours, and re-exported under your name."""
    return name.strip().lower() == "self"


def install_id(db: reputation.Database) -> str:
    row = db.db.execute("SELECT value FROM meta WHERE key='install'").fetchone()
    if row and INSTALL_RE.match(row["value"] or ""):
        return row["value"]
    iid = uuid.uuid4().hex
    db.db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('install', ?)",
                  (iid,))
    db.db.commit()
    return iid


# ---- export ----------------------------------------------------------------

def build(db: reputation.Database, name: str) -> dict:
    """The export as a document. Your own records only - see the docstring."""
    name = clean_name(name)
    if not name:
        raise ShareError("give a name to be shown as - your in-game name is best")
    if _reserved(name):
        raise ShareError('"self" cannot be used as a name - your in-game name is best')
    # origin IS NULL as well: an imported report is never yours to re-share,
    # whatever its source says.
    reports = [{"id": r["id"], "handle": r["handle"], "ts": r["ts"],
                "category": r["category"], "note": r["note"] or ""}
               for r in db.db.execute(
                   """SELECT id, handle, ts, category, note FROM reports
                      WHERE COALESCE(source,'self')='self' AND origin IS NULL
                        AND category!=?
                      ORDER BY id""", (reputation.PENDING,))]
    used = sorted({r["category"] for r in reports})
    flags = []
    for key in used:
        f = flags_mod.by_key(key)
        flags.append({"key": key, "label": f.label, "color": f.color,
                      "polarity": f.polarity, "say": f.say})
    # Not the orgs an import flagged: apply() is the only writer of a
    # "shared by <name>" note, and flagging the org yourself replaces it. They
    # used to go out under your name like any other.
    orgs = [{"sid": o["sid"], "name": o["name"], "note": o["note"]}
            for o in db.flagged_orgs()
            if not (o["note"] or "").startswith(SHARED_NOTE)]
    roster = [{"sid": r["sid"], "handle": r["handle"], "note": r["note"] or "",
               "ts": r["manual_ts"]}
              for r in db.db.execute(
                  """SELECT m.sid, m.handle, m.note, m.manual_ts
                     FROM org_membership m JOIN org_flags f ON f.sid = m.sid
                     WHERE m.manual_ts IS NOT NULL AND m.manual_src IS NULL
                       AND m.gone_since IS NULL
                     ORDER BY m.sid, m.handle""")]
    return {"format": FORMAT, "version": VERSION, "from": name,
            "install": install_id(db), "exported_at": time.time(),
            "reports": reports, "flags": flags, "orgs": orgs, "roster": roster}


def pack(doc: dict) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr(MEMBER, json.dumps(doc, indent=1))
        z.writestr("README.txt", README)
    return buf.getvalue()


def filename(doc: dict) -> str:
    safe = re.sub(r"[^A-Za-z0-9_-]+", "_", doc["from"]).strip("_") or "share"
    return f"sc-watch-share-{safe}-{time.strftime('%Y%m%d')}.zip"


# ---- reading one ---------------------------------------------------------------

def unpack(data: bytes) -> dict:
    """Bytes of a zip -> a checked document, or ShareError."""
    if len(data) > MAX_ZIP:
        raise ShareError("that file is far too big to be an sc-watch share")
    try:
        z = zipfile.ZipFile(io.BytesIO(data))
    except zipfile.BadZipFile:
        raise ShareError("that is not a zip file") from None
    with z:
        try:
            info = z.getinfo(MEMBER)
        except KeyError:
            raise ShareError("that zip is not an sc-watch share (no share.json)") from None
        if info.flag_bits & 0x1:
            raise ShareError("that share is encrypted")
        if info.file_size > MAX_JSON:
            raise ShareError("that share is too big")
        with z.open(info) as f:
            raw = f.read(MAX_JSON + 1)
    if len(raw) > MAX_JSON:
        raise ShareError("that share is too big")
    try:
        doc = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError, RecursionError):
        # RecursionError: nesting thousands deep, which no export has.
        raise ShareError("that share is damaged (share.json is not readable)") from None
    return check(doc)


def unpack_b64(text: str) -> dict:
    try:
        return unpack(base64.b64decode(str(text or ""), validate=True))
    except (ValueError, TypeError) as exc:
        if isinstance(exc, ShareError):
            raise
        raise ShareError("the file did not arrive intact") from None


def _num(v, what):
    if isinstance(v, bool) or not isinstance(v, (int, float)) or v != v:
        raise ShareError(f"damaged share: {what} is not a number")
    return float(v)


def _text(v, cap, what):
    if v is None:
        return ""
    if not isinstance(v, str):
        raise ShareError(f"damaged share: {what} is not text")
    return _CONTROL.sub(" ", v).strip()[:cap]


def _rows(doc, key):
    v = doc.get(key, [])
    if not isinstance(v, list):
        raise ShareError(f"damaged share: {key} is not a list")
    if len(v) > MAX_ROWS:
        raise ShareError(f"that share has too many {key}")
    if not all(isinstance(r, dict) for r in v):
        raise ShareError(f"damaged share: an entry in {key} is not a record")
    return v


def check(doc) -> dict:
    """Validate everything, or refuse everything. Returns a clean copy."""
    if not isinstance(doc, dict) or doc.get("format") != FORMAT:
        raise ShareError("that is not an sc-watch share")
    ver = doc.get("version")
    if not isinstance(ver, int) or ver < 1:
        raise ShareError("damaged share: no version")
    if ver > VERSION:
        raise ShareError("that share was made by a newer sc-watch - update to import it")
    name = clean_name(doc.get("from"))
    if not name:
        raise ShareError("damaged share: it does not say who it is from")
    if _reserved(name):
        raise ShareError('damaged share: "self" is not a name it can be imported under')
    install = str(doc.get("install", ""))
    if not INSTALL_RE.match(install):
        raise ShareError("damaged share: no install id")
    now = time.time()
    exported = _num(doc.get("exported_at"), "the export date")

    reports = []
    for r in _rows(doc, "reports"):
        h, cat = str(r.get("handle", "")), str(r.get("category", ""))
        if not HANDLE_RE.match(h):
            raise ShareError(f"damaged share: {h[:24]!r} is not a handle")
        if not KEY_RE.match(cat) or cat == reputation.PENDING:
            raise ShareError(f"damaged share: {cat[:24]!r} is not a report reason")
        rid = r.get("id")
        if isinstance(rid, bool) or not isinstance(rid, int) or rid < 0:
            raise ShareError("damaged share: a report has no id")
        ts = _num(r.get("ts"), "a report date")
        reports.append({"id": rid, "handle": h.upper(), "category": cat,
                        "ts": min(ts, now), "note": _text(r.get("note"), 500, "a note")})
    if len({r["id"] for r in reports}) != len(reports):
        raise ShareError("damaged share: two reports share an id")

    flags = []
    for f in _rows(doc, "flags"):
        key = str(f.get("key", ""))
        if not KEY_RE.match(key):
            raise ShareError("damaged share: a reason has a bad key")
        if key == reputation.PENDING:
            continue            # not a reason anyone defines; flags.save drops it
        color = str(f.get("color", ""))
        pol = str(f.get("polarity", "negative"))
        flags.append({"key": key,
                      "label": _text(f.get("label"), flags_mod.MAX_LABEL, "a reason name") or key,
                      "color": color if COLOR_RE.match(color) else flags_mod.UNKNOWN_COLOR,
                      "polarity": pol if pol in flags_mod.POLARITIES else "negative",
                      "say": _text(f.get("say"), flags_mod.MAX_SAY, "a spoken name")})

    orgs = []
    for o in _rows(doc, "orgs"):
        sid = str(o.get("sid", ""))
        if not SID_RE.match(sid):
            raise ShareError(f"damaged share: {sid[:24]!r} is not an org SID")
        orgs.append({"sid": sid.upper(), "name": _text(o.get("name"), 80, "an org name"),
                     "note": _text(o.get("note"), 200, "an org note")})

    roster = []
    for m in _rows(doc, "roster"):
        sid, h = str(m.get("sid", "")), str(m.get("handle", ""))
        if not SID_RE.match(sid) or not HANDLE_RE.match(h):
            raise ShareError("damaged share: a roster entry is not an org and a handle")
        ts = m.get("ts")
        roster.append({"sid": sid.upper(), "handle": h.upper(),
                       "note": _text(m.get("note"), 200, "a roster note"),
                       "ts": min(_num(ts, "a roster date"), now) if ts is not None else now})

    return {"format": FORMAT, "version": ver, "from": name, "install": install,
            "exported_at": min(exported, now), "reports": reports, "flags": flags,
            "orgs": orgs, "roster": roster}


# ---- what an import would do, and doing it -----------------------------------------

def _mine(db, install: str) -> bool:
    row = db.db.execute("SELECT value FROM meta WHERE key='install'").fetchone()
    return bool(row) and row["value"] == install


def preview(db: reputation.Database, doc: dict) -> dict:
    """What importing `doc` would add, change and remove. Writes nothing."""
    install = doc["install"]
    if _mine(db, install):
        raise ShareError("that is your own export")
    prefix = install + ":"
    have = {r["origin"] for r in db.db.execute(
        "SELECT origin FROM reports WHERE origin LIKE ?", (prefix + "%",))}
    incoming = {prefix + str(r["id"]) for r in doc["reports"]}
    handles = {r["handle"] for r in doc["reports"]}
    met = {r["handle"] for r in db.db.execute(
        "SELECT handle FROM players WHERE sightings > 0")} & handles
    flagged = {o["sid"] for o in db.flagged_orgs()}
    before = db.db.execute("SELECT name, imported_at FROM shares WHERE install=?",
                           (install,)).fetchone()
    roster_orgs = {m["sid"] for m in doc["roster"]}
    return {
        "from": doc["from"], "exported_at": doc["exported_at"],
        "before": ({"name": before["name"], "imported_at": before["imported_at"]}
                   if before else None),
        "reports": len(doc["reports"]), "players": len(handles),
        "met": sorted(met),
        "new_reports": len(incoming - have), "updated_reports": len(incoming & have),
        "dropped_reports": len(have - incoming),
        "orgs": len(doc["orgs"]),
        "new_orgs": [o["sid"] for o in doc["orgs"] if o["sid"] not in flagged],
        "roster": len(doc["roster"]),
        # Members of orgs that are not flagged here and are not in the file's
        # own org list either: nothing to put them on.
        "roster_orphans": sum(1 for m in doc["roster"]
                              if m["sid"] not in flagged
                              and m["sid"] not in {o["sid"] for o in doc["orgs"]}),
        "roster_orgs": sorted(roster_orgs),
    }


def _ensure_flags(doc):
    """Reasons you do not have are added RETIRED: they render with the
    author's name and colour and score with its polarity, but stay out of your
    dropdown. A reason you already have keeps YOUR definition."""
    known = {f.key for f in flags_mod.all_flags()}
    new = [dict(f, retired=True) for f in doc["flags"] if f["key"] not in known]
    used = {r["category"] for r in doc["reports"]} - known - {f["key"] for f in new}
    new += [{"key": k, "label": k.replace("_", " "), "color": flags_mod.UNKNOWN_COLOR,
             "polarity": "negative", "say": "", "retired": True} for k in used]
    if new:
        from dataclasses import asdict
        flags_mod.save([asdict(f) for f in flags_mod.all_flags()] + new)
    return [f["key"] for f in new]


def apply(db: reputation.Database, doc: dict, reports: bool = True,
          orgs: bool = True, roster: bool = True) -> dict:
    """Import. The author's previous import is REPLACED by this one: their
    reports updated by origin, ones they deleted dropped, and the same for the
    roster members they added. Your own records are never touched."""
    install, name = doc["install"], doc["from"]
    if _mine(db, install):
        raise ShareError("that is your own export")
    now = time.time()
    out = {"from": name, "reports_added": 0, "reports_updated": 0,
           "reports_dropped": 0, "orgs_flagged": [], "roster_added": 0,
           "roster_dropped": 0, "roster_skipped": 0, "reasons_added": []}
    if reports:
        out["reasons_added"] = _ensure_flags(doc)
    c = db.db
    c.commit()                      # nothing half-open before ours
    c.execute("BEGIN")
    try:
        c.execute("""INSERT INTO shares (install, name, exported_at, imported_at)
                     VALUES (?,?,?,?) ON CONFLICT(install) DO UPDATE SET
                       name=excluded.name, exported_at=excluded.exported_at,
                       imported_at=excluded.imported_at""",
                  (install, name, doc["exported_at"], now))
        if reports:
            prefix = install + ":"
            have = {r["origin"]: r["id"] for r in c.execute(
                "SELECT id, origin FROM reports WHERE origin LIKE ?", (prefix + "%",))}
            seen = set()
            for r in doc["reports"]:
                origin = prefix + str(r["id"])
                seen.add(origin)
                if origin in have:
                    c.execute("""UPDATE reports SET handle=?, ts=?, category=?, note=?,
                                 source=? WHERE id=?""",
                              (r["handle"], r["ts"], r["category"], r["note"], name,
                               have[origin]))
                    out["reports_updated"] += 1
                else:
                    # Straight into reports, NOT add_report(): that creates a
                    # contact, and a stranger someone else met is not yours.
                    # When you do meet them, their tile brings these along.
                    c.execute("""INSERT INTO reports
                                   (handle, ts, category, note, evidence, source, origin)
                                 VALUES (?,?,?,?,'',?,?)""",
                              (r["handle"], r["ts"], r["category"], r["note"], name,
                               origin))
                    out["reports_added"] += 1
            for origin, rid in have.items():
                if origin not in seen:
                    c.execute("DELETE FROM reports WHERE id=?", (rid,))
                    out["reports_dropped"] += 1
        if orgs:
            flagged = {r["sid"] for r in c.execute("SELECT sid FROM org_flags")}
            for o in doc["orgs"]:
                if o["sid"] in flagged:
                    continue
                note = f"{SHARED_NOTE}{name}" + (f": {o['note']}" if o["note"] else "")
                c.execute("INSERT INTO org_flags (sid, name, ts, note) VALUES (?,?,?,?)",
                          (o["sid"], o["name"], now, note[:500]))
                out["orgs_flagged"].append(o["sid"])
        if roster:
            flagged = {r["sid"] for r in c.execute("SELECT sid FROM org_flags")}
            theirs = {(r["sid"], r["handle"]): r for r in c.execute(
                "SELECT sid, handle, src FROM org_membership WHERE manual_src=?",
                (install,))}
            seen = set()
            for m in doc["roster"]:
                key = (m["sid"], m["handle"])
                if m["sid"] not in flagged:
                    out["roster_skipped"] += 1
                    continue
                seen.add(key)
                row = c.execute(
                    """SELECT manual_ts, manual_src FROM org_membership
                       WHERE sid=? AND handle=?""", key).fetchone()
                if row is None:
                    c.execute("""INSERT INTO org_membership
                                   (sid, handle, display, rank, stars, first_seen,
                                    last_seen, gone_since, src, manual_ts, note,
                                    manual_src)
                                 VALUES (?,?,'','',0,?,?,NULL,'manual',?,?,?)""",
                              (m["sid"], m["handle"], now, now, m["ts"], m["note"],
                               install))
                    out["roster_added"] += 1
                elif row["manual_ts"] and row["manual_src"] is None:
                    continue                 # yours: your note, your call
                elif row["manual_ts"] and row["manual_src"] != install:
                    continue                 # another mate's: first in keeps it
                else:
                    c.execute("""UPDATE org_membership SET gone_since=NULL,
                                   manual_ts=COALESCE(manual_ts, ?), note=?, manual_src=?
                                 WHERE sid=? AND handle=?""",
                              (m["ts"], m["note"], install, *key))
                    if not row["manual_ts"]:
                        out["roster_added"] += 1
                c.execute("""INSERT OR IGNORE INTO player_orgs (handle, sid, is_main, source)
                             VALUES (?,?,0,'roster')""", (m["handle"], m["sid"]))
            for key, r in theirs.items():
                if key not in seen:
                    _drop_manual(c, *key, r["src"])
                    out["roster_dropped"] += 1
        c.commit()
    except Exception:
        c.rollback()
        raise
    return out


def _drop_manual(c, sid, handle, src):
    """Take back a hand-added membership: gone entirely if only that hand
    listed them, otherwise back to plain roster membership."""
    if (src or "sci") == "manual":
        c.execute("DELETE FROM org_membership WHERE sid=? AND handle=?", (sid, handle))
        c.execute("DELETE FROM player_orgs WHERE sid=? AND handle=? AND source='roster'",
                  (sid, handle))
    else:
        c.execute("""UPDATE org_membership SET manual_ts=NULL, note=NULL, manual_src=NULL
                     WHERE sid=? AND handle=?""", (sid, handle))


def sources(db: reputation.Database) -> list[dict]:
    """Everyone you have imported from, with what of theirs you still hold."""
    out = []
    for s in db.db.execute("SELECT * FROM shares ORDER BY imported_at DESC"):
        n_rep = db.db.execute("SELECT COUNT(*) FROM reports WHERE origin LIKE ?",
                              (s["install"] + ":%",)).fetchone()[0]
        n_ros = db.db.execute("SELECT COUNT(*) FROM org_membership WHERE manual_src=?",
                              (s["install"],)).fetchone()[0]
        out.append({"install": s["install"], "name": s["name"],
                    "exported_at": s["exported_at"], "imported_at": s["imported_at"],
                    "reports": n_rep, "roster": n_ros})
    return out


def remove_source(db: reputation.Database, install: str) -> dict:
    """Everything one person's exports brought in, gone. Orgs their import
    flagged stay flagged - flagging is a judgement you can undo in the Orgs
    tab, and unflagging silently could drop your own intel on those orgs."""
    if not INSTALL_RE.match(install or ""):
        raise ShareError("no such source")
    c = db.db
    row = c.execute("SELECT name FROM shares WHERE install=?", (install,)).fetchone()
    if row is None:
        raise ShareError("no such source")
    c.commit()                      # nothing half-open before ours
    c.execute("BEGIN")
    try:
        n_rep = c.execute("DELETE FROM reports WHERE origin LIKE ?",
                          (install + ":%",)).rowcount
        rows = c.execute("SELECT sid, handle, src FROM org_membership WHERE manual_src=?",
                         (install,)).fetchall()
        for r in rows:
            _drop_manual(c, r["sid"], r["handle"], r["src"])
        c.execute("DELETE FROM shares WHERE install=?", (install,))
        c.commit()
    except Exception:
        c.rollback()
        raise
    return {"name": row["name"], "reports": n_rep, "roster": len(rows)}
