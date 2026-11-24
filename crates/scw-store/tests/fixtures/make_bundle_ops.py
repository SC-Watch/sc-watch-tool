"""debuglog.build_bundle's own behaviour, for tests/bundle_ops.rs.

A made-up data folder - session logs, debug frames, OCR and audit crops,
audit frames, a file that only looks like a PNG, old bundles - written here
with chosen bytes and times, then bundled under several scenarios. For each:
what build_bundle returned, every entry of the zip in order (text entries
whole, untouched images by SHA-1, re-encoded frames by their size), and the
files left in debug_bursts/.

The re-encoded frames are OpenCV's JPEG here and jpeg-encoder's in Rust:
their bytes differ, so they are compared by being a JPEG of the source's
size. The budget scenario keeps the selection independent of the encoder:
its full frames are noise, far over the budget in either.

Every time is distinct: ties are ordered by the directory listing, which
differs between systems.

    python make_bundle_ops.py      (sc-watch's build environment)

Writes bundle_ops.json.
"""
import base64
import hashlib
import json
import os
import shutil
import sys
import tempfile
import zipfile
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

ROOT = Path(tempfile.mkdtemp())
paths.DATA_ROOT = ROOT                    # a made-up data folder, never the real one
import cv2  # noqa: E402
import numpy as np  # noqa: E402
import debuglog  # noqa: E402
import flags  # noqa: E402

NOW = datetime(2026, 11, 24, 21, 5, 9)
EPOCH = 1_795_000_000.0


class FakeDatetime(datetime):
    @classmethod
    def now(cls, tz=None):
        return NOW


debuglog.datetime = FakeDatetime


def png(w, h, seed, noise=False):
    rng = np.random.default_rng(seed)
    if noise:
        img = rng.integers(0, 256, (h, w, 3), dtype=np.uint8)
    else:
        img = np.zeros((h, w, 3), np.uint8)
        img[:, :] = (seed * 37 % 256, seed * 91 % 256, seed * 53 % 256)
        img[h // 4: h // 2, w // 3: w // 2] = (250, 250, 250)
    ok, buf = cv2.imencode(".png", img)
    assert ok
    return buf.tobytes()


def jpg(w, h, seed):
    img = np.full((h, w, 3), seed * 17 % 256, np.uint8)
    ok, buf = cv2.imencode(".jpg", img, [cv2.IMWRITE_JPEG_QUALITY, 70])
    assert ok
    return buf.tobytes()


def log_text(i):
    return (f"========\nsc-watch session log {i}\n-- events\n"
            f"21:0{i % 10}:00.000 burst            frames={i}\n")


def tree_full():
    t = []
    # 14 session logs: the newest 12 go in
    for i in range(14):
        t.append((f"debug_bursts/session_202611{10 + i:02d}_120000.log",
                  log_text(i).encode(), 100.0 + i))
    t += [
        ("debug_bursts/burst_210001_0.png", png(64, 48, 1), 500.0),
        ("debug_bursts/burst_210001_1.png", png(80, 40, 2), 501.0),
        ("debug_bursts/burst_200000_0.png", png(32, 32, 3), 300.0),
        ("debug_bursts/frame_crop_name.png", png(20, 10, 4), 200.0),   # an OCR crop
        ("debug_bursts/broken.png", b"not a png at all", 450.0),      # cannot decode
        ("debug_bursts/probe.jpeg", jpg(16, 16, 5), 460.0),
        ("debug_bursts/sentry_requests.log", b"GET ...\n", 470.0),     # not a session log
        ("debug_bursts/notes.txt", b"hello", 480.0),
        ("debug_bursts/sc-watch-support-20261101_000000.zip", b"PK old1", 10.0),
        ("debug_bursts/sc-watch-support-20261102_000000.zip", b"PK old2", 20.0),
        ("debug_bursts/sc-watch-support-20261103_000000.zip", b"PK old3", 30.0),
        ("audit/20261124_210000_EXMPLHANDLE_crop_name.png", png(30, 12, 6), 600.0),
        ("audit/20261124_210000_EXMPLHANDLE_crop_range.png", png(24, 12, 7), 601.0),
        ("audit/20261124_210000_EXMPLHANDLE_frame.jpg", jpg(64, 36, 8), 602.0),
        ("audit/20261120_100000_EXMPL0NAME_frame.jpg", jpg(48, 27, 9), 50.0),
        ("audit/readme.txt", b"x", 603.0),
        # databases and the key, which no bundle ever holds
        ("sc-watch.db", b"SQLite format 3\0", 700.0),
        ("sc_api_key.txt", b"SECRETKEY123\n", 701.0),
        ("sightings.csv", b"a,b\n", 702.0),
    ]
    return t


def tree_budget():
    t = [("debug_bursts/session_20261124_120000.log", log_text(1).encode(), 100.0)]
    # untouched crops of exact sizes, and frames far over the budget
    t += [
        ("debug_bursts/a_crop_x.png", b"\x89PNG" + b"a" * 996, 300.0),     # 1000 bytes
        ("debug_bursts/b_crop_x.png", b"\x89PNG" + b"b" * 1496, 310.0),    # 1500
        ("debug_bursts/c_crop_x.png", b"\x89PNG" + b"c" * 796, 320.0),     # 800
        ("debug_bursts/burst_210000_0.png", png(256, 256, 11, noise=True), 400.0),
        ("audit/d_frame.jpg", b"\xff\xd8" + b"d" * 598 + b"\xff\xd9", 330.0),  # 602
        ("audit/e_crop_name.png", b"\x89PNG" + b"e" * 296, 290.0),          # 300
    ]
    return t


SETTINGS = {
    "key": "f9",                 # the read key: the redaction rule cannot tell it from a secret
    "focus_key": "9",
    "rsi_source": "sentry",
    "burst": 7,
    "voice_rate": 190,
    "audit": True,
}
FLAGS = [
    {"key": "pirate", "label": "Pirate", "color": "#d9534f", "polarity": "negative",
     "say": "pie rat", "retired": False},
    {"key": "friend", "label": "Friend", "color": "#5cb85c", "polarity": "positive",
     "say": "", "retired": False},
    {"key": "old", "label": "Old one", "color": "#6b7a8c", "polarity": "neutral",
     "say": "", "retired": True},
]


def reset(tree, with_config=True):
    flags.invalidate()    # flags.load() trusts its cache for a second
    for p in list(ROOT.iterdir()):
        if p.is_dir():
            shutil.rmtree(p)
        else:
            p.unlink()
    if with_config:
        (ROOT / "settings.json").write_text(json.dumps(SETTINGS, indent=2))
        (ROOT / "flags.json").write_text(json.dumps(FLAGS, indent=2))
    out = []
    for rel, data, age in tree:
        p = ROOT / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        t = EPOCH + age
        os.utime(p, (t, t))
        out.append({"path": rel, "b64": base64.b64encode(data).decode(), "mtime": t})
    return out


def entries(zpath):
    out = []
    with zipfile.ZipFile(zpath) as z:
        for info in z.infolist():
            data = z.read(info.filename)
            e = {"name": info.filename}
            if info.filename.endswith((".txt", ".log", ".json")):
                e["text"] = data.decode("utf-8")
            elif info.filename.endswith(".jpg") and info.filename.startswith("debug_bursts/burst"):
                img = cv2.imdecode(np.frombuffer(data, np.uint8), cv2.IMREAD_COLOR)
                e["jpeg"] = [int(img.shape[1]), int(img.shape[0])]
            else:
                e["sha1"] = hashlib.sha1(data).hexdigest()
            out.append(e)
    return out


def left():
    d = ROOT / "debug_bursts"
    return sorted(p.name for p in d.iterdir()) if d.is_dir() else []


def run(name, tree, with_frames=True, budget_mb=None, with_config=True):
    files = reset(tree, with_config)
    debuglog.BUNDLE_MB = budget_mb if budget_mb is not None else 24.0
    info = debuglog.build_bundle(with_frames=with_frames)
    assert info["ok"], info
    z = Path(info["file"])
    rec = {
        "name": name,
        "with_frames": with_frames,
        "budget_mb": debuglog.BUNDLE_MB,
        "with_config": with_config,
        "tree": files,
        "file": z.relative_to(ROOT).as_posix(),
        "info": {k: v for k, v in info.items() if k not in ("file", "bytes", "mb")},
        "entries": entries(z),
        "left": left(),
    }
    return rec


scenarios = [
    run("full", tree_full()),
    run("text only", tree_full(), with_frames=False),
    run("budget", tree_budget(), budget_mb=0.003),
    run("empty folder, no settings or flags", [], with_config=False),
]
out = {"now": NOW.isoformat(), "settings": SETTINGS, "flags": FLAGS, "scenarios": scenarios}
(HERE / "bundle_ops.json").write_text(json.dumps(out, indent=1), encoding="utf-8")
print("scenarios", len(scenarios),
      "entries", [len(s["entries"]) for s in scenarios],
      "images", [s["info"]["images"] for s in scenarios])
shutil.rmtree(ROOT, ignore_errors=True)
