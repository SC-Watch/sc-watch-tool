"""watch.parse_args itself, for tests/cli_ops.rs: settings.json as the
defaults, the command line over them, argparse's errors.

Each case is a settings.json (or none) and an argv; the result is every
argument's value, or argparse's error message and exit code. --help is
left out: the help text is the port's own.

    python make_cli_ops.py      (sc-watch's build environment)

Writes cli_ops.json.
"""
import contextlib
import io
import json
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())       # never the real data folder
import settings as settings_mod  # noqa: E402
import watch  # noqa: E402

DATA = str(paths.DATA_ROOT)

SETTINGS = {
    "none": None,
    "custom": {"trigger": "auto", "key": "rctrl+joy0.b24", "focus_key": "off", "burst": 5,
               "hz": 10, "reader": "iris", "audio_mode": "speech", "rsi_enabled": False,
               "debug_frames": True, "audit_enabled": False, "dual_on_read": True,
               "ping_delay": 1.5, "monitor": 2, "scan_area": "narrow", "min_votes": 2,
               "chat_key": "f8", "queue_max": 1, "hdr_capture": "off", "debug_log": False},
    "odd": {"reader": "ocr", "hz": 0.5, "burst": 99, "audio_mode": "loud", "key": "",
            "opencv_threads": -3, "cooldown": "60", "radius": True},
}

CASES = [
    ("none", []),
    ("custom", []),
    ("odd", []),
    ("none", ["--trigger", "auto"]),
    ("none", ["--trigger=auto"]),
    ("none", ["--trigger", "both"]),
    ("none", ["--trigger"]),
    ("none", ["--trig", "auto"]),
    ("none", ["--key", "TAB", "--key", "f4"]),
    ("none", ["--key", "-1"]),
    ("none", ["--key", "-x"]),
    ("none", ["--key", "--verbose"]),
    ("none", ["--key=-x"]),
    ("none", ["--key="]),
    ("none", ["--key", "a b"]),
    ("none", ["--hz", "7"]),
    ("none", ["--hz", " 7.5 "]),
    ("none", ["--hz", "1_0"]),
    ("none", ["--hz", "inf"]),
    ("none", ["--hz", "abc"]),
    ("none", ["--hz", ""]),
    ("none", ["--hz", "-2"]),
    ("none", ["--hz", "1e1"]),
    ("none", ["--burst", "4"]),
    ("none", ["--burst", "4.0"]),
    ("none", ["--burst", "+4"]),
    ("none", ["--burst", "0x10"]),
    ("none", ["--burst", " 3 "]),
    ("none", ["--seconds", "2.5", "--replay", "D:/frames/*.png"]),
    ("none", ["--verbose", "--debug", "--no-audit", "--no-db", "--no-rsi"]),
    ("custom", ["--rsi"]),
    ("custom", ["--no-debug", "--no-dual-on-read"]),
    ("none", ["--verbose=yes"]),
    ("none", ["--verb"]),
    ("none", ["--ver"]),
    ("none", ["--rs"]),
    ("none", ["--rsi-m", "3"]),
    ("none", ["--no-d"]),
    ("none", ["--no-deb"]),
    ("none", ["--no-db=1"]),
    ("none", ["--focus"]),
    ("none", ["--focus-k", "8"]),
    ("none", ["--reader", "ocr"]),
    ("none", ["--reader", "iris+ocr"]),
    ("none", ["--reader", "IRIS"]),
    ("none", ["--scan-area", "centre", "--auto-scan-area", "whole"]),
    ("none", ["--scan-area", "center"]),
    ("none", ["--audio", "both"]),
    ("none", ["--audio", "loud"]),
    ("none", ["--hdr-capture", "on"]),
    ("none", ["--check-resolution", "a.png", "b.png"]),
    ("none", ["--check-resolution", "a.png", "--verbose", "b.png"]),
    ("none", ["--check-resolution"]),
    ("none", ["--check-resolution=a.png", "b.png"]),
    ("none", ["--where"]),
    ("none", ["--list-inputs"]),
    ("none", ["--support-bundle", "--no-frames"]),
    ("none", ["extra"]),
    ("none", ["extra", "--verbose", "more"]),
    ("none", ["-v"]),
    ("none", ["-5"]),
    ("none", ["-"]),
    ("none", ["--"]),
    ("none", ["--", "--verbose"]),
    ("none", ["--unknown"]),
    ("none", ["--unknown=3", "--verbose"]),
    ("none", ["--bogus", "--trigger", "nope"]),
    ("none", ["--trigger", "nope", "--rs"]),
    ("none", ["-hx"]),
    ("none", ["--log", "elsewhere.csv"]),
    ("none", ["--monitor", "2", "--opencv-threads", "0", "--queue-max", "9"]),
    ("none", ["--min-votes", "2", "--rsi-min-votes", "1", "--cooldown", "30",
              "--radius", "90", "--expire", "5"]),
    ("none", ["--burst-spacing", "0.25", "--ping-delay", "4", "--auto-read-delay", "0"]),
    ("none", ["--focus-size", "800x400", "--focus-mask", "--no-focus-mask", "--gate", "--haze"]),
    ("none", ["--chat-key", "joy0.b24", "--chat-region", "0.1,0.2,0.3,0.4", "--dual-key", "f9"]),
    ("none", ["--dual-on-read", "--focus-always", "--no-debug-log"]),
    ("none", ["--no-gate", "--no-haze"]),
    ("none", ["--hz"]),
    ("none", ["--burst", "--verbose"]),
    ("odd", ["--reader", "iris"]),
]


def run(settings, argv):
    path = settings_mod.CONFIG_PATH
    if settings is None:
        path.unlink(missing_ok=True)
    else:
        path.write_text(json.dumps(settings), encoding="utf-8")
    err = io.StringIO()
    out = io.StringIO()
    try:
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(out):
            args = watch.parse_args(argv)
    except SystemExit as exc:
        text = err.getvalue()
        msg = text.split("error: ", 1)[1].rstrip("\n") if "error: " in text else text
        return {"exit": exc.code, "error": msg}
    v = dict(vars(args))
    for k, x in v.items():                  # JSON has no infinity
        if isinstance(x, float) and x != x or x in (float("inf"), float("-inf")):
            v[k] = repr(x)
    if isinstance(v.get("log"), str) and v["log"].startswith(DATA):
        v["log"] = "<DATA>" + v["log"][len(DATA):]
    return {"args": v}


results = [{"settings": s, "argv": argv, **run(SETTINGS[s], argv)} for s, argv in CASES]
(HERE / "cli_ops.json").write_text(json.dumps({"settings": SETTINGS, "cases": results}, indent=1),
                                   encoding="utf-8", newline="\n")
print(len(results), "cases;", sum("error" in r for r in results), "errors")
