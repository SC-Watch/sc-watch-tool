"""OpenCV's own outputs for the image functions scw-vision reimplements.

    python make_fixtures.py            (needs requirements.txt's pinned
                                        opencv-python 5.0.0.93 and numpy 2.5.2)

Every input is SYNTHETIC - random noise, sparse blobs, strokes, gradients -
made from fixed seeds, so the fixtures carry nothing from a game frame and can
be committed. The Rust tests (tests/opencv_parity.rs) run the same inputs
through scw-vision and require byte-identical output.

All arrays go in one file, opencv.bin; each is b"SCWA", u8 dtype (0 u8,
1 i32, 2 f32, 3 f64), u8 ndim, ndim x u32 dims (row-major, channels last),
then the data, little-endian. index.json lists every case: op, params, and the
byte offsets of its inputs and outputs in opencv.bin.
"""
import json
import struct
from pathlib import Path

import cv2
import numpy as np

assert cv2.__version__.startswith("5.0.0"), cv2.__version__
HERE = Path(__file__).resolve().parent
DT = {np.dtype(np.uint8): 0, np.dtype(np.int32): 1, np.dtype(np.float32): 2, np.dtype(np.float64): 3}
cases = []


BLOB = bytearray()


def save(name: str, a: np.ndarray) -> int:
    """Appends one array to the single data file; returns its byte offset."""
    a = np.ascontiguousarray(a)
    at = len(BLOB)
    BLOB.extend(b"SCWA" + struct.pack("<BB", DT[a.dtype], a.ndim))
    BLOB.extend(struct.pack(f"<{a.ndim}I", *a.shape))
    BLOB.extend(a.astype(a.dtype.newbyteorder("<")).tobytes())
    return at


def case(op, name, inputs, outputs, **params):
    cases.append({"op": op, "name": name, "params": params,
                  "inputs": [save(f"{name}_in{i}", a) for i, a in enumerate(inputs)],
                  "outputs": [save(f"{name}_out{i}", a) for i, a in enumerate(outputs)]})


rng = np.random.default_rng(20261121)


def noise(h, w, c=None, lo=0, hi=256):
    shape = (h, w) if c is None else (h, w, c)
    return rng.integers(lo, hi, size=shape, dtype=np.uint8)


def sparse(h, w, p=0.08, value=255):
    return ((rng.random((h, w)) < p) * value).astype(np.uint8)


def strokes(h, w):
    """Text-like: a few bright bars and dots on a dim, noisy ground."""
    a = noise(h, w, lo=0, hi=40)
    for _ in range(max(2, (h * w) // 150)):
        y, x = rng.integers(0, h), rng.integers(0, w)
        hh, ww = rng.integers(1, max(2, h // 3)), rng.integers(1, max(2, w // 4))
        a[y:y + hh, x:x + ww] = rng.integers(150, 256)
    return a


def blobs(h, w):
    """Binary shapes for labelling: rectangles, diagonals, U shapes."""
    a = np.zeros((h, w), np.uint8)
    for _ in range(max(3, (h * w) // 120)):
        y, x = rng.integers(0, h), rng.integers(0, w)
        a[y:y + rng.integers(1, 6), x:x + rng.integers(1, 6)] = 255
    for k in range(min(h, w) // 3):                      # a diagonal: 8-connected only
        a[k, min(w - 1, k + w // 2)] = 255
    if h > 8 and w > 8:                                  # a U that joins late in a scan
        a[h - 6:h - 1, 1] = a[h - 6:h - 1, 5] = 255
        a[h - 2, 1:6] = 255
    return a


SIZES = [(1, 1), (2, 3), (5, 4), (23, 17), (37, 53), (64, 48)]

# ---- threshold, subtract, LUT, count, bounding rect --------------------------
for h, w in SIZES:
    src = noise(h, w)
    for thr in (0, 1, 25.5, 90, 150.7, 254, 255):
        for maxval in (1, 255):
            _, dst = cv2.threshold(src, thr, maxval, cv2.THRESH_BINARY)
            case("threshold_binary", f"thr_{h}x{w}_{str(thr).replace('.', 'p')}_{maxval}", [src], [dst],
                 thresh=thr, maxval=maxval)
    a, b = noise(h, w), noise(h, w)
    case("subtract", f"sub_{h}x{w}", [a, b], [cv2.subtract(a, b)])
    table = noise(1, 256).reshape(256)
    case("lut", f"lut_{h}x{w}", [src, table], [cv2.LUT(src, table)])
    for p in (0.0, 0.01, 0.2):
        m = sparse(h, w, p)
        x, y, bw, bh = cv2.boundingRect(m)
        case("count_bounding", f"cnt_{h}x{w}_{int(p * 100)}", [m],
             [np.array([cv2.countNonZero(m), x, y, bw, bh], np.int32)])

# ---- Otsu ----------------------------------------------------------------------
otsu_inputs = {
    "uniform": noise(40, 30),
    "bimodal": np.where(rng.random((40, 30)) < 0.3, noise(40, 30, lo=180, hi=230),
                        noise(40, 30, lo=10, hi=60)).astype(np.uint8),
    "constant": np.full((20, 20), 77, np.uint8),
    "two_values": np.where(rng.random((25, 25)) < 0.5, 3, 200).astype(np.uint8),
    "sparse_hat": sparse(60, 90, 0.05, 140) + noise(60, 90, lo=0, hi=12),
    "strokes": strokes(48, 120),
    "single_pixel": np.zeros((1, 1), np.uint8),
}
for k, src in otsu_inputs.items():
    t, dst = cv2.threshold(src, 0, 255, cv2.THRESH_BINARY | cv2.THRESH_OTSU)
    case("otsu", f"otsu_{k}", [src], [np.array([t], np.float64), dst])

# ---- erode, dilate, top-hat, open ------------------------------------------------
KERNELS = [(1, 1), (3, 3), (1, 3), (3, 1), (2, 2), (4, 3), (5, 2), (10, 1), (1, 10), (7, 7), (31, 31)]
for h, w in [(5, 4), (23, 17), (37, 53)]:
    for kind, src in (("noise", noise(h, w)), ("sparse", sparse(h, w, 0.15)), ("strokes", strokes(h, w))):
        for kw, kh in KERNELS:
            k = cv2.getStructuringElement(cv2.MORPH_RECT, (kw, kh))
            tag = f"{h}x{w}_{kind}_k{kw}x{kh}"
            case("erode", f"erode_{tag}", [src], [cv2.erode(src, k)], kw=kw, kh=kh)
            case("dilate", f"dilate_{tag}", [src], [cv2.dilate(src, k)], kw=kw, kh=kh)
            case("tophat", f"tophat_{tag}", [src], [cv2.morphologyEx(src, cv2.MORPH_TOPHAT, k)], kw=kw, kh=kh)
            case("open", f"open_{tag}", [src], [cv2.morphologyEx(src, cv2.MORPH_OPEN, k)], kw=kw, kh=kh)

# ---- resize --------------------------------------------------------------------------
for h, w in [(1, 1), (3, 2), (13, 37), (24, 61)]:
    for c in (1, 3):
        src = noise(h, w) if c == 1 else noise(h, w, 3)
        big = cv2.resize(src, None, fx=4, fy=4, interpolation=cv2.INTER_CUBIC)
        case("resize_cubic", f"cubic4_{h}x{w}x{c}", [src], [big], fx=4)
    f = rng.random((h, w)).astype(np.float32) * 255
    case("resize_cubic_f32", f"cubic8f_{h}x{w}", [f],
         [cv2.resize(f, None, fx=8, fy=8, interpolation=cv2.INTER_CUBIC)], fx=8)
for (h, w), (dw, dh) in [((24, 16), (16, 24)), ((30, 20), (16, 24)), ((53, 37), (16, 24)), ((17, 9), (16, 24)),
                         ((96, 64), (16, 24)), ((48, 32), (16, 24)), ((100, 300), (13, 34)),
                         ((1440, 640), (90, 40)), ((99, 101), (7, 8)), ((64, 64), (16, 16))]:
    src = strokes(h, w)
    case("resize_area", f"area_{h}x{w}_to_{dw}x{dh}", [src],
         [cv2.resize(src, (dw, dh), interpolation=cv2.INTER_AREA)], dw=dw, dh=dh)
for (h, w), (dw, dh) in [((4, 3), (17, 9)), ((13, 34), (100, 300)), ((90, 40), (1440, 640)), ((7, 8), (99, 101)),
                         ((5, 5), (5, 5)), ((1, 1), (6, 4)), ((40, 90), (640, 1440))]:
    src = noise(h, w)
    case("resize_linear", f"linear_{h}x{w}_to_{dw}x{dh}", [src],
         [cv2.resize(src, (dw, dh), interpolation=cv2.INTER_LINEAR)], dw=dw, dh=dh)

# ---- box blur --------------------------------------------------------------------------
for h, w in [(5, 4), (13, 34), (40, 90), (90, 40)]:
    src = noise(h, w)
    for k in (3, 5, 15):
        case("blur", f"blur_{h}x{w}_k{k}", [src], [cv2.blur(src, (k, k))], k=k)

# ---- connected components ------------------------------------------------------------
for h, w in [(1, 1), (2, 2), (7, 9), (23, 17), (37, 53), (64, 128)]:
    for kind, m in (("blobs", blobs(h, w)), ("sparse", sparse(h, w, 0.3))):
        for conn in (4, 8):
            n, labels, stats, _ = cv2.connectedComponentsWithStats(m, connectivity=conn)
            case("components", f"cc{conn}_{h}x{w}_{kind}", [m],
                 [labels.astype(np.int32), stats.astype(np.int32)], connectivity=conn)

# ---- colour --------------------------------------------------------------------------
for h, w in [(1, 1), (13, 37)]:
    src = noise(h, w, 3)
    case("bgr2gray", f"gray_{h}x{w}", [src], [cv2.cvtColor(src, cv2.COLOR_BGR2GRAY)])

# ---- float area resize: the glyph features' 16 x 24 letter image -----------------
for h, w in [(24, 16), (48, 32), (72, 48), (30, 20), (53, 37), (17, 9), (40, 7), (10, 30), (96, 64),
             (25, 17), (41, 11), (9, 5), (1, 1), (60, 90), (47, 16), (24, 3)]:
    for kind, src in (("mask", (rng.random((h, w)) < 0.35).astype(np.float32)),
                      ("rand", rng.random((h, w)).astype(np.float32))):
        case("resize_area_f32", f"areaf_{h}x{w}_{kind}", [src],
             [cv2.resize(src, (16, 24), interpolation=cv2.INTER_AREA)], dw=16, dh=24)

# ---- the IRIS letter model and reader, through the Python port ---------------------
# The Python port (iris_reader.py at the repository root) matches IRIS's own
# readings on the real corpus; here it reads SYNTHETIC rows - OpenCV's Hershey
# fonts on noise, placeholder names and random strings - so the whole reader
# can be checked in CI without a game frame.
import sys
import tempfile
import zlib

ROOT = HERE.parents[3]
sys.path.insert(0, str(ROOT))
import paths                                    # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())      # nothing here writes data; never the real folder
import iris_reader as ir                        # noqa: E402

model = ir.LetterModel.load(ROOT / "models" / "iris-letters-model.bin")


def logits_csharp(x):
    """IRIS's forward pass in its own order: biases first, then each input
    row in turn (zero inputs skipped), multiply and add rounded separately."""
    cur = x
    last = len(model.layers) - 1
    for k, (w, b) in enumerate(model.layers):
        nxt = b.copy()
        for i in range(w.shape[0]):
            if cur[i] != 0.0:
                nxt += cur[i] * w[i]
        cur = np.maximum(nxt, 0.0) if k < last else nxt
    return cur


vectors = [np.zeros(ir.VECTOR_LENGTH)]
for _ in range(12):
    v = np.zeros(ir.VECTOR_LENGTH)
    v[:384] = (rng.random(384) < 0.3) * rng.random(384)
    v[384:] = rng.uniform(-1.0, 6.0, 4)
    vectors.append(v)
for k, v in enumerate(vectors):
    case("letter_model", f"model_{k:02d}", [v], [logits_csharp(v), model.log_probs(v)])

rr = ir.RowReader(ir.NameplateReader(model))
FONTS = (cv2.FONT_HERSHEY_SIMPLEX, cv2.FONT_HERSHEY_DUPLEX)
COLOURS = ((230, 230, 230), (255, 190, 90), (70, 70, 225), (120, 230, 120))


def render(text, scale, colour, font, spacing=None):
    (tw, th), base = cv2.getTextSize(text, font, scale, 1)
    width = tw + 40 + 6 * len(text)
    img = noise(th + base + 24, width, 3, lo=8, hi=45)
    x0, y0 = 20, 12 + th
    if spacing is None:
        cv2.putText(img, text, (x0, y0), font, scale, colour, 1, cv2.LINE_AA)
    else:
        x = x0
        for ch in text:
            cv2.putText(img, ch, (x, y0), font, scale, colour, 1, cv2.LINE_AA)
            x += cv2.getTextSize(ch, font, scale, 1)[0][0] + spacing
        tw = x - x0
    return img, (x0 - 2, y0 - th - 2, max(1, tw) + 4, th + base + 4)


def pieces_json(read):
    return {"cap": read.cap, "spans": [list(s) for s in read.spans],
            "pieces": [[p.start, p.end, p.text, p.score, p.free_score] for p in read.pieces]}


NAME_CHARS = ir.NAME_ALPHABET
texts = ["EXMPL0NAME", "EXMPLHANDLE", "EXMPLORG", "O0O0_-0O", "A_B-C", "123.4km", "56.7m",
         "1000.0km", "7.0m", "0.5km", "12.3km [", "AB CD", "W", "MM_WW-88"]
texts += ["".join(rng.choice(list(NAME_CHARS), int(rng.integers(3, 15)))) for _ in range(14)]
texts += [f"{int(rng.integers(0, 999))}.{int(rng.integers(0, 10))}{rng.choice(['km', 'm'])}" for _ in range(6)]
rows = []
for k, t in enumerate(texts):
    rows.append((t, 0.40 + 0.05 * (k % 4), COLOURS[k % 4], FONTS[k % 2], None))
for k, t in enumerate(["EXMPLTWO", "TOUCHING", "QOD0_8B", "45.6km"]):
    rows.append((t, 0.5, COLOURS[k % 4], FONTS[k % 2], -3))           # letters run together
rows.append(("WWWWWWWWWWWWWW", 0.5, COLOURS[0], FONTS[1], -5))       # one blob past the cap
def row_case(name, img, box):
    expect = {}
    m = ir.create_mask(img, box)
    if m is None:
        expect["mask"] = None
    else:
        expect["mask"] = {"w": m.width, "h": m.height, "ox": m.origin_x, "oy": m.origin_y,
                          "col": m.col_offset, "row": m.row_offset, "crc": zlib.crc32(m.data.tobytes())}
        expect["namePieces"] = pieces_json(rr.reader.read_pieces(m, rr.reader.name_alphabet, ir.NAME_MIN_INK))
        expect["distPieces"] = pieces_json(
            rr.reader.read_pieces(m, rr.reader.distance_alphabet, ir.DISTANCE_MIN_INK))
    nr = rr.read_name(img, box)
    expect["name"] = {"text": nr.text, "score": nr.score, "oneWord": nr.one_word,
                      "validChars": nr.valid_chars, "o0": [[i, p] for i, p in nr.o0]}
    dr = rr.read_distance(img, box)
    expect["dist"] = None if dr is None else {"text": dr.text, "value": dr.value, "km": dr.kilometers,
                                              "speed": dr.speed_group}
    case("iris_row", name, [img], [], box=list(box), expect=expect)


for k, (t, scale, colour, font, spacing) in enumerate(rows):
    row_case(f"row_{k:02d}", *render(t, scale, colour, font, spacing))
row_case("row_blank", noise(30, 120, 3, lo=8, hi=45), (10, 5, 100, 20))
row_case("row_outside", noise(30, 120, 3), (200, 50, 10, 10))

(HERE / "opencv.bin").write_bytes(bytes(BLOB))
(HERE / "index.json").write_text(json.dumps({"opencv": cv2.__version__, "numpy": np.__version__,
                                             "cases": cases}, indent=0), encoding="utf-8", newline="\n")
print(f"{len(cases)} cases, opencv.bin {len(BLOB) / 1e6:.1f} MB")
