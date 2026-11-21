"""jsonstore.py's own answers, for tests/json.rs to require: dumps() text,
float repr, what _parse/read make of raw files, and coerce().

    python make_fixtures.py     (sc-watch's build environment)

Synthetic values only. Writes json_cases.json beside this script; the raw
file contents travel as hex.
"""
import json
import random
import struct
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())      # never the real data folder
import jsonstore  # noqa: E402

rnd = random.Random(20261122)
floats = [0.0, -0.0, 1.0, -2.5, 0.1, 0.0001, 0.00001, 1e-05, 123456789012345.0, 1e15, 1e16, 1.5e16,
          9999999999999998.0, 1e22, 5e-324, 1.7976931348623157e308, 2.2250738585072014e-308, 3.14159,
          100.0, 0.5, 1234.5678, 1e-4, 9.999999999999999e-05, 0.30000000000000004]
floats += [rnd.uniform(-1e6, 1e6) for _ in range(30)] + [10 ** rnd.uniform(-12, 22) for _ in range(30)]
values = [
    {}, [], {"a": 1}, [1, 2, [3, []], {}], {"volume": 0.9, "rate": 175, "voice": "", "muted": ["flag_piracy"]},
    {"nested": {"deeper": {"list": [True, False, None, 1.5, -3, "x"]}}, "after": 2},
    {"unicode": "Café — über 中文", "emoji": "\U0001F680 ok", "ctrl": "a\tb\nc\r\x01\x7f\x1f",
     "quote": "say \"hi\" \\ back/slash", "bs": "\b\f"},
    {"keys order": 1, "b": 2, "a": 3, "": 4}, [0.1, 1e16, -0.0, 1e-07, 12345678901234567890],
    {"big": 9007199254740993, "neg": -42, "zero": 0},
]
cases = {"floats": [{"bits": struct.pack("<d", f).hex(), "repr": repr(f)} for f in floats],
         "dumps": [{"value": json.dumps(v), "text": json.dumps(v, indent=2, allow_nan=False)} for v in values]}

raw_files = [b'{"a": 1}', b'\xef\xbb\xbf{"bom": true}', b'{"a": "Caf\xe9"}', b'{"a": "\x81\x8d\x9d"}',
             b'{"a": NaN, "b": Infinity, "c": -Infinity, "d": [NaN]}', b'{"s": "NaN Infinity"}',
             b'[1, 2, 3]', b'null', b'"just a string"', b'{"a": 1,}', b'', b'   ', b'{"a": }', b'{"dup": 1, "dup": 2}',
             b'{"a": 1} trailing', b'  {"ws": 1}  \n', b'{"x": 1e400}', rb'{"u": "\u00e9\ud83d\ude80"}']
read_cases = []
tmp = Path(tempfile.mkdtemp())
for i, blob in enumerate(raw_files):
    p = tmp / f"case{i}.json"
    p.write_bytes(blob)
    data, problem = jsonstore._parse(p)
    entry = {"hex": blob.hex(), "problem": problem is not None}
    entry["data"] = None if problem else json.dumps(data, allow_nan=True)
    for name, expect in (("dict", dict), ("list", list), ("any", None)):
        got = jsonstore.read(p, expect)
        entry["read_" + name] = None if got is None else json.dumps(got, allow_nan=True)
    read_cases.append(entry)
cases["read"] = read_cases

coerce_values = [0, 1, -5, 2.0, 2.5, 0.9, True, False, "s", "", [], [1], {}, {"k": 1}, None]
cases["coerce"] = [{"default": json.dumps(d), "value": json.dumps(v), "result": json.dumps(jsonstore.coerce(d, v))}
                   for d in (175, 0.9, True, "voice", ["x"], {"a": 1}, None) for v in coerce_values]
(HERE / "json_cases.json").write_text(json.dumps(cases, indent=0), encoding="utf-8", newline="\n")
print(len(cases["floats"]), "floats,", len(cases["dumps"]), "dumps,", len(read_cases), "files,",
      len(cases["coerce"]), "coerce pairs")
