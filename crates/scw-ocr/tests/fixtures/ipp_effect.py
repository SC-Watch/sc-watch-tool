# What Intel's IPP changes in the legacy reader's reads: legacy_ipp.json (the
# cubic enlargement in prep_for_ocr as the opencv-python wheel runs it, IPP
# on) against legacy.json (OpenCV's own code, which the port matches). Counts
# only - the reads name real players.
#
#   python ipp_effect.py <parity-data>/legacy
import json, sys, pathlib
D = pathlib.Path(sys.argv[1])
a = json.load(open(D / "legacy_ipp.json", encoding="utf-8"))["frames"]
b = json.load(open(D / "legacy.json", encoding="utf-8"))["frames"]
assert len(a) == len(b)


def players(cs):
    return sorted((c[3], c[2]) for c in cs if c[4] == "player")


for mode in ["detect", "detect_gate", "detect_haze", "detect_orphan", "focused", "focused_mask", "dual"]:
    get = (lambda r: r[mode]) if mode == "dual" else (lambda r: r[mode]["contacts"])
    frames = same = 0
    n_a = n_b = only_a = only_b = names_a = names_b = 0
    for ra, rb in zip(a, b):
        ca, cb = get(ra), get(rb)
        frames += 1
        same += ca == cb
        pa, pb = players(ca), players(cb)
        n_a += len(pa); n_b += len(pb)
        sa, sb = set(n for n, _ in pa), set(n for n, _ in pb)
        names_a += len(sa); names_b += len(sb)
        only_a += len(sa - sb); only_b += len(sb - sa)
    print(f"{mode:13s} frames identical {same}/{frames}; player reads IPP {n_a} / own {n_b}; "
          f"names per frame only with IPP {only_a}, only without {only_b}")
