# RapidOCR's own answers on every crop sc-watch's legacy OCR pipeline hands
# it, over the five corpora: the plate reads (sd.detect), focused reads
# (sd.detect_focused, every 10th frame) and chat reads (chat.read_located).
# The reference for scw-ocr (tests/rapidocr_parity.rs). Crops and answers
# hold player names: written to parity-data/ocr/ (local only, never
# committed); only counts are printed. Run with the Python tree's build venv
# (rapidocr-onnxruntime 1.2.3, OpenCV 5.0):
#
#   python make_ocr_ref.py <python tree> <scratch dir> <parity-data>/ocr
import sys, pathlib, json, hashlib
DEV = pathlib.Path(sys.argv[1]); SCR = pathlib.Path(sys.argv[2]); OUT = pathlib.Path(sys.argv[3])
sys.path.insert(0, str(DEV))
import paths
paths.DATA_ROOT = SCR / "data"
import cv2, numpy as np
import sc_detector as sd
import chat
from rapidocr_onnxruntime import RapidOCR

eng = RapidOCR(**sd._ocr_kwargs())
eng.use_angle_cls = False            # as sc_detector.make_ocr
crops = OUT / "crops"; crops.mkdir(parents=True, exist_ok=True)
entries, seen = [], {}
caps = {"focus": 40, "chat": 60}
counts = {}
checked = [0]

def answer(img):
    """eng(img), step by step, keeping the steps."""
    h, w = img.shape[:2]
    skip = (not eng.use_text_det) or h <= eng.min_height or (eng.width_height_ratio != -1 and w / h > eng.width_height_ratio)
    if skip:
        boxes, parts = eng.get_boxes_img_without_det(img, h, w)
    else:
        boxes, _ = eng.text_detector(img)
        if boxes is None or len(boxes) < 1:
            return skip, [], [], []
        boxes = eng.sorted_boxes(boxes)
        parts = eng.get_crop_img_list(img, boxes)
    recs, _ = eng.text_recognizer(parts)
    keep = [(b, r) for b, r in zip(boxes, recs) if float(r[1]) >= eng.text_score]
    return skip, [np.asarray(b).tolist() for b in boxes], [[t, float(s)] for t, s in recs], \
        [[np.asarray(b).tolist(), t, float(s)] for b, (t, s) in keep]

def record(img, kind):
    skip, boxes, recs, result = answer(img)
    if checked[0] < 40:                 # the steps give what eng() gives
        res, _ = eng(img)
        want = [[np.asarray(b).tolist(), t, float(s)] for b, t, s in (res or [])]
        assert [(r[1], round(r[2], 5)) for r in want] == [(r[1], round(r[2], 5)) for r in result], (want, result)
        checked[0] += 1
    png = cv2.imencode(".png", img)[1].tobytes()
    sha = hashlib.sha1(png).hexdigest()
    if sha not in seen and counts.get(kind, 0) < caps.get(kind, 10**9):
        seen[sha] = True
        counts[kind] = counts.get(kind, 0) + 1
        (crops / f"{sha}.png").write_bytes(png)
        entries.append({"file": f"crops/{sha}.png", "kind": kind, "w": int(img.shape[1]), "h": int(img.shape[0]),
                        "skip_det": bool(skip), "boxes": boxes, "recs": recs, "result": result})
    return result

class Recorder:
    def __init__(self, kind): self.kind = kind
    def __call__(self, img):
        return "".join(r[1] for r in record(img, self.kind))
    def lines(self, img):
        out = []
        for box, text, conf in record(img, self.kind):
            xs = [p[0] for p in box]; ys = [p[1] for p in box]
            out.append(sd.OcrLine(x=int(min(xs)), y=int(min(ys)), w=int(max(xs) - min(xs)), h=int(max(ys) - min(ys)), text=text, conf=float(conf)))
        return out

plate, focus, chat_r = Recorder("plate"), Recorder("focus"), Recorder("chat")
sets = ["corpus", "corpus_nohdr", "corpus_winhdr", "corpus_cockpit", "corpus_16x9"]
n = 0
for s in sets:
    for p in sorted((DEV / s).glob("*.png")):
        f = cv2.imread(str(p), cv2.IMREAD_COLOR)
        sd.detect(f, plate)
        if n % 10 == 0:
            sd.detect_focused(f, focus)
        chat.read_located(f, chat_r)
        n += 1
        if n % 25 == 0:
            print(f"{n} frames, {len(entries)} crops {counts}", flush=True)
json.dump({"rapidocr": "1.2.3", "use_angle_cls": False, "frames": n, "crops": entries},
          open(OUT / "rapidocr.json", "w", encoding="utf-8"), ensure_ascii=True)
dets = sum(1 for e in entries if not e["skip_det"])
tilted = sum(1 for e in entries for b in e["boxes"] if not e["skip_det"]
             and not (b[0][1] == b[1][1] and b[2][1] == b[3][1] and b[0][0] == b[3][0] and b[1][0] == b[2][0]))
nb = sum(len(e["boxes"]) for e in entries if not e["skip_det"])
print(f"done: {n} frames, {len(entries)} crops {counts}; detector on {dets}; boxes {nb}, not upright rectangles {tilted}")
