"""inputs.parse()'s answers - tokens, labels or the error message - for
tests/parse.rs to require exactly. Run on Windows with sc-watch's build
environment (inputs.py loads user32 and winmm when imported)."""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import inputs  # noqa: E402

SPECS = ["", "off", "OFF", "  ", "A", "a", "f9", "F24", "F25", "rctrl", "rctrl+a", "rctrl + joy0.b24",
         "joy0.b1", "joy0.b0", "joy0.b33", "joy0.b007", "joy12.b32", "joy00012.b3", "joyx.b1", "joy.b1",
         "joy0.c1", "joy0.b", "joy0.b1x", "joy0.b1.2", "vid231d:0126.b7", "VID231D:0126.B7", "vid231d0126.b7",
         "vid231:0126.b7", "vid231g:0126.b1", "vid0x12:4567.b1", "vid-fff:0126.b1", "vid1_23:4567.b1",
         "vid__12:4567.b1", "vid12_3:4567.b1", "a++b", "+a", "a+", "numpad3", "NUMPAD10", "space+tab",
         "ctrl+shift+alt+delete", "unknown", "joy0.b1+vid231d:0126.b2+rshift", "it's", "a+'q", 'say"hi',
         "joy0", "vid231d:0126", "joy0.b99999999999999999999", "grave+backslash", "pause", "lalt+ralt",
         "0", "9+z", "Numpad0+numlock", "joy3.b32+joy3.b31", " f1 ", "joy1.B5"]
cases = []
for spec in SPECS:
    try:
        tokens = inputs.parse(spec)
        cases.append({"spec": spec, "ok": True,
                      "tokens": [{"kind": t.kind, "vk": t.vk, "index": t.index, "ident": t.ident,
                                  "button": t.button} for t in tokens],
                      "labels": [t.label() for t in tokens]})
    except inputs.BindError as e:
        cases.append({"spec": spec, "ok": False, "error": str(e)})
(HERE / "parse.json").write_text(json.dumps(cases, indent=0), encoding="utf-8", newline="\n")
print(len(cases), "specs,", sum(c["ok"] for c in cases), "valid")
