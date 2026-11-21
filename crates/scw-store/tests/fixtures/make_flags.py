"""flags.py's own answers, for tests/flags.rs: what odd flags.json contents
load as, every lookup on them, and what save() writes over a file.

    python make_flags.py     (sc-watch's build environment)

Synthetic flags only. Writes flags_cases.json.
"""
import json
import sys
import tempfile
from dataclasses import asdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())      # never the real data folder
import flags  # noqa: E402

RAWS = [None, [], [{"key": "kos", "label": "KOS!!!", "color": "#ff0000", "polarity": "negative", "say": "kill on sight"}],
        [{"key": " Camping ", "label": "Spawn camping", "color": "#abc"}, {"key": "camping", "label": "dup"}],
        [{"key": "pending", "label": "mine"}, {"key": "bad key", "label": "x"}, {"key": "", "label": "y"},
         {"key": "k" * 41, "label": "z"}, "not a dict", 5],
        [{"key": "good_trader", "label": "Good trader", "polarity": "Positive"}, {"key": "seen", "label": "Seen",
                                                                                    "polarity": "meh"}],
        [{"key": "retired_one", "label": ""}, {"key": "old", "label": "Old", "retired": 1}, {"key": "n", "label": None},
         {"key": 7, "label": 3.5, "color": None, "polarity": None, "say": False}],
        [{"key": "long", "label": "L" * 40, "say": "S" * 60, "color": " #A1b2C3 "}],
        [{"key": "note", "label": "My notes", "polarity": "negative"}]]
KEYS = ["camping", "CAMPING ", "pending", "", "nobody_knows", "friendly", "note", "kos", "good_trader", "seen", "none", "7"]

cases = []
tmp = Path(tempfile.mkdtemp())
for i, raw in enumerate(RAWS):
    fl = flags._from_raw(raw)
    p = tmp / f"flags{i}.json"
    if raw is not None:
        p.write_text(json.dumps(raw), encoding="utf-8")
    flags.invalidate()
    entry = {"raw": raw, "loaded": [asdict(f) for f in fl],
             "lookups": [{"key": k, "by_key": asdict(flags.by_key(k, p)), "sign": flags.sign(k, p),
                          "speakable": flags.speakable(k, p)} for k in KEYS],
             "active": [f.key for f in flags.active(p)], "keys": flags.keys(p)}
    # save(): retire one, rename one, drop one, clear a label.
    items = [asdict(f) for f in fl]
    if items:
        items[0]["label"] = ""
        if len(items) > 1:
            items[1]["label"] = "Renamed"
        items = items[:-1] + [{"key": "brand_new", "label": "Brand new", "color": "#123456"}]
    written = flags.save(items, p)
    entry["save_items"] = items
    entry["written"] = [asdict(f) for f in written]
    entry["file_after"] = p.read_text(encoding="utf-8")
    cases.append(entry)
(HERE / "flags_cases.json").write_text(json.dumps(cases, indent=0), encoding="utf-8", newline="\n")
print(len(cases), "flag files,", sum(len(c["lookups"]) for c in cases), "lookups")
