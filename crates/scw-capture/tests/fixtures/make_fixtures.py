"""hdr_capture.py's own conversion tables and one converted frame, for
tests/hdr.rs to require exactly.

    python make_fixtures.py      (sc-watch's build environment: numpy 2.5.2)

Synthetic only: the tables are functions of the SDR white level, and the
frame is random float16 values.
"""
import json
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import hdr_capture  # noqa: E402  (imports numpy only at module level)

WHITES = [80.0, 1.0, 0.5, 200.0, 203.3, 240.0, 360.0, 480.0, 1000.0, 10000.0]
blob = bytearray()
for w in WHITES:
    blob += hdr_capture.lut_for(w).tobytes()
(HERE / "luts.bin").write_bytes(bytes(blob))

rng = np.random.default_rng(20261122)
H, W, PITCH = 5, 6, 7                       # pitch in pixels, wider than the frame
vals = rng.uniform(-0.5, 6.0, size=(H, PITCH, 4)).astype(np.float16)
vals[0, 0] = [np.nan, np.inf, -np.inf, 0]
bits = vals.view(np.uint16)
bgr = hdr_capture.to_bgr(bits[:, :W], hdr_capture.lut_for(240.0))
(HERE / "frame_f16.bin").write_bytes(bits.astype("<u2").tobytes())
(HERE / "frame_bgr.bin").write_bytes(np.ascontiguousarray(bgr).tobytes())
(HERE / "index.json").write_text(json.dumps({"whites": WHITES, "frame": {"h": H, "w": W, "pitch": PITCH,
                                                                        "white": 240.0}}),
                                 encoding="utf-8", newline="\n")
print(f"{len(WHITES)} tables, frame {W}x{H}")
