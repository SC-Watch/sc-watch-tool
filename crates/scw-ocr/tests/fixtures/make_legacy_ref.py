# The legacy OCR reader's own answers (sc_detector.py, as watch.py --reader
# ocr runs it) on every frame of the five corpora: the boxes the masks find,
# the pairs, the monitor-rate pass (watch.cheap_pass), the full read
# (sd.detect) with each option the watcher can switch on (gate, haze, dim
# range retry), the focused read (sd.detect_focused) direct and through the
# masks, and the dual read (sd.merge_contacts) - with every OCR call each one
# made, so a difference can be traced to the first crop that differs. The
# reference for scw-ocr's legacy reader (tests/legacy_parity.rs).
#
# Frames and reads name real players: written to parity-data/legacy/ (local
# only, never committed); only counts are printed. Run with the Python tree's
# build venv (rapidocr-onnxruntime 1.2.3, OpenCV 5.0):
#
#   python make_legacy_ref.py <python tree> <scratch dir> <parity-data>/legacy [--ipp]
import sys, pathlib, json, hashlib, time
DEV = pathlib.Path(sys.argv[1]); SCR = pathlib.Path(sys.argv[2]); OUT = pathlib.Path(sys.argv[3])
sys.path.insert(0, str(DEV))
import paths
paths.DATA_ROOT = SCR / "data"          # before anything else of sc-watch's
import cv2, numpy as np
import sc_detector as sd
import km_gate
from rapidocr_onnxruntime import RapidOCR

OUT.mkdir(parents=True, exist_ok=True)
eng = RapidOCR(**sd._ocr_kwargs())
eng.use_angle_cls = False               # as sc_detector.make_ocr
template = km_gate.load_template()
assert template is not None

# prep_for_ocr's cubic enlargement as OpenCV's own code does it. The
# opencv-python wheels hand cv2.resize(INTER_CUBIC) on 8 bits to Intel's
# IPP, whose last bit depends on the processor (here its AVX-512 code moved
# 101 of 15,075 pixels of one crop by one level); the port is OpenCV's own
# arithmetic. Only this step: the OCR engine runs as it does in sc-watch.
# --ipp keeps IPP on throughout, to measure what it changes.
IPP = "--ipp" in sys.argv
_prep = sd.prep_for_ocr


def prep_for_ocr(crop):
    cv2.ipp.setUseIPP(IPP)
    try:
        return _prep(crop)
    finally:
        cv2.ipp.setUseIPP(True)


sd.prep_for_ocr = prep_for_ocr

memo = {}


def key(img):
    return hashlib.sha1(img.tobytes()).hexdigest() + f":{img.shape[1]}x{img.shape[0]}x{img.shape[2] if img.ndim == 3 else 1}"


def result_of(img):
    k = key(img)
    if k not in memo:
        res, _ = eng(img)
        memo[k] = [[[[float(x), float(y)] for x, y in np.asarray(b).tolist()], t, float(s)]
                   for b, t, s in (res or [])]
    return k, memo[k]


class Ocr:
    """sc_detector.make_ocr's callable, memoised, logging each call."""
    def __init__(self):
        self.calls = []

    def __call__(self, img):
        k, res = result_of(img)
        text = "".join(r[1] for r in res)
        self.calls.append(["text", k, text])
        return text

    def lines(self, img):
        k, res = result_of(img)
        out = []
        for box, text, conf in res:
            xs = [p[0] for p in box]; ys = [p[1] for p in box]
            out.append(sd.OcrLine(x=int(min(xs)), y=int(min(ys)), w=int(max(xs) - min(xs)),
                                  h=int(max(ys) - min(ys)), text=text, conf=float(conf)))
        self.calls.append(["lines", k, [[l.x, l.y, l.w, l.h, l.text, l.conf] for l in out]])
        return out


def box(b):
    return None if b is None else [int(b.x), int(b.y), int(b.w), int(b.h)]


def contacts(cs):
    return [[box(c.name_box), box(c.range_box), c.range_km, c.name, c.kind] for c in cs]


def run(fn):
    o = Ocr()
    found = fn(o)
    return found, {"contacts": contacts(found), "ocr": o.calls}


def with_options(gate=False, haze=False, orphan=False):
    sd.KM_GATE_ENABLED, sd.HAZE_LOCATOR_ENABLED = gate, haze
    sd.ORPHAN_RETRY_MAX = sd.ORPHAN_RETRY_WHEN_ON if orphan else 0


def cheap_pass(frame, gate):
    """watch.cheap_pass, which imports the whole watcher."""
    pairs = sd.pair_boxes(sd.find_boxes(sd.hud_mask(frame)), frame.shape[0])
    if not gate:
        return pairs
    return [p for p in pairs
            if km_gate.score(p.range_box.crop(frame, sd.CROP_PAD), template) >= sd.KM_GATE_THRESHOLD]


sets = ["corpus", "corpus_nohdr", "corpus_winhdr", "corpus_cockpit", "corpus_16x9"]
paths_ = [p for s in sets for p in sorted((DEV / s).glob("*.png"))]
(OUT / "frames.txt").write_text("".join(f"{p}\n" for p in paths_), encoding="utf-8")
frames = []
t0 = time.time()
for n, p in enumerate(paths_):
    f = cv2.imread(str(p), cv2.IMREAD_COLOR)
    H = f.shape[0]
    rec = {"w": int(f.shape[1]), "h": int(H)}
    with_options()
    boxes = sd.find_all_boxes(f, H)
    rec["boxes"] = [[b.x, b.y, b.w, b.h, b.core] for b in boxes]
    pairs = sd.pair_boxes(boxes, H)
    idx = {id(b): i for i, b in enumerate(boxes)}
    rec["pairs"] = [[idx[id(c.name_box)], idx[id(c.range_box)]] for c in pairs]
    if len(pairs) > sd.MAX_OCR_PAIRS:
        rec["shape_scores"] = [sd.range_shape_score(c.range_box.crop(f, sd.CROP_PAD)) for c in pairs]
    rec["gate_scores"] = [km_gate.score(c.range_box.crop(f, sd.CROP_PAD), template) for c in pairs]
    with_options(haze=True)
    rec["boxes_haze"] = [[b.x, b.y, b.w, b.h, b.core] for b in sd.find_all_boxes(f, H)]
    with_options()
    rec["cheap"] = [[box(c.name_box), box(c.range_box)] for c in cheap_pass(f, False)]
    rec["cheap_gate"] = [[box(c.name_box), box(c.range_box)] for c in cheap_pass(f, True)]

    whole, rec["detect"] = run(lambda o: sd.detect(f, o))
    with_options(gate=True)
    _, rec["detect_gate"] = run(lambda o: sd.detect(f, o))
    with_options(haze=True)
    _, rec["detect_haze"] = run(lambda o: sd.detect(f, o))
    with_options(orphan=True)
    _, rec["detect_orphan"] = run(lambda o: sd.detect(f, o))
    with_options()
    centre, rec["focused"] = run(lambda o: sd.detect_focused(f, o))
    _, rec["focused_mask"] = run(lambda o: sd.detect_focused(f, o, direct=False))
    # watch.Session._detect's dual: whole() and centre() over the same frame
    rec["dual"] = contacts(sd.merge_contacts(whole, centre, H))
    frames.append(rec)
    if (n + 1) % 10 == 0:
        print(f"{n + 1}/{len(paths_)} frames, {len(memo)} distinct OCR inputs, {time.time() - t0:.0f} s", flush=True)

json.dump({"rapidocr": "1.2.3", "ipp_cubic": IPP, "frames": frames},
          open(OUT / ("legacy_ipp.json" if IPP else "legacy.json"), "w", encoding="utf-8"), ensure_ascii=True)
mode = ["detect", "detect_gate", "detect_haze", "detect_orphan", "focused", "focused_mask"]
print(f"done: {len(frames)} frames, {len(memo)} distinct OCR inputs; contacts "
      + ", ".join(f"{m} {sum(len(r[m]['contacts']) for r in frames)}" for m in mode)
      + f", dual {sum(len(r['dual']) for r in frames)}; {time.time() - t0:.0f} s")
