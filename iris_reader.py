"""IRIS's nameplate READING layer, ported to Python.

Ported from IRIS - https://github.com/Niteb1rd/IRIS - at commit aec86f08
(IRIS 2.0.0), with the two reading fixes of IRIS pull request #1 (commit
29c3250e95: distance end-trim by score per letter, split cap 500), licensed
GPL-3.0 like this project. The letter model
(models/iris-letters-model.bin) is IRIS's own file, unchanged. Modified from
the original: translated from C# to Python/numpy, and cut down to what
reading needs - LetterModel, NameplateFeatures (mask, cap band, gap cuts,
glyph vector), NameplateReader (pieces and the best-path split),
NameplateDetector.ReadNameRow / ReadDistanceLine, O0Rule,
UnderscoreHyphenRule, DistanceLineRule and LookAlikes. IRIS's multi-layer
text check (LayerCheck) is left out: its default rule ('Brightest') never
runs it.

What this is for: given an image and the box of ONE text row, read it as a
player name or as a distance line, the way IRIS does - a per-letter network
trained on the HUD font, far cheaper than general OCR. Finding the rows is a
separate layer, not ported here.

Parity with IRIS is checked by tools/iris_parity.py against reference
readings written by IRIS's own code (the bench harness's --golden mode).
Names and structure follow the C# so each function can be compared with its
original line by line.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from pathlib import Path

import cv2
import numpy as np

import paths

# paths.resource, not __file__: in the frozen build the model sits next to the
# executable (sc-watch.spec bundles it as models/), wherever the code is.
MODEL_PATH = paths.resource("models", "iris-letters-model.bin")

# ---- settings (NameplateDetectorSettings defaults used by reading) ---------

SCALE = 4                       # NameplateFeatures.Scale
GLYPH_W, GLYPH_H = 16, 24
VECTOR_LENGTH = GLYPH_W * GLYPH_H + 4
MIN_NAME_SCORE = -0.20
MIN_DISTANCE_SCORE = -0.35
MAX_DISTANCE_GAP = 4.0
DISTANCE_END_SCORE = -1.0
MAX_DISTANCE_VALUE = 1000
DISTANCE_MIN_INK = 0.003
NAME_MIN_INK = 0.02
WORD_GAP = 0.33
BRACKET_MIN_SCORE = -0.7
MAX_PIECE_CAPS = 8.0
MAX_SPLIT_SPANS = 500           # IRIS PR #1 (was 1500); plate names peak at 271 here
SPLIT_FAILURE_SCORE = -50.0
MIN_PROBABILITY = 1e-6
O0_DECIDE = 0.95
BAR_UNDERSCORE_MIN = 0.87
BAR_HYPHEN_MAX = 0.71
BAR_MAX_THICK = 0.45
BAR_ROW_FRACTION = 0.25

NAME_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-"
EFFICIENCY_GROUPS = ("0OQD", "1I7", "5S", "8B", "6G", "2Z")
EFFICIENCY_LETTERS = "OQDISBGZ"
DISTANCE_ALPHABET = "0123456789.km" + EFFICIENCY_LETTERS

_TOPHAT = cv2.getStructuringElement(cv2.MORPH_RECT, (31, 31))
_OPEN = cv2.getStructuringElement(cv2.MORPH_RECT, (1, 3))     # 1 wide, 3 tall


# ---- the letter model -------------------------------------------------------

class LetterModel:
    """Dense layers, ReLU between them, log-softmax at the end."""

    MAGIC = b"IRISLM01"

    def __init__(self, charset: str, layers: list[tuple[np.ndarray, np.ndarray]]):
        self.charset = charset
        self.layers = layers            # (weights rows x cols float64, biases)
        self.input_size = layers[0][0].shape[0]

    @classmethod
    def load(cls, path: Path = MODEL_PATH) -> "LetterModel":
        b = Path(path).read_bytes()
        if b[:8] != cls.MAGIC:
            raise ValueError("not an IRIS letter model (bad magic)")
        version, n = struct.unpack_from("<II", b, 8)
        if version != 1:
            raise ValueError(f"unsupported letter model version {version}")
        charset = b[16:16 + n].decode("utf-8")
        at = 16 + n
        input_size, count = struct.unpack_from("<II", b, at)
        at += 8
        layers, expected = [], input_size
        for _ in range(count):
            rows, cols = struct.unpack_from("<II", b, at)
            at += 8
            if rows != expected:
                raise ValueError("layer shape does not follow the previous one")
            w = np.frombuffer(b, "<f4", rows * cols, at).reshape(rows, cols)
            at += 4 * rows * cols
            bias = np.frombuffer(b, "<f4", cols, at)
            at += 4 * cols
            # float32 weights, double arithmetic - as the C# does.
            layers.append((w.astype(np.float64), bias.astype(np.float64)))
            expected = cols
        if expected != len(charset) or at != len(b):
            raise ValueError("model file does not match its header")
        return cls(charset, layers)

    def log_probs(self, x: np.ndarray) -> np.ndarray:
        cur = x
        last = len(self.layers) - 1
        for k, (w, bias) in enumerate(self.layers):
            nxt = bias + cur @ w
            if k < last:
                cur = np.maximum(nxt, 0.0)
            else:
                e = np.exp(nxt - nxt.max())
                return np.log(np.clip(e / e.sum(), MIN_PROBABILITY, 1.0))
        raise AssertionError("unreachable")

    def alphabet(self, chars: str) -> np.ndarray:
        allowed = np.array([c in chars for c in self.charset])
        if not allowed.any():
            raise ValueError("none of the characters is in the model's charset")
        return allowed


def _argmax(values: np.ndarray, allowed: np.ndarray | None) -> int:
    """First index of the maximum, among the allowed ones."""
    if allowed is None:
        return int(np.argmax(values))
    idx = np.flatnonzero(allowed)
    return int(idx[np.argmax(values[idx])]) if idx.size else 0


# ---- masks and glyph features (NameplateFeatures) ---------------------------

@dataclass
class LineMask:
    data: np.ndarray                    # uint8 0/1, height x width, at 4x
    origin_x: int = 0                   # crop origin in the source image
    origin_y: int = 0
    col_offset: int = 0                 # trim offsets inside the 4x crop
    row_offset: int = 0
    ink_colors: np.ndarray | None = None    # N x 3 BGR at 4x under the ink

    @property
    def height(self) -> int:
        return self.data.shape[0]

    @property
    def width(self) -> int:
        return self.data.shape[1]


@dataclass(frozen=True)
class CapBand:
    top: int
    bottom: int

    @property
    def height(self) -> int:
        return self.bottom - self.top + 1


def create_mask(bgr: np.ndarray, box, threshold: float | None = None,
                pad: int = 2, keep_colors: bool = False) -> LineMask | None:
    """Box (x, y, w, h) -> the text's ink at 4x: cubic upscale, per-channel
    top-hat, brightest channel, Otsu clamped to 25-90, a 1x3 opening, then
    trimmed to the text. keep_colors also keeps the upscaled colour under
    each ink pixel (IRIS's plate tint and colour)."""
    x, y, w, h = box
    H, W = bgr.shape[:2]
    x0, y0 = max(0, x - pad), max(0, y - pad)
    x1, y1 = min(W, x + w + pad), min(H, y + h + pad)
    if x1 <= x0 or y1 <= y0:
        return None
    crop = np.ascontiguousarray(bgr[y0:y1, x0:x1, :3])
    big = cv2.resize(crop, None, fx=SCALE, fy=SCALE, interpolation=cv2.INTER_CUBIC)
    hats = [cv2.morphologyEx(np.ascontiguousarray(big[:, :, k]), cv2.MORPH_TOPHAT, _TOPHAT)
            for k in range(3)]
    top_hat = np.maximum(np.maximum(hats[0], hats[1]), hats[2])
    if threshold is None:
        otsu, _ = cv2.threshold(top_hat, 0, 255, cv2.THRESH_BINARY | cv2.THRESH_OTSU)
        thr = min(max(otsu, 25.0), 90.0)
    else:
        thr = threshold
    _, binary = cv2.threshold(top_hat, thr, 1, cv2.THRESH_BINARY)
    opened = cv2.morphologyEx(binary, cv2.MORPH_OPEN, _OPEN)
    return trim_to_text(opened, x0, y0, big if keep_colors else None)


def trim_to_text(data: np.ndarray, origin_x: int = 0, origin_y: int = 0,
                 colors4x: np.ndarray | None = None) -> LineMask | None:
    h, w = data.shape
    rows = np.flatnonzero(data.sum(axis=1, dtype=np.int64) > 0.02 * w)
    if rows.size == 0:
        return None
    r0, r1 = int(rows[0]), int(rows[-1])
    cols = np.flatnonzero(data[r0:r1 + 1].any(axis=0))
    if cols.size == 0:
        return None
    c0, c1 = int(cols[0]), int(cols[-1])
    out = np.ascontiguousarray(data[r0:r1 + 1, c0:c1 + 1])
    ink = None
    if colors4x is not None:
        ink = colors4x[r0:r1 + 1, c0:c1 + 1][out != 0].reshape(-1, 3)
    return LineMask(out, origin_x, origin_y, c0, r0, ink)


def gap_cuts(m: LineMask) -> list[tuple[int, int]]:
    """Runs of columns with any ink."""
    ink = m.data.any(axis=0)
    cuts, start = [], -1
    for x, has in enumerate(ink):
        if has and start < 0:
            start = x
        elif not has and start >= 0:
            cuts.append((start, x))
            start = -1
    if start >= 0:
        cuts.append((start, m.width))
    return cuts


def _median_int(values: list[int]) -> int:
    v = sorted(values)
    n = len(v)
    return int(v[n // 2] if n & 1 else (v[n // 2 - 1] + v[n // 2]) / 2.0)


def find_cap_band(m: LineMask) -> CapBand:
    tops, bottoms = [], []
    for a, b in gap_cuts(m):
        rows = np.flatnonzero(m.data[:, a:b].any(axis=1))
        if rows.size and rows[-1] - rows[0] > 0.5 * m.height:
            tops.append(int(rows[0]))
            bottoms.append(int(rows[-1]))
    if not tops:
        return CapBand(0, m.height - 1)
    return CapBand(_median_int(tops), _median_int(bottoms))


def glyph_vector(m: LineMask, x0: int, x1: int, band: CapBand) -> np.ndarray | None:
    """The letter between columns x0 and x1, area-resized to 16x24, plus its
    width, height and top/bottom offsets in cap heights (x4)."""
    x1 = min(x1, m.width)
    cap = max(1, band.bottom - band.top + 1)
    sub = m.data[:, x0:x1]
    rows = np.flatnonzero(sub.any(axis=1))
    if rows.size == 0:
        return None
    cols = np.flatnonzero(sub.any(axis=0))
    row_min, row_max = int(rows[0]), int(rows[-1])
    col_min, col_max = x0 + int(cols[0]), x0 + int(cols[-1])
    gh, gw = row_max - row_min + 1, col_max - col_min + 1
    src = m.data[row_min:row_max + 1, col_min:col_max + 1].astype(np.float32)
    small = cv2.resize(src, (GLYPH_W, GLYPH_H), interpolation=cv2.INTER_AREA)
    v = np.empty(VECTOR_LENGTH, np.float64)
    v[:GLYPH_W * GLYPH_H] = small.ravel()
    n = GLYPH_W * GLYPH_H
    v[n] = gw / cap * 4.0
    v[n + 1] = gh / cap * 4.0
    v[n + 2] = (row_min - band.top) / cap * 4.0
    v[n + 3] = (row_max - band.bottom) / cap * 4.0
    return v


# ---- reading pieces (NameplateReader) ---------------------------------------

@dataclass(frozen=True)
class Piece:
    start: int
    end: int
    text: str
    score: float
    free_score: float


@dataclass
class PieceRead:
    pieces: list[Piece]
    cap: int
    spans: list[tuple[int, int]]
    mask: LineMask


class NameplateReader:
    def __init__(self, model: LetterModel | None = None):
        self.model = model or LetterModel.load()
        if self.model.input_size != VECTOR_LENGTH:
            raise ValueError("the model's input does not match the glyph features")
        self.name_alphabet = self.model.alphabet(NAME_ALPHABET)
        self.distance_alphabet = self.model.alphabet(DISTANCE_ALPHABET)

    def read_pieces(self, m: LineMask, allowed: np.ndarray | None = None,
                    min_ink: float = NAME_MIN_INK) -> PieceRead:
        band = find_cap_band(m)
        cap = band.bottom - band.top + 1
        pieces: list[Piece] = []
        spans: list[tuple[int, int]] = []
        charset = self.model.charset
        for a, b in gap_cuts(m):
            ink = int(m.data[:, a:b].sum(dtype=np.int64))
            if ink < min_ink * cap * cap:
                continue
            if b - a <= 1.2 * cap:
                v = glyph_vector(m, a, b, band)
                if v is None:
                    continue
                lp = self.model.log_probs(v)
                best = _argmax(lp, allowed)
                free = best if allowed is None else _argmax(lp, None)
                spans.append((a, b))
                pieces.append(Piece(a, b, charset[best], float(lp[best]), float(lp[free])))
            else:
                text, score = self._split(m, a, b, band, allowed, True, spans)
                pieces.append(Piece(a, b, text, score, score))
        return PieceRead(pieces, cap, spans, m)

    def _split(self, m: LineMask, x0: int, x1: int, band: CapBand,
               allowed, limit_work: bool, spans_out: list) -> tuple[str, float]:
        """Letters joined into one blob: try every cut at a column-ink minimum
        and keep the best-scoring path through them."""
        cap = band.bottom - band.top + 1
        width = x1 - x0
        if limit_work and width > MAX_PIECE_CAPS * cap:
            return "", SPLIT_FAILURE_SCORE
        col = m.data[:, x0:x1].sum(axis=0, dtype=np.int64)
        cand = [0] + [x for x in range(2, width - 2)
                      if col[x] <= col[x - 1] and col[x] <= col[x + 1] and col[x] <= 0.5 * cap]
        cand.append(width)
        hi, lo = 1.2 * cap, 0.25 * cap
        if limit_work:
            pairs = 0
            for i in range(len(cand)):
                for j in range(i + 1, len(cand)):
                    span = cand[j] - cand[i]
                    if span > hi:
                        break
                    if span >= lo:
                        pairs += 1
            if pairs > MAX_SPLIT_SPANS:
                return "", SPLIT_FAILURE_SCORE
        spans = []
        for i in range(len(cand)):
            for j in range(i + 1, len(cand)):
                span = cand[j] - cand[i]
                if span > hi:
                    break
                if span < lo:
                    continue
                v = glyph_vector(m, x0 + cand[i], x0 + cand[j], band)
                if v is None:
                    continue
                lp = self.model.log_probs(v)
                best = _argmax(lp, allowed)
                spans.append((i, j, best, float(lp[best])))
        if not spans:
            return "", SPLIT_FAILURE_SCORE
        n = len(cand)
        score = [-1e9] * n
        back_from = [-1] * n
        back_char = [0] * n
        score[0] = 0.0
        for i, j, ch, lp in spans:
            s = score[i] + lp
            if score[i] > -1e8 and s > score[j]:
                score[j], back_from[j], back_char[j] = s, i, ch
        end = n - 1
        if score[end] <= -1e8:
            return "", SPLIT_FAILURE_SCORE
        letters, letter_spans = [], []
        at = end
        while at != 0 and back_from[at] >= 0:
            letters.append(self.model.charset[back_char[at]])
            letter_spans.append((x0 + cand[back_from[at]], x0 + cand[at]))
            at = back_from[at]
        letters.reverse()
        letter_spans.reverse()
        spans_out.extend(letter_spans)
        return "".join(letters), score[end]

    def is_open_bracket(self, m: LineMask, start: int, end: int, min_score: float) -> bool:
        bracket = self.model.charset.find("[")
        if bracket < 0:
            return False
        v = glyph_vector(m, start, end, find_cap_band(m))
        if v is None:
            return False
        lp = self.model.log_probs(v)
        best = _argmax(lp, None)
        return best == bracket and lp[best] >= min_score


# ---- O / 0 by the hole's shape (O0Rule) -------------------------------------

O0_WEIGHT_A = 0.5667556378738614
O0_WEIGHT_B = 0.8238859429181884
O0_MEAN_O, O0_SD_O = 0.7533676849758579, 0.025796386202400184
O0_MEAN_0, O0_SD_0 = 0.6808192767983506, 0.01720362317305907
O0_UPSCALE, O0_PAD = 8, 3


def _percentile_sorted(s: np.ndarray, q: float) -> float:
    pos = (len(s) - 1) * q / 100.0
    lower = int(np.floor(pos))
    upper = min(lower + 1, len(s) - 1)
    return float(s[lower]) + (float(s[upper]) - float(s[lower])) * (pos - lower)


def o0_measure(crop: np.ndarray):
    gray = crop.max(axis=2) if crop.ndim == 3 else crop
    big = cv2.resize(gray.astype(np.float32), None, fx=O0_UPSCALE, fy=O0_UPSCALE,
                     interpolation=cv2.INTER_CUBIC)
    s = np.sort(big, axis=None)
    thr = np.float32((_percentile_sorted(s, 10) + _percentile_sorted(s, 99)) / 2.0)
    return o0_measure_mask((big > thr).astype(np.uint8))


def o0_measure_mask(mask: np.ndarray):
    h, w = mask.shape
    columns = mask.sum(axis=0, dtype=np.int64)
    best_start = best_end = -1
    best_ink = -1
    at = 0
    while at < w:
        if columns[at] > 0:
            start = at
            ink = 0
            while at < w and columns[at] > 0:
                ink += int(columns[at])
                at += 1
            if ink > best_ink:
                best_ink, best_start, best_end = ink, start, at
        at += 1
    if best_start < 0:
        return None
    gw = best_end - best_start
    rows = np.flatnonzero(mask[:, best_start:best_end].any(axis=1))
    first, last = int(rows[0]), int(rows[-1])
    gh = last - first + 1
    if gw > 1.6 * gh:
        return None
    region = mask[first:last + 1, best_start:best_end]
    holes = (region == 0).astype(np.uint8)
    n, labels, stats, _ = cv2.connectedComponentsWithStats(holes, connectivity=4)
    best_area, hole_w, hole_h, best_key = 0, 0, 0, None
    for k in range(1, n):
        x, y, bw, bh, area = (int(v) for v in stats[k])
        interior = x > 0 and y > 0 and x + bw < gw and y + bh < gh
        if not interior:
            continue
        # the C# keeps the first hole (raster order) among equal largest
        first_px = int(np.flatnonzero(labels.ravel() == k)[0])
        if area > best_area or (area == best_area and best_key is not None and first_px < best_key):
            best_area, hole_w, hole_h, best_key = area, bw, bh, first_px
    if best_area == 0:
        return None
    u = float(O0_UPSCALE)
    return gw / u, gh / u, hole_w / u, hole_h / u


def o0_probability(m) -> float:
    ow, oh, hw, _ = m
    s = (ow / oh) * O0_WEIGHT_A + (hw / oh) * O0_WEIGHT_B
    s = min(max(s, min(O0_MEAN_0, O0_MEAN_O)), max(O0_MEAN_0, O0_MEAN_O))
    l_o = -0.5 * ((s - O0_MEAN_O) / O0_SD_O) ** 2 - np.log(O0_SD_O)
    l_0 = -0.5 * ((s - O0_MEAN_0) / O0_SD_0) ** 2 - np.log(O0_SD_0)
    d = l_0 - l_o
    if d > 700:
        return 0.0
    return float(1.0 / (1.0 + np.exp(d)))


def o0_glyph_box(mask: LineMask, start: int, end: int, pad: int = O0_PAD):
    sub = mask.data[:, start:min(end, mask.width)]
    rows = np.flatnonzero(sub.any(axis=1)) if sub.size else np.array([], int)
    if rows.size == 0:
        return None
    first, last = int(rows[0]), int(rows[-1])
    x0 = mask.origin_x + int(np.floor((mask.col_offset + start) / SCALE)) - pad
    x1 = mask.origin_x + int(np.ceil((mask.col_offset + end) / SCALE)) + pad
    y0 = mask.origin_y + int(np.floor((mask.row_offset + first) / SCALE)) - pad
    y1 = mask.origin_y + int(np.ceil((mask.row_offset + last + 1) / SCALE)) + pad
    return x0, y0, x1, y1


def o0_correct(bgr: np.ndarray, read: PieceRead, text: str, evidence: list | None = None) -> str:
    if len(read.spans) != len(text) or ("O" not in text and "0" not in text):
        return text
    chars = list(text)
    H, W = bgr.shape[:2]
    for k, ch in enumerate(chars):
        if ch not in "O0":
            continue
        box = o0_glyph_box(read.mask, *read.spans[k])
        if box is None:
            continue
        x0, y0 = max(0, box[0]), max(0, box[1])
        x1, y1 = min(W, box[2]), min(H, box[3])
        if x1 <= x0 or y1 <= y0:
            continue
        m = o0_measure(bgr[y0:y1, x0:x1, :3])
        if m is None:
            continue
        p = o0_probability(m)
        if evidence is not None:
            evidence.append((k, p))
        if p >= O0_DECIDE:
            chars[k] = "O"
        elif p <= 1.0 - O0_DECIDE:
            chars[k] = "0"
    return "".join(chars)


# ---- '_' against '-' by the bar's height (UnderscoreHyphenRule) -------------

def bar_measure(m: LineMask, start: int, end: int, band: CapBand):
    start, end = max(0, start), min(end, m.width)
    if end <= start:
        return None
    sums = m.data[:, start:end].sum(axis=1, dtype=np.int64)
    top = int(sums.max())
    if top <= 0:
        return None
    rows = np.flatnonzero(sums >= BAR_ROW_FRACTION * top)
    r0, r1 = int(rows[0]), int(rows[-1])
    cols = np.flatnonzero(m.data[r0:r1 + 1, start:end].any(axis=0))
    cap = float(max(1, band.bottom - band.top + 1))
    frac = ((r0 + r1 + 1) / 2.0 - band.top) / cap
    thick = (r1 - r0 + 1) / cap
    length = 0.0 if cols.size == 0 else (int(cols[-1]) - int(cols[0]) + 1) / cap
    return frac, thick, length


def underscore_hyphen_correct(read: PieceRead, text: str) -> str:
    if len(read.spans) != len(text) or ("_" not in text and "-" not in text):
        return text
    band = find_cap_band(read.mask)
    chars = list(text)
    for k, ch in enumerate(chars):
        if ch not in "_-":
            continue
        bar = bar_measure(read.mask, *read.spans[k], band)
        if bar is None or bar[1] > BAR_MAX_THICK:
            continue
        if bar[0] >= BAR_UNDERSCORE_MIN:
            chars[k] = "_"
        elif bar[0] <= BAR_HYPHEN_MAX:
            chars[k] = "-"
    return "".join(chars)


# ---- distance lines (DistanceLineRule) --------------------------------------

@dataclass(frozen=True)
class DistanceReading:
    text: str
    value: float
    kilometers: bool
    speed_group: bool = False

    @property
    def km(self) -> float:
        return self.value if self.kilometers else self.value / 1000.0


def efficiency_digit(c: str) -> str | None:
    if "0" <= c <= "9":
        return c
    for group in EFFICIENCY_GROUPS:
        if group.find(c) > 0:
            return group[0]
    return None


def parse_distance(text: str, max_value: int = MAX_DISTANCE_VALUE) -> DistanceReading | None:
    at = 0
    while at < len(text) and at < 4 and "0" <= text[at] <= "9":
        at += 1
    if at == 0 or at >= len(text) or text[at] != ".":
        return None
    whole = int(text[:at])
    if whole > max_value:
        return None
    if at + 1 >= len(text) or not ("0" <= text[at + 1] <= "9"):
        return None
    tenth = ord(text[at + 1]) - ord("0")
    unit = text[at + 2:]
    if unit not in ("km", "m"):
        return None
    return DistanceReading(text, whole + tenth / 10.0, unit == "km")


def _score_per_letter(piece: Piece) -> float:
    return piece.score / max(1, len(piece.text))


def evaluate_distance(read: PieceRead | None, is_open_bracket=None) -> DistanceReading | None:
    if read is None or not read.pieces:
        return None
    pieces = read.pieces
    cap = read.cap
    # Junk at either end is trimmed by score PER LETTER (IRIS PR #1): a piece
    # of touching letters read as one ('km') sums their scores and was cut
    # off as junk.
    first, last = 0, len(pieces) - 1
    while last > first and _score_per_letter(pieces[last]) < DISTANCE_END_SCORE:
        last -= 1
    while first < last and _score_per_letter(pieces[first]) < DISTANCE_END_SCORE:
        first += 1
    pieces = pieces[first:last + 1]
    speed = False
    for i in range(len(pieces) - 1):
        if pieces[i + 1].start - pieces[i].end <= WORD_GAP * cap:
            continue
        if is_open_bracket is None or not is_open_bracket(pieces[i + 1]):
            return None
        pieces = pieces[:i + 1]
        speed = True
        break
    text, chars, total = [], 0, 0.0
    for p in pieces:
        for ch in p.text:
            text.append(efficiency_digit(ch) or ch)
        chars += len(p.text)
        total += p.score
    mean = total / max(1, chars)
    count = min(6, len(pieces))
    gap = sum((p.free_score - p.score) / max(1, len(p.text)) for p in pieces[:count]) / count
    if mean < MIN_DISTANCE_SCORE or gap > MAX_DISTANCE_GAP:
        return None
    parsed = parse_distance("".join(text))
    if parsed is None:
        return None
    return DistanceReading(parsed.text, parsed.value, parsed.kilometers, speed)


# ---- the two row readers (NameplateDetector.ReadNameRow / ReadDistanceLine) -

@dataclass
class NameRowRead:
    text: str = ""
    score: float = 0.0
    one_word: bool = False
    valid_chars: bool = False
    mask: LineMask | None = None
    o0: list = field(default_factory=list)

    @property
    def usable(self) -> bool:
        return self.mask is not None and bool(self.text) and self.one_word and self.valid_chars


class RowReader:
    """Read one text row of a nameplate, IRIS's way."""

    def __init__(self, reader: NameplateReader | None = None,
                 o0_rule: bool = True, underscore_rule: bool = True):
        self.reader = reader or NameplateReader()
        self.o0_rule = o0_rule
        self.underscore_rule = underscore_rule

    def read_name(self, bgr: np.ndarray, box) -> NameRowRead:
        return self.read_name_mask(bgr, create_mask(bgr, box, keep_colors=True))

    def read_name_mask(self, bgr: np.ndarray, mask: LineMask | None) -> NameRowRead:
        if mask is None:
            return NameRowRead()
        read = self.reader.read_pieces(mask, self.reader.name_alphabet)
        if not read.pieces:
            return NameRowRead()
        text = "".join(p.text for p in read.pieces)
        if not text:
            return NameRowRead()
        one_word = all(read.pieces[i + 1].start - read.pieces[i].end <= WORD_GAP * read.cap
                       for i in range(len(read.pieces) - 1))
        valid = all(("A" <= c <= "Z") or ("0" <= c <= "9") or c in "_-" for c in text)
        score = sum(p.score for p in read.pieces)
        if not one_word or not valid:
            return NameRowRead(text, score, one_word, valid, mask)
        evidence: list = []
        if self.o0_rule:
            text = o0_correct(bgr, read, text, evidence)
        if self.underscore_rule:
            text = underscore_hyphen_correct(read, text)
        return NameRowRead(text, score, True, True, mask, evidence)

    def read_distance(self, bgr: np.ndarray, box) -> DistanceReading | None:
        return self.read_distance_mask(create_mask(bgr, box))

    def read_distance_mask(self, mask: LineMask | None) -> DistanceReading | None:
        if mask is None:
            return None
        read = self.reader.read_pieces(mask, self.reader.distance_alphabet, DISTANCE_MIN_INK)
        return evaluate_distance(
            read, lambda p: self.reader.is_open_bracket(mask, p.start, p.end, BRACKET_MIN_SCORE))
