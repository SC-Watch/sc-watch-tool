"""The Python side of scw-replay: the same frames through sc-watch's own
reading pipeline with --reader iris - iris_read.IrisReader (no OCR
fallback), watch.Session._tally's sorting, consensus.resolve - written as
the JSON `scw-replay --json` writes, so the two can be compared exactly.

    python reference.py --frames-list F [--scan-area A] [--burst N]
                        [--min-votes N] [--listed F]  > expected.json

Run with the Python environment sc-watch builds with (OpenCV 5.0.0). Reads
frames only; writes nothing but stdout. The output names real players when
the frames show them: keep it with the frames, out of every repository.
"""
import argparse
import json
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())      # never the real data folder
import consensus  # noqa: E402
import imgio  # noqa: E402
import iris_read  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--frames-list", required=True)
ap.add_argument("--scan-area", default="whole", choices=tuple(iris_read.SCAN_AREAS))
ap.add_argument("--burst", type=int, default=3)
ap.add_argument("--min-votes", type=int, default=1)
ap.add_argument("--listed")
args = ap.parse_args()

lst = Path(args.frames_list)
frames = []
for line in lst.read_text(encoding="utf-8").splitlines():
    if line.strip():
        p = Path(line.strip())
        frames.append(p if p.is_absolute() else lst.parent / p)
listed = set()
if args.listed:
    listed = {l.strip().upper() for l in Path(args.listed).read_text(encoding="utf-8").splitlines() if l.strip()}

reader = iris_read.IrisReader(args.scan_area, fallback=False)
per_burst = args.burst if args.burst > 0 else len(frames)
frames_out, bursts_out = [], []
for first in range(0, len(frames), per_burst):
    chunk = frames[first:first + per_burst]
    accumulated, unreadable, discarded, per_frame = [], 0, {}, []
    for path in chunk:
        img = imgio.imread(path)
        found = reader.detect(img)
        stats = reader.last
        frames_out.append({
            "file": path.name,
            "lines": stats.lines,
            "contacts": [{"name": c.name, "kind": c.kind, "km": c.range_km,
                          "name_box": [c.name_box.x, c.name_box.y, c.name_box.w, c.name_box.h],
                          "range_box": [c.range_box.x, c.range_box.y, c.range_box.w, c.range_box.h]}
                         for c in found],
            "refused": [[k, n] for k, n in (stats.refused or {}).items()],
        })
        # watch.Session._tally, with the user's lists as a set of handles.
        named = 0
        for c in found:
            if c.range_km is None:
                continue
            kind = c.kind
            if kind == "world_label" and (c.name or "").upper() in listed:
                kind = "player"
            if kind == "player":
                accumulated.append(consensus.Reading(c.name, c.range_km, c.name_box.cx, c.name_box.y))
                named += 1
            elif kind == "unreadable":
                unreadable += 1
            else:
                discarded[kind] = discarded.get(kind, 0) + 1
        per_frame.append(named)
    resolved = consensus.resolve(accumulated, min_votes=args.min_votes)
    bursts_out.append({
        "frames": list(range(first, first + len(chunk))),
        "unreadable": unreadable,
        "discarded": [[k, n] for k, n in discarded.items()],
        "per_frame": per_frame,
        "resolved": [{"name": r.name, "km": r.range_km, "votes": r.votes, "total": r.total,
                      "variants": [[n, c] for n, c in r.variants]} for r in resolved],
    })
print(json.dumps({"scan_area": args.scan_area, "burst": args.burst, "min_votes": args.min_votes,
                  "frames": frames_out, "bursts": bursts_out}))
