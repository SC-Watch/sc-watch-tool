# chat.py's own answers, for scw-ocr's chat reader (tests/chat_parity.rs):
#
#   chat_ops.json (committed): handles_in on generated lines - every channel
#     tag and its OCR spellings, every closing-bracket misread, spaces
#     (Unicode ones too), digits, NOT_HANDLES, long and short handles,
#     several tags in one line - and default_region / crop_region on screen
#     sizes and set regions. Placeholder handles only.
#   <parity-data>/chat/chat.json (local only - real frames name real
#     players): read_located on every frame of the five corpora, the
#     region worked out from the frame and (every tenth frame) one set by
#     hand, with each OCR call: its input's key and RapidOCR's lines.
#
# Run with the Python tree's build venv (rapidocr-onnxruntime 1.2.3):
#
#   python make_chat_ref.py <python tree> <scratch dir> <parity-data>/chat
import sys, pathlib, json, hashlib, random
DEV = pathlib.Path(sys.argv[1]); SCR = pathlib.Path(sys.argv[2]); OUT = pathlib.Path(sys.argv[3])
sys.path.insert(0, str(DEV))
import paths
paths.DATA_ROOT = SCR / "data"          # before anything else of sc-watch's
import cv2, numpy as np
import chat
import sc_detector as sd
from rapidocr_onnxruntime import RapidOCR

HERE = pathlib.Path(__file__).resolve().parent
rng = random.Random(20261127)

# ---- lines ---------------------------------------------------------------
tags = ["GLOBAL", "GL0BAL", "GLOB4L", "GL0B4L", "LOCAL", "L0C4L", "PARTY", "P4RTY", "ORG", "0RG",
        "SYSTEM", "TEAM", "TE4M", "GUILD", "SQUAD", "SQU4D", "ADMIN", "4DMIN", "SERVER",
        "GLOBA", "LOCALL", "Global", "GL0BALI", "ORGG", "SYS"]
opens = ["", "[", "(", "{", " [", "[ "]
closes = ["", "]", ")", "}", "I", "J", "|", "1", "l", "] ", " ]", "I ", "L"]
handles = ["EXMPLONE", "EXMPL_two", "exmpl-3", "Ex", "E", "IEXMPL", "JEXMPL", "1EXMPL", "lexmpl",
           "12345", "07", "SYSTEM", "global", "Notice", "all", "X" * 40, "Y" * 41, "Z" * 39 + "-",
           "EXMPL.DOT", "EXMPLÜML", "EXMPL１", "_", "--"]
seps = [":", " :", ":  ", "　:", " ", "", ";", "::", ": hello: there"]
lines = set()
for _ in range(4000):
    s = (rng.choice(["", "", "x ", "12:04 ", "[GLOBAL]"]) + rng.choice(opens) + rng.choice(tags)
         + rng.choice(["", " ", "  ", "　", "\t"]) + rng.choice(closes)
         + rng.choice(["", " ", "　"]) + rng.choice(handles) + rng.choice(seps)
         + rng.choice(["", " o7", " [LOCAL] EXMPLTWO: hi", "thanks"]))
    lines.add(s)
lines |= {"", ":", "[GLOBAL]", "[GLOBAL] :", "[GLOBALIExmplkillsX:", "[GLOBAL]]EXMPL:", "BALI EXMPLHANDLE: x",
          "[GLOBAL) EXMPLONE:thanks", "[GLOBAL]EXMPLONE:07", "(LOCAL EXMPLONE : hi", "[[GLOBAL] EXMPL: x",
          "[GLOBAL] EXMPL [LOCAL] EXMPLTWO: x", "GLOBALGLOBAL EXMPL: x"}
lines = sorted(lines)
cases = [[s, chat.handles_in(s)] for s in lines]

sizes = [(5120, 1440), (3440, 1440), (2559, 1439), (2560, 1440), (1920, 1080), (1600, 900),
         (3840, 2160), (1280, 720), (800, 600), (2000, 1500), (1024, 768), (7680, 2160)]
regions = [[w, h, helmet, list(chat.default_region(w, h, helmet))]
           for (w, h) in sizes for helmet in (True, False)]
crops = []
for (w, h) in sizes:
    f = np.zeros((h, w, 3), np.uint8)
    for region in [None, (0.1, 0.5, 0.4, 0.2), (0.0, 0.0, 1.0, 1.0), (0.333, 0.333, 0.333, 0.333),
                   (0.9, 0.9, 0.5, 0.5), (0.15, 0.30, 0.35, 0.45)]:
        r = region if region is not None else chat.default_region(w, h)
        crop, (x0, y0) = chat.crop_region(f, region)
        crops.append([w, h, list(r), int(x0), int(y0), int(crop.shape[1]), int(crop.shape[0])])
json.dump({"handles_in": cases, "default_region": regions, "crop_region": crops},
          open(HERE / "chat_ops.json", "w", encoding="utf-8"), ensure_ascii=True, indent=0)
print(f"chat_ops.json: {len(cases)} lines, {sum(1 for c in cases if c[1])} with a handle; "
      f"{len(regions)} regions, {len(crops)} crops")

# ---- frames --------------------------------------------------------------
OUT.mkdir(parents=True, exist_ok=True)
eng = RapidOCR(**sd._ocr_kwargs())
eng.use_angle_cls = False               # as sc_detector.make_ocr
memo = {}


def key(img):
    return hashlib.sha1(img.tobytes()).hexdigest() + f":{img.shape[1]}x{img.shape[0]}x{img.shape[2] if img.ndim == 3 else 1}"


class Ocr:
    def __init__(self):
        self.calls = []

    def lines(self, img):
        k = key(img)
        if k not in memo:
            res, _ = eng(img)
            out = []
            for box, text, conf in (res or []):
                xs = [p[0] for p in box]; ys = [p[1] for p in box]
                out.append([int(min(xs)), int(min(ys)), int(max(xs) - min(xs)), int(max(ys) - min(ys)),
                            text, float(conf)])
            memo[k] = out
        self.calls.append([k, memo[k]])
        return [sd.OcrLine(*l) for l in memo[k]]


SET = (0.2, 0.4, 0.3, 0.25)
sets = ["corpus", "corpus_nohdr", "corpus_winhdr", "corpus_cockpit", "corpus_16x9"]
paths_ = [p for s in sets for p in sorted((DEV / s).glob("*.png"))]
(OUT / "frames.txt").write_text("".join(f"{p}\n" for p in paths_), encoding="utf-8")
frames = []
for n, p in enumerate(paths_):
    f = cv2.imread(str(p), cv2.IMREAD_COLOR)
    rec = {}
    for name, region in [("auto", None)] + ([("set", SET)] if n % 10 == 0 else []):
        o = Ocr()
        got, place = chat.read_located(f, o, region)
        rec[name] = {"lines": [[c.handle, c.text, c.conf] for c in got], "place": place, "ocr": o.calls}
    frames.append(rec)
    if (n + 1) % 50 == 0:
        print(f"{n + 1}/{len(paths_)} frames", flush=True)
json.dump({"rapidocr": "1.2.3", "set_region": list(SET), "frames": frames},
          open(OUT / "chat.json", "w", encoding="utf-8"), ensure_ascii=True)
print(f"done: {len(frames)} frames, {len(memo)} distinct crops; frames with handles "
      f"{sum(1 for r in frames if r['auto']['lines'])}, handles {sum(len(r['auto']['lines']) for r in frames)}; "
      f"places {sorted(set(r['auto']['place'] for r in frames))}")
