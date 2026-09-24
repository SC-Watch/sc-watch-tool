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
One way to open a SQLite file, because there are two processes and three of them.

WHY WAL
-------
The watcher and the UI are separate processes on the same database, and the
default journal mode ('delete') gives a writer an exclusive lock that every
reader waits behind. Measured over six seconds with one process writing
sightings and another polling the way the UI does:

    journal=delete   writer 785 rows | reader 83 polls  | p95 341 ms | max 733 ms
    journal=wal      writer 3551 rows | reader 121899 polls | p95 0.1 ms | max 5.4 ms

Nobody errored - busy_timeout absorbs it - so this was never a crash, just the
UI freezing for up to two thirds of a second whenever a contact was recorded,
and the watcher running at a quarter speed while the UI was open.

WAL keeps readers and writers out of each other's way at the cost of two extra
files beside the database (-wal and -shm). Those are already excluded from
support bundles and from the sterile build.

The mode is a property of the FILE, not the connection, so setting it once
would do - but it is set on every open because that is cheaper than reasoning
about which open happens first, and re-applying it is a no-op.

WHY ONE-TIME MIGRATIONS
-----------------------
reputation.Database ran its schema and four PRAGMA table_info migrations on
every construction, and the UI constructs one per HTTP request. That is ~1 ms
of pointless work per request, and more importantly it means a migration
races itself under concurrent requests. Callers now ask migrated_once() and
skip the work when another connection to the same file has already done it.

WHY ONE CONNECTION PER THREAD
-----------------------------
connect() used to hand back a single sqlite3 connection opened with
check_same_thread=False, and three objects were shared across threads on the
strength of that flag: the watcher's Database (main loop and RSI thread), the
RSI cache (lookups on one thread, identity checks on the other) and the
process-wide watchlist (both of those, plus every UI request thread).

The flag only switches off Python's safety check. It does not make sharing
safe, and none of it was. Measured:

    4 threads, identical reads, one connection   16,063 errors in 80,000
                                                 9,656 of them WRONG ANSWERS -
                                                 fetchone() said "no row" for
                                                 a row that exists
    2 threads through one rsi.Client             ~30% of lookups raised
    the watcher's own ignore test                is_ignored() raised
                                                 "not an error", was swallowed,
                                                 and an IGNORED name was
                                                 announced, logged and looked up

The errors were all caught by `except Exception: pass` around the calls, so
the only visible symptom was data quietly missing - and for the watchlist, a
listed handle quietly coming back unlisted.

So connect() now returns a ThreadLocalConnection: one object, as before, but
each thread that uses it gets its own sqlite3 connection underneath, opened on
first use. That is SQLite's actual rule, and it gives each thread real
transaction isolation too - one thread's commit() can no longer land in the
middle of another's writes. WAL is what makes several connections to one file
cheap. Nothing that calls it had to change.
"""

from __future__ import annotations

import sqlite3
import threading
import weakref
from pathlib import Path

# Files whose migrations have already run in THIS process. Not persistence -
# just "we have already looked at this path", which is all that is needed to
# stop the same work repeating per request.
_migrated: set[str] = set()
_lock = threading.Lock()

# Kept explicit rather than left to the default so the value is visible next to
# the reason for it. Cross-process contention retries for this long before
# raising; with WAL it should never be reached.
BUSY_TIMEOUT_MS = 5000


class _Conn(sqlite3.Connection):
    """A plain connection that can be weakly referenced; the C type cannot."""


def _open(path: str, row_factory) -> sqlite3.Connection:
    """One real connection, configured. Only ThreadLocalConnection calls this.

    check_same_thread=False is kept for ONE reason: close() is allowed to shut
    every thread's connection from whichever thread closes the database. No
    connection is otherwise used off the thread that opened it.
    """
    db = sqlite3.connect(path, check_same_thread=False,
                         timeout=BUSY_TIMEOUT_MS / 1000.0, factory=_Conn)
    if row_factory is not None:
        db.row_factory = row_factory
    try:
        # Per-connection, and cheap.
        db.execute(f"PRAGMA busy_timeout={BUSY_TIMEOUT_MS}")
        # NORMAL rather than FULL: with WAL this is the documented safe pairing
        # and it stops every commit waiting on a disk flush. The failure it
        # exposes is losing the last transactions in a power cut, which for a
        # log of who flew past is an acceptable trade against the watcher
        # stalling on every sighting.
        db.execute("PRAGMA synchronous=NORMAL")
        # journal_mode is a property of the FILE and persists, but SETTING it
        # costs 1.9 ms even when the answer is already WAL, because it takes a
        # lock to do it. Reading it costs ~0.01 ms. Applying it unconditionally
        # on every connection made a UI request SLOWER than the per-request
        # migrations this module was written to remove, so: read, and only
        # write when it actually differs.
        cur = db.execute("PRAGMA journal_mode").fetchone()
        if (cur[0] if cur else "").lower() != "wal":
            db.execute("PRAGMA journal_mode=WAL")
    except sqlite3.Error:
        # A read-only directory, a network share that refuses WAL, an
        # exotic filesystem. The database still works in its default mode;
        # it is only slower under contention.
        pass
    return db


# Every live ThreadLocalConnection, so release_thread() can reach them all.
_registry: "weakref.WeakSet[ThreadLocalConnection]" = weakref.WeakSet()


class ThreadLocalConnection:
    """One database handle that is safe to share between threads.

    Looks like a sqlite3 connection - execute, executemany, executescript,
    commit, rollback, close - but every thread gets its own real connection,
    opened the first time that thread touches it. See the module docstring
    for what sharing one connection actually did.

    Connections belonging to threads that have finished are dropped with the
    thread; release_thread() closes the calling thread's promptly, for
    servers that start a thread per request.
    """

    def __init__(self, path: Path | str, row_factory=sqlite3.Row):
        self.path = str(path)
        self._row_factory = row_factory
        self._local = threading.local()
        self._open_conns: "weakref.WeakSet[_Conn]" = weakref.WeakSet()
        self._guard = threading.Lock()
        self._closed = False
        _registry.add(self)
        # Opened now, on the constructing thread, so a bad path or a locked
        # file fails at construction exactly as sqlite3.connect() did.
        self._conn()

    def _conn(self) -> sqlite3.Connection:
        c = getattr(self._local, "c", None)
        if c is None:
            if self._closed:
                raise sqlite3.ProgrammingError(
                    "Cannot operate on a closed database.")
            c = _open(self.path, self._row_factory)
            self._local.c = c
            with self._guard:
                self._open_conns.add(c)
        return c

    def execute(self, *args):
        return self._conn().execute(*args)

    def executemany(self, *args):
        return self._conn().executemany(*args)

    def executescript(self, *args):
        return self._conn().executescript(*args)

    def commit(self):
        # A thread that never wrote has nothing to commit; opening a
        # connection just to commit nothing would be waste.
        c = getattr(self._local, "c", None)
        if c is not None:
            c.commit()

    def rollback(self):
        c = getattr(self._local, "c", None)
        if c is not None:
            c.rollback()

    def release_thread(self):
        """Close the CALLING thread's connection, if it has one."""
        c = getattr(self._local, "c", None)
        if c is None:
            return
        self._local.c = None
        with self._guard:
            self._open_conns.discard(c)
        try:
            c.close()
        except sqlite3.Error:
            pass

    def close(self):
        """Close every thread's connection. Further use raises, as sqlite3's
        own close() does."""
        with self._guard:
            self._closed = True
            conns = list(self._open_conns)
            self._open_conns = weakref.WeakSet()
        self._local = threading.local()
        for c in conns:
            try:
                c.close()
            except sqlite3.Error:
                pass

    def __getattr__(self, name):
        # Anything else - total_changes, in_transaction, row_factory - is the
        # calling thread's connection's.
        return getattr(self._conn(), name)


def connect(path: Path | str, *, row_factory=sqlite3.Row) -> ThreadLocalConnection:
    """Open a database the way every caller here should open one."""
    return ThreadLocalConnection(path, row_factory)


def release_thread():
    """Close this thread's connection to every open database.

    For a server that runs each request on a fresh thread: without it, a
    shared handle - the watchlist - would hold one connection per finished
    request until the garbage collector got round to it.
    """
    for tl in list(_registry):
        tl.release_thread()


def migrated_once(path: Path | str) -> bool:
    """True the FIRST time this process is asked about a path, False after.

    Lets a caller run schema creation and column migrations once instead of
    on every connection.
    """
    key = str(Path(path).resolve()).lower()
    with _lock:
        if key in _migrated:
            return False
        _migrated.add(key)
        return True


def forget(path: Path | str | None = None):
    """Drop the migration memory. For tests that rebuild a database."""
    with _lock:
        if path is None:
            _migrated.clear()
        else:
            _migrated.discard(str(Path(path).resolve()).lower())
