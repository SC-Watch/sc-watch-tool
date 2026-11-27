# The order RapidOCR batches each crop's lines in: np.argsort of their
# width / height, numpy's default sort - which orders equal ratios its own
# way (vectorised, CPU-dependent) once there are more than 16. Written only
# where it is not the stable order, beside rapidocr.json:
#
#   python make_ocr_order.py <parity-data>/ocr
import sys, json, pathlib
import cv2, numpy as np
from rapidocr_onnxruntime import RapidOCR
OUT = pathlib.Path(sys.argv[1])
eng = RapidOCR()
ref = json.load(open(OUT / "rapidocr.json", encoding="utf-8"))
orders = {}
for e in ref["crops"]:
    if e["skip_det"] or len(e["boxes"]) < 2:
        continue
    img = cv2.imread(str(OUT / e["file"]), cv2.IMREAD_COLOR)
    crops = eng.get_crop_img_list(img, np.array(e["boxes"], dtype=np.float32))
    wl = np.array([c.shape[1] / float(c.shape[0]) for c in crops])
    a, b = np.argsort(wl), np.argsort(wl, kind="stable")
    if (a != b).any():
        orders[e["file"]] = a.tolist()
json.dump(orders, open(OUT / "rapidocr_order.json", "w"))
print(f"{len(orders)} crops batch in an order other than the stable one")
