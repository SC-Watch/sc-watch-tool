# sc-watch in Rust - the `rust-port` branch

sc-watch, ported to Rust, for Windows and Linux. The port is a workspace of
library crates with no user interface, so the standalone tool and SC
All-In-One can both build on them. The Python program beside this file is the
reference: until the Rust version replaces it, every stage is checked against
it - or against OpenCV and IRIS, which it was itself checked against.

## Status

| Phase | What | State |
|---|---|---|
| 0 | Groundwork on the Windows PC: the corpus, the parity tools, IRIS's reference readings | in progress |
| 1 | `scw-vision`: the image functions, the IRIS letter reader and nameplate finder, a replay tool | **done** - exact on every reference, end to end |
| 2 | The watcher on Windows: capture (with HDR), keys, store, RSI client, audio | **capture, keys, the JSON config files, audio, report flags, sc-watch.db, watchlist.db, the RSI client with its cache and HTTPS, sharing, housekeeping and settings done**; **the watcher done** (the `sc-watch` program, IRIS reader only: whole sessions identical to watch.py on 122 real frames); **the page's server done** (`sc-watch-ui`: every route identical to ui_server.py, and on the real data) |
| 2a | The mining overlay (IRIS's), on the phase 1 and 2 pieces | - |
| 3 | One process and the page | - |
| 4 | Linux on Bazzite (KDE): portal capture, evdev, speech-dispatcher, layer-shell overlay | - |
| 5 | OCR (chat reads, the IRIS fallback) | - |
| 6 | SC All-In-One: a Watch page on the same crates | - |

Each phase ends at a gate. Phase 1's - every stage matches the reference
exactly on every corpus frame - decides whether the rest is worth building.

**`scw-vision::cv`** reimplements the 15 OpenCV functions the IRIS reader
uses: threshold (plain and Otsu), subtract, lookup table, counting, bounding
box, grey, erode/dilate/opening/top-hat, cubic/linear/area resize, box blur
and connected components. On 584 synthetic fixtures made with the pinned
OpenCV 5.0.0 (`crates/scw-vision/tests/fixtures/make_fixtures.py`) every one
gives OpenCV's bytes - including where OpenCV's vector and plain code round
differently, and its label order - except the float cubic resize, which
OpenCV hands to Intel's IPP: that one is held within 1e-4 and its one user,
the O/0 rule, is held exactly at the reader level instead.

**`scw-vision::iris`** is IRIS's text-row reader: the 4x ink mask, the
letter model (in IRIS's own order of operations, so its logits are IRIS's to
the bit), the best-path split of joined letters, the O/0 and `_`/`-` rules
and the distance-line checks. Against IRIS's own readings of real game
frames - 743 text rows in 243 crops, the reference the Python port was held
to - every stage matches on every row: the mask bit for bit, the pieces in
both alphabets, the names with their O/0 evidence, the distances. A name
read takes 0.9 ms and a distance read 0.7 ms, against the Python port's 1.6
and 1.4 ms on the same rows. In CI, 43 synthetic rows (OpenCV's own fonts on
noise, read by the Python port) check the same stages.

**`scw-vision::iris::{detector, ops}`** is IRIS's nameplate finder and its
two app-level operations: scan a frame for distance lines, and read the plate
above one. Against IRIS's own output on 135 real 5120x1440 frames, for both
scan areas the Python port was checked with, every stage matches: the capture
area, the candidate boxes (151 a frame) in order, the strict, crowd and loose
pairs, the distance flags, the lines kept, and all 440 plate reads (406 of
them names) with their class, reason, name and rows. One thread scans the
app's area in about 90 ms a frame - what OpenCV's optimised code takes in the
Python port (91.6 ms on one thread, 89.7 on the app's two) - and reads a
plate in 2 ms against 4. The Python detector kept each frame's masks on
itself between calls; here they belong to the call, so one detector can serve
any number of threads.

**`scw-vision::names`** sorts a raw read into what it is (player, ship serial,
NPC crew, UNKNOWN, world label, unreadable) and votes a burst of reads into
contacts, exactly as the Python's classify_name and consensus.py.
**`scw-vision::pipeline`** joins it all as the watcher does with
`--reader iris`: each frame's distance lines read and classified, a burst's
players tallied and voted. **`scw-replay`** runs saved frames through it.
Against the Python pipeline on the 135 reference frames
(`crates/scw-replay/reference.py`, run with the Python's own reader) the
output is identical, field for field, for two configurations: the default
(whole screen, bursts of 3, 1 vote - 222 contacts, 34 voted) and IRIS's
scan area with bursts of 5 and 2 votes (184 contacts, 14 voted).

**`scw-capture`** gives frames from the Windows desktop through Desktop
Duplication, as the Python's dxcam and hdr_capture.py: monitors numbered the
same way, 8-bit frames as they come, and - with Windows HDR on and HDR
capture `auto` - float frames converted to what the game drew at the
display's SDR white level. Every float16 value converts exactly as
hdr_capture.py does (ten white levels, checked in CI), and on this PC's two
HDR monitors a frame from each path matches the Python's own capture of the
same screen on 100.00% of pixels (the rest is the screen changing between
the two grabs). An HDR frame takes 15 ms against the Python's 45. Two things
found on the way: Windows refuses the float capture to a process that is not
per-monitor DPI aware (dxcam sets that quietly; so does this), and the first
frame of a new duplication is a blank float surface whatever was asked for -
the Python passes it over only by luck of its start-up check; here it is
skipped after every (re)open. The replay source for saved frames lives here
too. Linux capture is phase 4.

**`scw-input`** is the bindings - keys, joystick buttons and combinations
(`rctrl+joy0.b24`, `vid231d:0126.b7`) - and when they fire: once, on the
change into all held. The parser gives inputs.parse()'s tokens, labels and
error messages for all 58 specs it is checked on (CI); the press logic runs
against any `InputState`, so it is tested with a fake one. On Windows the
state is read as the Python reads it - GetAsyncKeyState and joyGetPosEx,
nothing hooked, nothing swallowed - and its device list on this PC is
identical to `python inputs.py --list`, every field of four sticks. Linux
(evdev) is phase 4.

**`scw-store::json`** reads and writes the JSON config files as
jsonstore.py does - a BOM or the Windows codepage read, NaN and Infinity as
no value, an unreadable file copied aside before anything overwrites it,
atomic writes with retries - and writes the same bytes: Python's
`json.dumps(indent=2)` (non-ASCII escaped, floats as Python's repr, ties to
even included) with Python's text-mode line endings. Checked on jsonstore's
own answers (CI), and on this PC's real settings.json, audio.json and
flags.json: read and written again, each comes out byte for byte the file
the Python saved.

**`scw-audio`** says what audio.py says: the phrase table the settings page
is drawn from, check_phrase with CPython's own format-string parser and
error messages, render and its clean-ups, distances, speakable handles, flag
and reason words, whole lines and the "hear it" samples, the tone WAVs byte
for byte, and audio.json loaded field by field - 800 cases from audio.py
itself (CI). One deliberate difference: a field with a conversion, a format
spec or an attribute (`{handle!r}`, `{handle:>10}`, `{handle.upper}`) - which
the settings page refuses to save - is silence, where the Python speaks
quotes, cuts the handle short or reads out a memory address.

The alert player (`scw-audio::alerts`) makes audio.Alerts' decisions: for 8
configurations by 12 bursts, the same tones and the same lines in the same
order as the Python's own Alerts with a recording voice (CI) - rationing,
summaries, muted flags and reasons, tone-only contacts, the 20-second
expiry, phrases re-read when audio.json changes. On Windows the voice is
SAPI 5, made on the worker thread, speaking plain text synchronously, and
measured to really speak: a line into a file is 6.2 s of speech, the same
line to the speakers takes 6.5 s to return, and so does the second one (the
pyttsx3 failure returned in 0.1 s); a tone plays to its end.

**`scw-store::db` and `::reputation`** open the SQLite files as dbopen.py
does (WAL, read before set; synchronous=NORMAL; a 5 s busy timeout), so
both versions can use them at once, and create sc-watch.db from
reputation.py's own schema text with its migrations in its order. A fresh
database matches the Python's to the stored SQL of every table and index
(CI). On copies of this PC's databases - September's, before three later
migrations, November's and today's - the Python's migration and the port's
end in identical files, schema and every row (tools/migrate_compare.py),
and the port's sketch() equals the 701 sketches the Python stored.

The **reputation `Database`** is reputation.Database: identity (a merge
needs a verifier saying one side is NOT a real account; with none, nothing
merges), sightings, reports, the ignore list, audits and their images,
flagged orgs and roster history, hand-added members, and the assessment
with its score, spoken reasons and wording - the verifier, the watchlist,
the clock and the report flags (`scw-store::flags`, flags.py exactly)
injected. A script of 107 operations - every identity verdict, every
confirm_audit path, roster pulls with leavers, returns, a source switch and
a partial pull - gives the same return value, the same rows in every table
and the same audit images on disk after every step as reputation.py (CI).
On copies of this PC's three databases every read agrees for every player -
585, 541 and 286 of them: assessments, is_listed, audits, flagged orgs and
members, and 5,604 read-only canonical() checks on misread-style variants
(tools/read_compare.py). The parts that need the lookup client
(find_ghosts, purge_ghosts, fetch_roster) are in `scw-rsi::upkeep`, below.

**`scw-store::watchlist`** is watchlist.py: sources (enable, forget),
entries with their confidence and claims, flagged orgs, exact and
misread-variant lookup (each `Hit` worded by `describe()` as the Python
words it), the bundled starter list seeded and re-seeded by version, and
pasted lists through parse_lines, import_text and safe_url. It implements
the reputation `Database`'s watchlist lookup, so an assessment with a
watchlist attached is the Python's. A script of 40 operations gives the same
results and rows after every step; parse_lines on 7 pasted texts, safe_url
and 6 assessments with hits agree (CI). On a copy of this PC's watchlist,
lookups of all 702 known handles, the sources, orgs, stats and starter
seeding are identical (tools/watchlist_compare.py).

**`scw-rsi`** is rsi.py: SENTRY first and starcitizen-api.com behind it,
each with its own pacing and outage breaker; SENTRY's request log; and
rsi_cache.db - profiles, orgs, roster member lists, failed lookups, partial
profiles, and the pauses every process must see. With it come the client's
rules on what each source is trusted for: "no such account" only from a
source that asked RSI, SENTRY's placeholders and re-reads, its limit on live
lookups, affiliates kept, partial profiles finished later. The network is a
`Transport` and time a `Clock`, both given to the client, and what rsi.py
left on a thread (headers, the lookup state) is returned, so a test runs
hours of pauses instantly. A script of 110 steps on a scripted network
(statuses, headers, timeouts, bad JSON) gives rsi.py's answers. It sends the
same 115 requests at the same moments with the same timeouts, writes the
same 74 log lines, and leaves the same pauses and rows after every step
(CI). Twelve deliberate breakages are each caught. SENTRY's timestamps are
read as CPython 3.13's C `fromisoformat` reads them, corners included (ISO
week dates are not read). On copies of this PC's three caches (876, 824
and 443 profiles), every profile and org reads back and writes back the
same JSON. Every cached handle looked up with the network refused, with and
without a key, gives the same answer and wording (tools/rsi_compare.py).
Deliberately different: a value of the wrong shape inside an answer (a list
where an object belongs) is read as empty, where rsi.py fails with
AttributeError; and stats() measures the cache's own file.

The network is **`Https`**: ureq with rustls, so Linux needs no system
OpenSSL. Like urllib, it follows redirects, keeps the headers of any non-2xx
answer, and reads the body as UTF-8 with bad bytes replaced before parsing
it as JSON. Against a server on 127.0.0.1 (CI) it sends the right
User-Agent and Accept, and reads 2xx, 404 and 429 with their headers, bad
JSON, bad UTF-8, a redirect, a timeout and a refused connection as urllib
does. Once for real (2026-11-22, examples/sentry_probe.rs), it made one
database-only SENTRY lookup of a made-up name. SENTRY answered 404
`not_checked`, read as "nobody", and its rate-limit headers were read; 0.7
s. Deliberately different: the timeout bounds the whole request, where
urllib's bounds each socket operation.

**`scw-rsi::upkeep`** is what sc-watch.db needs the client for:
reputation.py's find_ghosts, purge_ghosts and fetch_roster, and the client
as the database's identity verifier (`Verifier`). They sit here rather than
in scw-store, which knows nothing of the network. A 46-step script, with
reputation.py driving rsi.py on the same scripted network, gives the same
results, requests, and rows in both databases after every step (CI). Seven
deliberate breakages are each caught. Deliberately different: find_ghosts
words a failed lookup with `describe`, where reputation.py printed the
Python exception.

**`scw-store::share`** is share.py: exporting your own reports, the reasons
they use, your flagged orgs and hand-added roster members to an org mate,
and importing theirs. An import keeps who said what, a re-export replaces
it, and one person's imports can all be removed. An export is untrusted
input: only share.json is read, sizes are checked before anything is
decompressed, and every field is checked before anything is written. Three
made-up installs export, preview, import, re-export, collide on a member
and remove a source over 59 steps. Results, rows and flags.json match after
every step, and each side reads the other's zips. 64 crafted documents, 14
crafted zips (encrypted, oversized, duplicated, not UTF-8, nested 5,000
deep) and 15 base64 texts get the same answers (CI). Eleven of twelve
deliberate breakages are caught; the twelfth removes a rule share.py
itself makes redundant. On a copy of this PC's database, the export (8
reports, 3 reasons, 6 orgs) and its import into an empty database are
identical (tools/share_compare.py). Deliberately stricter than share.py:
Python's `$` lets a value ending in a newline pass its checks (a handle, an
install id, a reason key or a colour stored with the newline), and its bool
passes as a version. The port refuses both, and refuses a share holding an
infinite number.

**`scw-store::housekeeping`** is housekeeping.py: retention for captured
frames in the managed directories only, never below the newest few, and the
audit pruning by database row. The settings come in as an argument rather
than being read here. A made-up data folder, rebuilt in the test with the
same names, sizes and times, swept under nine policies, dry and for real,
deletes the same frames and prints the same lines. Policy loading reads 13
legacy-file and settings cases the same way. The audit pruning leaves the
same rows and images (CI). On Windows, frames of equal age and the
case-blind `*.png` match too. Twelve deliberate breakages are each caught.
A dry run over this PC's real data folder is identical: 307 frames due in
debug_bursts, 26 audits and 2 orphan images, nothing deleted
(tools/housekeeping_compare.py).

**`scw-store::settings`** is settings.py. Its schema is data, as settings.py
intends: `settings_schema.json` is settings.py's own `describe()` (every
setting's type, default, range, label and help), generated from it, so the
page's form and every default are the Python's by construction. The code
holds the rules. Each value is clamped to its field's type and range (the
default when it cannot be read); two superseded defaults are upgraded; a
save merges over the file under its lock. On top is a typed `Settings` for
the watcher, with enums for the choices, which must cover exactly the
schema. Clamping 14 fields of every kind against 46 awkward values (CI),
loading 11 crafted files and a sequence of saves all match settings.py.
Eleven deliberate breakages are each caught. On copies of this PC's two
settings files, the load matches and an unchanged save writes the same
bytes (tools/settings_compare.py). Known: a number past 64 bits reaches the
port as a float, so as text it reads "1e+30" where Python prints every
digit; as a number or a switch it clamps the same.

**`scw-watch`** is the watcher, watch.py, being ported in parts. Done so
far:

- **The small pieces.** Contact tracking and repeat suppression (a longer
  reading of the same contact corrects a shorter one). The binding,
  chat-region and focus-size checks with their messages. The "waiting for"
  label. sightings.csv, written as Python's csv module writes it. The
  session log (debuglog.DebugLog). Every step and line matches the Python;
  twelve deliberate breakages are each caught.
- **The lookup worker** (RsiEnricher). It has its retry ladders, service
  pauses, expired copies, SENTRY still checking, misread audits with their
  pictures, and a flag alerted once. It is a core driven a step at a time,
  plus a thread around it. A 51-step script on a scripted client and fake
  clock gives watch.py's lookups at the same moments. The lines, alerts, log
  events, failure notes, retry state, rows and audit pictures all match.
  Thirteen deliberate breakages are each caught.
- **The session** (watch.Session). It covers the key loop with its queue
  and ping delay, the auto loop with its tracker, and bursts read frame by
  frame with announcements as they go. Each burst is settled into
  sightings.csv, the database, weak-read audits and the lookup queue. Then
  come the burst's alert, restart, the shutdown wait for lookups, and the
  summary. Everything it uses is handed to it: frames, reader, keys,
  console, alerts, log, database, lookup queue, clock, restart flag and
  housekeeping. Eleven scenarios run the real watch.Session on a scripted
  reader and keys and a fake clock. They give the same lines, status
  changes, alerts, log events, lookups (with the crops' pixels), reader
  calls, CSV, rows, audit pictures, summary and exit code. The clock ends at
  the same moment to the last bit, so every wait loop turned as often.
  Twenty deliberate breakages are each caught.

Deliberately different in the session. Reading chat needs OCR, which this
version lacks: the chat key says so instead of reading. `--debug` saves each
frame and logs its size, but the OCR pipeline's diagnostics (boxes, pairs,
the km gate) are not there. Ctrl+C is noticed at the next key poll or loop
turn (within 30 ms), not at whatever line Python was on.

- **The program** (`sc-watch`, watch.py's main). The command line has
  the saved settings as its defaults and argparse's rules: abbreviations,
  `--flag=value`, Python's number rules, the same error messages and exit
  codes. Eighty command lines over three settings files match
  watch.parse_args; twelve of thirteen deliberate breakages are caught,
  and the thirteenth changes a default no command line can reach. The
  console's status line matches watch.Console byte for byte, at three
  widths and redirected. The data folder follows the built Python's rules:
  `data\` beside a `portable.txt`, else %LOCALAPPDATA%\sc-watch. Then it
  wires up the screen (8-bit or HDR), keys, the IRIS reader, the alert
  player, the lookup client and worker, the database with its watchlist,
  and the session log.

  Whole sessions were compared on real frames. watch.py and sc-watch ran
  `--replay` over three folders of saved frames (122 frames) and once with
  other options. Each side had its own fresh data folder, with no lookups
  and audio off. The announcements, sightings.csv, the database rows, the
  audit crops' pixels and every session-log event were identical on all
  four runs. sc-watch took about three quarters of the Python's time
  (10.8 s against 14.3 s for 54 frames). The frames name real people, so
  the comparison stays on the machine that has them.

Deliberately different in the program:
- `SCW_DATA_ROOT` names the data folder outright, for development and
  tests.
- A saved `ocr` or `iris+ocr` reader is warned about and read as `iris`.
- `--support-bundle` and `--check-resolution` say they are not in this
  version yet.
- A second Ctrl+C leaves at once.
- A restart starts a new process and leaves, as os.execv does on Windows.
- A capture that fails says so once per reason.
- Lines printed while the status line shows (housekeeping) clear it first;
  the Python printed over it.

Kept as Python does it:
- A burst's alert lists every contact the burst settled, labels on the
  ignore list included; only the console and the records leave them out.
- The housekeeping line reads "freed 0 MB across 0 file(s) | " when its
  folders exist but hold nothing due.

**`scw-ui`** is the page's server, ui_server.py, and the `sc-watch-ui`
program. The page itself - ui/index.html, app.js, app.css - is the Python's,
built in byte for byte. Behind it: the state the page draws, the audit
pictures, every write, the lookups and roster pulls it starts in the
background, the background check, and the live stream. Everything outside -
the clock, the network, threads, the file manager, the voice, the keys - is
a hook, so one script drives it all.

make_ui_ops.py runs the real ui_server.py on a socket, 147 requests across
every route, on a made-up network (profiles, orgs and member lists answered
as SENTRY answers) and a fake clock. sc-watch-ui gives the same status,
content type and body for each, sends the same requests out, opens the same
folders and says the same lines. At each checkpoint all three databases and
the files it writes are the same. Nineteen of twenty deliberate breakages
are caught; the twentieth rewords a message nobody sees while jobs run
inline. A socket test checks the HTTP itself: HTTP/1.0, http.server's error
pages, the stream's headers and first event.

On the real data: both servers ran on copies of the dev data (120 tiles,
both views, six flagged orgs' member lists, the watchlist, shares, phrases
and storage panels) and gave identical answers. The comparison prints paths,
never values, and deletes the copies.

Deliberately different in the page's server:
- The support bundle says it is not in this version yet.
- A ghost check's HTTP failure reads as every other lookup error ("SENTRY
  had an error (HTTP 500)"), not urllib's "HTTP Error 500: <reason>".
- A share export's zip is this compressor's bytes; what is inside is the
  same.

Deliberately stricter than debuglog.py: a Windows home folder inside an
event's values is scrubbed. debuglog.py scrubs the JSON text, where every
backslash is doubled, so such a path went into the log unscrubbed.

## The replay tool

```
cargo run --release -p scw-replay -- [options] <frames>...
```

Frames are PNG files, folders, or patterns (`D:/frames/burst_*.png`); bursts
are consecutive frames, `--burst` at a time (the watcher's trigger is not
modelled). `--scan-area`, `--min-votes` and `--listed` (handles that count as
people even when they read as a world label) work as the watcher's own;
`--json` writes one document for comparing. `--help` lists the rest.

## Build and test

```
cargo build --workspace
cargo test --workspace
```

CI (`.github/workflows/rust.yml`) runs format, lint (warnings are errors) and
tests on Windows and Ubuntu for every push to this branch.

## Data that never enters the repository

Real game frames, the reference readings made from them and copies of real
databases name real players. They stay on the machine they were made on.
Tests that need them read a folder from the `SCW_PARITY_DATA` environment
variable and skip themselves when it is not set, so CI and a fresh clone run
everything else. Fixtures in the repository are synthetic.

What the folder holds:

| Folder | What | Used by |
|---|---|---|
| `iris-golden/` | IRIS's own readings: the bench harness's `--golden` output, crops as PNG plus `golden.json` | `tests/iris_golden.rs` |
| `iris-detect/` | `frames.txt` (one PNG frame per line; relative paths from this folder) and `out/` - IRIS's own finder output, the bench harness's `det_{whole,app}_{dump,read}.json` | `tests/iris_detect_golden.rs` |
| `replay/` | `*.json`: `reference.py --frames-list iris-detect/frames.txt [options]` output, one file per configuration | `crates/scw-replay/tests/replay_reference.rs` |

```
SCW_PARITY_DATA=/path/to/parity-data cargo test --release -- --nocapture
```

To work on another machine with it, copy the folder there by hand - never
through the repository.

## How the structure changes

The port is when the app gets cleaned up, in this order:

1. **Parity first.** A stage gives the reference's bytes before anything about
   it is improved. Then improvements come one at a time, each measured on the
   corpus - a reader that quietly reads differently attaches the wrong name to
   a real person.
2. **Pure core crates.** `scw-vision` (and later the mining reader and the
   store's rules) make no OS calls and keep no globals: a frame and a settings
   value in, typed results out. The Python reader reads module-level switches
   (`sd.KM_GATE_ENABLED`, `sd.RED_SAT_ENABLED`, ...); here every tunable is a
   field of a settings struct passed in.
3. **One process, one owner per job.** Capture, reading, voting, the store,
   audio and the page are supervised tasks joined by typed channels - no file
   polling, no restart flag, no rate-limit table shared through a database.
4. **Live settings.** One typed structure, changed in place and pushed to the
   tasks that use it; only what must rebuild (say, the capture after a monitor
   change) does.
5. **One RSI client.** One rate limiter in memory, and a question asked twice
   at once is answered once.
6. **The page keeps its JSON API**, so the files in `ui/` work unchanged, and
   receives what changed rather than the whole state after every write.
7. **People's data carries across.** The three SQLite files and their
   migrations, the JSON settings files, share zips and audit images stay
   readable by both versions.
8. **Platforms behind interfaces.** Windows: DXGI capture (HDR converted on the
   GPU), GetAsyncKeyState and winmm. Linux: the screen-sharing portal over
   PipeWire, evdev, speech-dispatcher, a layer-shell overlay on KDE.

## Layout

```
crates/scw-vision    image functions (cv); IRIS reader, finder and plate reads (iris);
                     name classifier and burst vote (names); frames to contacts (pipeline)
crates/scw-capture   frame sources: the Windows desktop (8-bit or HDR float), saved frames
crates/scw-input     key and joystick bindings, and the Windows state readers
crates/scw-store     the JSON config files, report flags, sc-watch.db, watchlist.db,
                     sharing with org mates, frame and audit housekeeping, settings
crates/scw-rsi       player and org lookup (SENTRY, starcitizen-api.com) and rsi_cache.db
crates/scw-watch     the watcher (watch.py) and the sc-watch program: tracking, announcing, lookups, audits, logs, the session
crates/scw-ui        the page's server (ui_server.py) and the sc-watch-ui program; the page is ui/, built in
crates/scw-audio     what alerts say, the tones, audio.json, and the alert player
crates/scw-replay    saved frames through the pipeline, as text or JSON
```

Planned beside it: `scw-mining`, `scw-overlay`, and `sc-watch`, the app that
joins them.

Licence: GPL-3.0-or-later, as the rest of sc-watch. The IRIS reader and the
mining overlay are ported from IRIS (github.com/Niteb1rd/IRIS, GPL-3.0).
