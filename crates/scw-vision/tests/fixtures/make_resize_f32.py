"""Float resizes as IRIS's mining reader uses them - each digit cell fitted
to 14 x 20 (INTER_LINEAR) and the pin template scaled to each height
(INTER_AREA) - on synthetic images: resize_f32_in.json.

Two answers. resize_f32_noipp.json is OpenCV's own code (IPP off), written
here when cv2 is importable (the dev tree's .venv-build: OpenCV 5.0.0);
scw-vision::cv must match it exactly. resize_f32_out.json is IRIS's build
of OpenCV (the bench harness's --resize-f32), which hands INTER_LINEAR on
floats to Intel's IPP - whose code, and so its last bit, depends on the
processor (measured: within 1.1e-6 of OpenCV's own here) - so it is held to
a tolerance. tests/mining_units.rs checks both.

    .venv-build/Scripts/python.exe make_resize_f32.py        (seeded)
"""
import base64
import json
import random
import struct
from pathlib import Path

HERE = Path(__file__).resolve().parent
rng = random.Random(20261126)


def f32s(values):
    return base64.b64encode(b"".join(struct.pack("<f", v) for v in values)).decode()


cases = []
for _ in range(120):  # digit cells: about 8-20 wide, 10-30 tall, to 14 x 20
    w, h = rng.randint(4, 24), rng.randint(6, 32)
    kind = rng.random()
    if kind < 0.5:   # ink scaled by a percentile and clipped, as Patch does
        v = [min(1.0, rng.randrange(256) / rng.uniform(60, 255)) for _ in range(w * h)]
    elif kind < 0.8:
        v = [rng.random() for _ in range(w * h)]
    else:
        v = [rng.choice([0.0, 1.0, 0.5]) for _ in range(w * h)]
    cases.append({"w": w, "h": h, "dw": 14, "dh": 20, "mode": "linear", "data": f32s(v)})
for (w, h, dw, dh) in [(14, 20, 14, 20), (7, 10, 14, 20), (28, 40, 14, 20), (3, 3, 14, 20)]:
    cases.append({"w": w, "h": h, "dw": dw, "dh": dh, "mode": "linear",
                  "data": f32s([rng.random() for _ in range(w * h)])})
pin = base64.b64decode(
    "DAkLCQYGBAUNEA4OCw4KEAwEBAQBCwgLCAYGBTSSkJmQji0KDAkEBAQCDAkHBwYFRpiblZqRmY5UCgsFBAMACwkGBQYwmZJpLCo2c5ORCAgFBAQCDAYJCQqHmXwACQoMCoKYXgcGBQcDCQQHCgePmE8DDA0NC3WYegQFBQYDBgYDBwePlkcGCg4OC3KYfwAGBQgEBgIEBQiFmXQGCxIODYeaYgAGBQQDAgEEBwgylpBOFxYZXI+TAQIGBgYCAwIBCAgLhJGZjJSMlZBuAAcKBwgEAQQCCAoMI4yVjpiNl4MDBQgHCAYCAAUEBgkMD2eVjJeMlUoACQsGBgcCAgYGCQ4NERSRkJuOgQULDQ0ICQkFBQMHBwoPEhBMjJiMNAQOCgwGBgYDBAQICQ0QEQ8Uf5lsAAkSCwsJCQcEBgYIDAwNExATOJILCRASDAwICAgGCQMIBwoLDg4SD0kFDgwPCwkHCAQEBQQEETBCVG0jCxELFGhJMSIOBAQDBQaGjpOTlpFyChUOfpCUjpGLeQQEBpCXlJaUmZKSNhIrlJGbkpaTlYMAB4+TkJOPlo+VjJeMlY2Vj5KQlI0FCWSNlJSQlpGVjJaMlYuUj5KNik8ACwcMUnB6hYSOiZSJjYeHfHFYCAAJCAoICQYFAgEFBAMFAAMABQIGBgwO")
for th in range(4, 40):  # the pin at every height the reader may ask for
    tw = max(3, round(th * 21 / 24))
    cases.append({"w": 21, "h": 24, "dw": tw, "dh": th, "mode": "area", "data": f32s([float(b) for b in pin])})
(HERE / "resize_f32_in.json").write_bytes((json.dumps(cases) + "\n").encode())
print(len(cases), "cases")

try:
    import cv2
    import numpy as np
except ImportError:
    print("no cv2: resize_f32_noipp.json left as it is")
else:
    cv2.ipp.setUseIPP(False)
    answers = []
    for c in cases:
        src = np.frombuffer(base64.b64decode(c["data"]), "<f4").reshape(c["h"], c["w"]).copy()
        mode = cv2.INTER_LINEAR if c["mode"] == "linear" else cv2.INTER_AREA
        r = cv2.resize(src, (c["dw"], c["dh"]), interpolation=mode)
        answers.append(base64.b64encode(r.astype("<f4").tobytes()).decode())
    (HERE / "resize_f32_noipp.json").write_bytes((json.dumps(answers) + "\n").encode())
    print("resize_f32_noipp.json from OpenCV", cv2.__version__, "without IPP")
