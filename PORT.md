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
| 1 | `scw-vision`: the image functions, the IRIS letter reader and nameplate finder, a replay tool | **image functions and reader done** - the finder next |
| 2 | The watcher on Windows: capture (with HDR), keys, store, RSI client, audio | - |
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

```
SCW_PARITY_DATA=/path/to/parity-data cargo test --release --test iris_golden -- --nocapture
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
crates/scw-vision    image functions (cv), IRIS reader (iris); the finder next
```

Planned beside it: `scw-mining`, `scw-capture`, `scw-input`, `scw-audio`,
`scw-store`, `scw-rsi`, `scw-overlay`, and `sc-watch`, the app that joins them.

Licence: GPL-3.0-or-later, as the rest of sc-watch. The IRIS reader and the
mining overlay are ported from IRIS (github.com/Niteb1rd/IRIS, GPL-3.0).
