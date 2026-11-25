"""Synthetic cases for IRIS's mining reading logic (scw-vision::mining):
mining_units_in.json. IRIS's own C# answers them - the bench harness's
`--mining-units` mode writes mining_units_out.json - and tests/mining_units.rs
holds the port to those answers, bit for bit.

Every double is stored as its 64-bit pattern (an integer), every patch as
base64 of little-endian float32s, so nothing is rounded on the way.

    python make_mining_units.py        (no packages needed; seeded)
"""
import base64
import json
import random
import struct
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODELS = HERE.parents[3] / "models"
rng = random.Random(20261125)
W, H = 14, 20


def bits(x):
    return struct.unpack("<Q", struct.pack("<d", float(x)))[0]


def f32(x):
    return struct.unpack("<f", struct.pack("<f", x))[0]


def shapes():
    out, section = {}, None
    for line in (MODELS / "iris-mining-digits.txt").read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            section = line[1:-1]
            continue
        d, b = line.split(":")
        out.setdefault(section, []).append((int(d), [v / 255 for v in base64.b64decode(b)]))
    return out


SHAPES = shapes()
ALL = [s for sec in SHAPES.values() for s in sec]


def patch_b64(values):
    return base64.b64encode(b"".join(struct.pack("<f", f32(v)) for v in values)).decode()


def shifted(p, dx, dy):
    q = [0.0] * (W * H)
    for y in range(H):
        for x in range(W):
            sx, sy = x - dx, y - dy
            if 0 <= sx < W and 0 <= sy < H:
                q[y * W + x] = p[sy * W + sx]
    return q


def noisy(p, scale, sd):
    return [min(1.0, max(0.0, v * scale + rng.gauss(0, sd))) for v in p]


# -- patches: match_digit / best_score ---------------------------------------
patches = []
for _ in range(110):
    d, p = rng.choice(ALL)
    p = shifted(p, rng.randint(-1, 1), rng.randint(-1, 1))
    p = noisy(p, rng.uniform(0.6, 1.0), rng.uniform(0.0, 0.15))
    patches.append({"patch": patch_b64(p), "tie": bits(0.08), "small": rng.random() < 0.4})
for _ in range(70):
    (da, a), (db, b) = rng.sample(ALL, 2)
    t = rng.uniform(0.35, 0.65)
    p = noisy([t * x + (1 - t) * y for x, y in zip(a, b)], 1.0, rng.uniform(0.0, 0.05))
    patches.append({"patch": patch_b64(p), "tie": bits(rng.choice([0.08, 0.3, 1.0])),
                    "small": rng.random() < 0.4})
for _ in range(16):
    p = [rng.random() for _ in range(W * H)]
    patches.append({"patch": patch_b64(p), "tie": bits(rng.choice([0.08, 1.0])), "small": False})
for _ in range(6):  # a digit's negative: every shift below the floor of -1
    d, p = rng.choice(ALL)
    patches.append({"patch": patch_b64([1.0 - v for v in p]), "tie": bits(0.08), "small": False})
for v in (0.0, 0.5):
    patches.append({"patch": patch_b64([v] * (W * H)), "tie": bits(0.08), "small": False})

# -- tables --------------------------------------------------------------------
embedded = (MODELS / "iris-mining.txt").read_text()
tables = [
    embedded,
    embedded.replace("\n", "\r\n"),
    "# IRIS mining data test-1\n  # IRIS mining data second\n1,000/A/3\n 2,000 / B / 2x \n3000/C/1X\n",
    "bad line\n1/2/3/4\n,/Name/2\n-5/Neg/2\n100//2\n100/Zero/0\n100/Big/100\n100/Word/x\n2147483647/Max/2\n1073741824/Half/3\n50/Ok/99\n",
    "  \n#only comments\n# IRIS mining data \n",
    "1,170/Shared/4\n2,340/Other/2\n4,680/Third/1\n",
]
lookups = []
for t in tables:
    vals = set()
    for line in t.replace("\r", "").split("\n"):
        parts = line.strip().split("/")
        if len(parts) == 3 and parts[0].strip().replace(",", "").isdigit():
            base = int(parts[0].strip().replace(",", ""))
            for n in range(0, 12):
                for dv in (-1, 0, 1):
                    v = base * n + dv
                    if -10 <= v <= 2**31 - 1:
                        vals.add(v)
    vals |= {0, 1, 999, 1000, 26000, 30000, 31000, 45000, 47123, 100000, 2**31 - 1}
    lookups.append(sorted(vals))

# -- reads ---------------------------------------------------------------------
TABLE_VALUES = sorted({int(l.split("/")[0].replace(",", "")) * n
                       for l in embedded.splitlines() if l and not l.startswith("#")
                       for n in range(1, int(l.split("/")[2]) + 1)})


def fmt(v, commas):
    return f"{v:,}" if commas else str(v)


def scores_for(text, strong=(0.82, 0.97), weak=(0.05, 0.5)):
    out = []
    for c in text:
        if not c.isdigit():
            continue
        s = [rng.uniform(*weak) for _ in range(10)]
        s[int(c)] = rng.uniform(*strong)
        out.append(s)
    return out


def read(state="Signal", text="", value=None, scores=None, **kw):
    return {
        "state": state, "text": text, "value": value,
        "box": [bits(kw.get("x", 100.5)), bits(kw.get("y", 470.0)), bits(kw.get("w", 80.0)),
                bits(kw.get("h", 18.0))],
        "row_top": bits(kw.get("row_top", 471.0)), "dh": bits(kw.get("dh", 12.0)),
        "tc": kw.get("tc", [200, 230, 60]), "bc": kw.get("bc", [20, 25, 30]),
        "off": bits(kw.get("off", 1.5)), "min": bits(kw.get("min", 0.8)),
        "scores": None if scores is None else [list(s) for s in scores],
    }


def encode_scores(node):
    """Every read's digit scores as base64 of little-endian doubles, ten a digit."""
    if isinstance(node, dict):
        for k, v in node.items():
            if k == "scores" and isinstance(v, list):
                flat = [x for s in v for x in s]
                node[k] = base64.b64encode(struct.pack(f"<{len(flat)}d", *flat)).decode()
            else:
                encode_scores(v)
    elif isinstance(node, list):
        for v in node:
            encode_scores(v)


def number_read(v, commas=None, state="Signal"):
    commas = rng.random() < 0.6 if commas is None else commas
    text = fmt(v, commas)
    return read(state, text, v if state == "Signal" else None, scores_for(text))


snaps = []
for _ in range(160):
    v = rng.choice(TABLE_VALUES)
    commas = rng.random() < 0.6
    text = fmt(v, commas)
    digits = [i for i, c in enumerate(text) if c.isdigit()]
    sc = scores_for(text)
    kind = rng.random()
    place = rng.randrange(len(digits))
    true_d = int(text[digits[place]])
    wrong = rng.choice([d for d in range(10) if d != true_d])
    shown = list(text)
    if kind < 0.6:  # the true digit a close second: snap finds the table value
        shown[digits[place]] = str(wrong)
        s = sc[place]
        s[wrong] = rng.uniform(0.7, 0.9)
        s[true_d] = s[wrong] - rng.uniform(0.0, 0.1)
    elif kind < 0.75:  # two doubtful digits: no snap
        p2 = rng.choice([p for p in range(len(digits)) if p != place] or [place])
        for p in {place, p2}:
            d = int(text[digits[p]])
            alt = (d + 1) % 10
            sc[p][alt] = sc[p][d] - rng.uniform(0.0, 0.05)
    elif kind < 0.85:  # a weak best: no snap
        sc[place][true_d] = rng.uniform(0.3, 0.54)
    elif kind < 0.92:  # several alternatives near
        for d in range(10):
            if d != true_d:
                sc[place][d] = sc[place][true_d] - rng.uniform(0.0, 0.09)
    # else: confident, untouched
    t = "".join(shown)
    value = int(t.replace(",", "")) if t.replace(",", "").isdigit() else None
    state = rng.choice(["Signal", "Signal", "Signal", "Unreadable", "None", "Disqualified"])
    snaps.append(read(state, t, value if state == "Signal" else None, sc, min=0.5))
for _ in range(12):  # an unreadable table value, one digit just past the doubt margin
    v = rng.choice(TABLE_VALUES)
    text = fmt(v, rng.random() < 0.5)
    sc = scores_for(text)
    place = rng.randrange(len(sc))
    d0 = int([c for c in text if c.isdigit()][place])
    alt = (d0 + 1) % 10
    sc[place][alt] = sc[place][d0] - rng.uniform(0.0805, 0.0895)
    snaps.append(read("Unreadable", text, None, sc, min=0.5))
snaps.append(read("Signal", "12,34", 1234, scores_for("12,34")))       # not a number
snaps.append(read("Signal", "7080", 7080, scores_for("708")))          # scores short
snaps.append(read("Signal", "7080", 7080, []))                          # no scores
snaps.append(read("Signal", "7080", 7080, None))

# -- evidence: fusion over a sequence ------------------------------------------
evidence = []
for _ in range(40):
    seq = []
    v = rng.choice(TABLE_VALUES)
    commas = rng.random() < 0.5
    for _ in range(rng.randint(1, 9)):
        r = rng.random()
        if r < 0.1:
            v = rng.choice(TABLE_VALUES)
        if r < 0.15:
            commas = not commas
        text = fmt(v, commas)
        sc = scores_for(text, strong=(0.55, 0.9), weak=(0.1, 0.7))
        state = rng.choice(["Signal"] * 6 + ["Unreadable", "None", "Disqualified"])
        seq.append(read(state, text, v if state == "Signal" else None, sc))
    evidence.append({"max": rng.choice([1, 2, 3, 5]), "min_digit": bits(0.55), "reads": seq})

# -- the label's state over reads -----------------------------------------------
label_states = []
for _ in range(50):
    t = t0 = float(rng.randrange(0, 10**6))
    v0 = rng.choice(TABLE_VALUES + [None, None])
    steps = []
    current = v0 or rng.choice(TABLE_VALUES)
    for _ in range(rng.randint(3, 25)):
        t += rng.choice([83.3, 83.3, 83.3, 40.0, 300.0, 1100.0, -20.0, 1000.0, 1030.0, 990.0])
        r = rng.random()
        if r < 0.15:
            current = rng.choice(TABLE_VALUES)
        elif r < 0.22:
            current = rng.choice([current + 1, current * 2 + 7, 99999, 48000, 51234])
        k = rng.random()
        if k < 0.6:
            rd = number_read(current)
            if rng.random() < 0.3 and rd["scores"]:  # a digit not clearly different
                p = rng.randrange(len(rd["scores"]))
                fl = rd["scores"][p]
                d = max(range(10), key=lambda i: fl[i])
                fl[(d + 3) % 10] = fl[d] - rng.uniform(0, 0.1)
        elif k < 0.7:
            rd = read("Unreadable", "7,O80")
        elif k < 0.78:
            rd = read("Disqualified", "1,234")
        elif k < 0.85:
            rd = read("Signal", "", None)
        else:
            rd = read("None")
        steps.append({"read": rd, "frame_ms": bits(t)})
    # the lock's own label is the table's for its value (as MiningState.FromLock)
    label_states.append({"locked_ms": bits(t0), "value": v0, "steps": steps})

# -- geometry ------------------------------------------------------------------
geometry = []
for _ in range(30):
    locked = read("Signal", "7,080", 7080, x=rng.uniform(2000, 2600), y=rng.uniform(440, 500),
                  w=rng.uniform(60, 120), h=rng.uniform(14, 22), row_top=rng.uniform(440, 500),
                  dh=rng.choice([0.0, rng.uniform(9, 18)]))
    steps = []
    bx = [struct.unpack("<d", struct.pack("<Q", b))[0] for b in locked["box"]]
    for _ in range(rng.randint(1, 9)):
        j = rng.choice([0.0, 0.4, 1.0, 3.0])
        steps.append([bits(bx[0] + rng.uniform(-j, j)), bits(bx[1] + rng.uniform(-j, j)),
                      bits(bx[2] + rng.uniform(-j, j)), bits(bx[3] + rng.uniform(-j, j)),
                      bits(rng.uniform(440, 500)), bits(rng.uniform(8, 20))])
    geometry.append({"locked": locked, "steps": steps})

# -- colours -------------------------------------------------------------------
def colour():
    return [rng.randrange(256) for _ in range(3)]


colours = []
for _ in range(30):
    t = float(rng.randrange(0, 10**5))
    steps = []
    for _ in range(rng.randint(1, 12)):
        t += rng.choice([83.3, 200.0, 250.0, 250.0, 260.0, 1000.0])
        steps.append({"text": colour(), "back": colour(), "now": bits(t)})
    colours.append({"text": colour(), "back": colour(), "steps": steps})
readable = [{"text": colour(), "back": colour(), "ratio": bits(rng.choice([1.0, 3.0, 4.5, 7.0, 21.0]))}
            for _ in range(120)]
readable += [{"text": [255, 255, 255], "back": [255, 255, 255], "ratio": bits(3.0)},
             {"text": [0, 0, 0], "back": [0, 0, 0], "ratio": bits(3.0)}]

from_lock = [{"read": number_read(rng.choice(TABLE_VALUES + [12345, 99]), state=rng.choice(["Signal", "Unreadable"])),
              "frame_ms": bits(rng.uniform(0, 1e6))} for _ in range(30)]
for fl in from_lock:
    fl["read"]["tc"], fl["read"]["bc"] = colour(), colour()

margins = [number_read(rng.choice(TABLE_VALUES), state=rng.choice(["Signal", "Unreadable"])) for _ in range(30)]
margins += [read("Signal", "7080", 7080, None), read("Signal", "7080", 7080, [])]

numbers = ["", "1", "12", "123", "1234", "12345", "123456", "1234567", "1,234", "12,345", "123,456",
           "1,234,567", "1234,567", ",123", "1,23", "1,2345", "12a4", "1,2a4", "0000", "00,000", "1,,234"]

clearly = []
for _ in range(60):
    v = rng.choice(TABLE_VALUES)
    rd = number_read(v, commas=False)
    shown = rng.choice([v, v + 1, v + 10, v * 10, rng.choice(TABLE_VALUES)])
    clearly.append({"read": rd, "shown": shown, "margin": bits(rng.choice([0.0, 0.08, 0.3]))})

out = {
    "about": "made by make_mining_units.py; IRIS answers with the bench harness --mining-units",
    "patches": patches, "tables": tables, "lookups": lookups, "snaps": snaps,
    "evidence": evidence, "label_states": label_states, "geometry": geometry,
    "colours": colours, "readable": readable, "from_lock": from_lock, "margins": margins,
    "numbers": numbers, "clearly": clearly,
}
encode_scores(out)
(HERE / "mining_units_in.json").write_text(json.dumps(out, separators=(",", ":")) + "\n")
print(f"{len(patches)} patches, {len(tables)} tables, {len(snaps)} snaps, {len(evidence)} evidence, "
      f"{len(label_states)} label states, {len(geometry)} geometry, {len(colours)} colours, "
      f"{len(readable)} readable, {len(from_lock)} locks, {len(margins)} margins")
