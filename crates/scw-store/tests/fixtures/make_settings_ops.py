"""settings.py's own behaviour, for tests/settings_ops.rs - and the schema.

Writes two files:

  ../../src/settings_schema.json   settings.describe(), which the port reads
                                   as its schema: every setting, its type,
                                   default, range, label and help, as data.
  settings_ops.json                clamp() on every kind of field against
                                   awkward values, load() on crafted files,
                                   and a sequence of save()s with the file
                                   after each.

    python make_settings_ops.py      (sc-watch's build environment)
"""
import json
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import settings  # noqa: E402

(HERE.parents[1] / "src" / "settings_schema.json").write_text(
    json.dumps(settings.describe(), indent=1) + "\n", encoding="utf-8", newline="\n")

VALUES = [True, False, 0, 1, -1, 2, 2.5, 3.5, -0.5, 0.49, 1e300, -1e300, 10 ** 30,
          "4", " 4 ", "2.5", "1e3", "1_000", "_1", "1__0", "+3", "-0", ".5", "5.",
          "on", "Yes", "Y", "off", "no", "", "  ", "nan", "inf", "-Infinity", "abc",
          None, [], [1], {}, {"a": 1}, "auto", "key", "iris+ocr", "IRIS", "f9", "1200x600"]
FIELDS = ["gate", "dual_on_read", "burst", "keep_newest", "port", "max_files_per_dir",
          "burst_spacing", "volume", "max_age_hours", "trigger", "reader", "hdr_capture",
          "key", "chat_region"]
clamps = [{"field": f, "value": v, "result": settings.FIELDS[f].clamp(v)}
          for f in FIELDS for v in VALUES]

ALL_JUNK = {k: "junk" for k in settings.FIELDS}
LOADS = [
    ("missing", None),
    ("empty", "{}"),
    ("bom", "﻿" + json.dumps({"burst": 5, "trigger": "auto"})),
    ("unknown keys", json.dumps({"nope": 1, "burst": 4, "focus_always": True})),
    ("old defaults", json.dumps({"chat_region": " 0.15,0.30,0.35,0.45 ", "focus_size": "1200x600"})),
    ("typed old default", json.dumps({"chat_region": "0.15,0.30,0.35,0.46", "focus_size": 1200})),
    ("nan and infinity", '{"burst_spacing": NaN, "volume": Infinity, "burst": -Infinity}'),
    ("a list", "[1, 2, 3]"),
    ("broken", "{nope"),
    ("all junk", json.dumps(ALL_JUNK)),
    ("everything set", json.dumps({**{k: v for k, v in settings.DEFAULTS.items()},
                                   "burst": 7, "volume": 0.25, "key": "f9", "reader": "ocr",
                                   "monitor": 2, "port": 80})),
]
loads = []
for label, text in LOADS:
    p = paths.DATA_ROOT / f"load_{len(loads)}.json"
    if text is not None:
        p.write_text(text, encoding="utf-8")
    loads.append({"label": label, "text": text, "result": settings.load(p)})

SAVES = [
    {"burst": "6", "volume": 2, "nope": 1},
    {"trigger": "auto", "key": " f10 "},
    {},
    {"chat_region": "0.15,0.30,0.35,0.45"},          # saved as given: not migrated
    {"burst": None, "port": "8000"},
]
saves, sp = [], paths.DATA_ROOT / "saved.json"
for values in SAVES:
    out = settings.save(values, sp)
    saves.append({"values": values, "result": out,
                  "file": json.loads(sp.read_text(encoding="utf-8-sig"))})
# a save over a file that already holds an old default and junk
sp2 = paths.DATA_ROOT / "saved2.json"
sp2.write_text(json.dumps({"focus_size": "1200x600", "burst": "x", "nope": 2}), encoding="utf-8")
over = settings.save({"volume": 0.5}, sp2)

(HERE / "settings_ops.json").write_text(json.dumps({
    "defaults": settings.DEFAULTS, "clamps": clamps, "loads": loads, "saves": saves,
    "save_over": {"start": {"focus_size": "1200x600", "burst": "x", "nope": 2},
                  "result": over, "file": json.loads(sp2.read_text(encoding="utf-8-sig"))},
}, indent=0), encoding="utf-8", newline="\n")
print(len(settings.FIELDS), "settings,", len(clamps), "clamps,", len(loads), "loads,",
      len(saves) + 1, "saves")
