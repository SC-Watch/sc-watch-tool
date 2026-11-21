"""audio.py's own answers, for tests/audio.rs to require exactly: template
parsing, check_phrase, render, the word helpers, whole lines, the settings
page's samples, the SAPI rate, the tone WAVs and AudioConfig.load.

    python make_fixtures.py     (sc-watch's build environment)

Made-up names only (EXMPL..., SAMPLE_PILOT). Writes cases.json and tones.bin.
"""
import json
import string
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())      # never the real data folder
import audio  # noqa: E402

TEMPLATES = ["", "plain", "{handle}", "Warning. {handle}. {distance}. {flags}", "{{literal}} {handle}",
             "a}}b", "}", "{", "{handle", "x{handle}y{range}z", "{handle!r}", "{handle:>10}", "{handle!}",
             "{handle!rx}", "{handle!r", "{handle:{range}}", "{a{b}", "{a[0]}", "{a[0}", "{}", "{0}",
             "{handle.upper}", "Known contact. {handle}. {range} kilometres. {reason}",
             "{range} km to {handle}", "{range}KM", "{range}   klicks.", "{range} kilometers away",
             "{range}kilometre", "  ;; {handle} ,, . {flags} ..", "a\t\tb\n\nc", "{count} contacts, {flagged} flagged",
             "é{handle}—", "Reported by {source}. {label}", "{label}.,;:", ". . .", "x \x1c\x1f y"]
VALS = [{}, {"handle": "Exmpl Pilot", "range": "2.4", "distance": "2.4 kilometres", "flags": "Piracy",
             "reason": "Member of Example Org", "count": "6", "flagged": "2", "label": "Unprovoked attack",
             "source": "Orgmate"},
        {"handle": "Exmpl", "range": "", "distance": "", "flags": "", "reason": ""},
        {"handle": "", "range": "1", "distance": "1 kilometre"}]

cases = {"parse": [], "check": [], "render": []}
for t in TEMPLATES:
    try:
        cases["parse"].append({"t": t, "ok": [[lit, name, spec, conv] for lit, name, spec, conv in
                                              string.Formatter().parse(t)]})
    except ValueError as e:
        cases["parse"].append({"t": t, "error": str(e)})
    for k in ("flagged", "known", "summary", "nothing", "reason_shared", "flag_piracy", "bogus"):
        cases["check"].append({"key": k, "t": t, "result": audio.check_phrase(k, t)})
    for v in VALS:
        cases["render"].append({"t": t, "vals": v, "out": audio.render(t, v)})
cases["check"].append({"key": "known", "t": "x" * 201, "result": audio.check_phrase("known", "x" * 201)})

cases["distance"] = [{"km": km, "out": list(audio.distance_words(km))} for km in
                     [None, 0.0, 0.04, 0.05, 0.25, 0.35, 1.0, 1.04, 2.4, 2.45, 10.0, 12.96, 100.0, 999.95, 1234.5, -0.0]]
HANDLES = ["", "EXMPL0NAME", "EXMPL_HANDLE", "exmpl-somename", "WZMBT", "XYZ", "ABCD", "AEIO", "BCDFGHJ_AEIOU",
           "MCDONALD", "o'neil", "2pac", "x  y", "Straße", "été", "A_B-C", "__", "SKY", "RHYTHM"]
cases["speakable"] = [{"h": h, "out": audio._speakable(h), "name": audio._speakable_name(h)} for h in HANDLES]
cases["speakable"] += [{"h": n, "out": audio._speakable(n), "name": audio._speakable_name(n)}
                       for n in ["Example Org", "  EXMPLORG  ", "two words here", "EXMPL"]]
FLAGS = [[], ["PIRACY: EXMPLORG (40 members)"], ["Syndicate: EXMPLTWO"], ["new account (12d)"],
         ["New Account (3 d)"], ["new account"], ["Smuggling: EXMPLBIG", "Infiltration: EXMPLORG"],
         ["PIRACY: EXMPLORG (40 members)", "Piracy: EXMPLTWO"], ["Bounty hunting: EXMPLORG"], ["weird"],
         ["  Infiltration :  EXMPL_IO-2  (9)"], [":nothing"], ["Piracy:"]]
MUTED = [[], ["flag_piracy"], ["flag_new_account", "flag_syndicate"]]
speech = audio.DEFAULT_SPEECH
custom = dict(speech, flag_piracy="pirates ({org})", flag_new_account="{days} days old")
cases["flags"] = [{"flags": f, "muted": m, "custom": c, "out": audio.flag_words(f, custom if c else speech, m),
                   "live": audio.live_flags(f, m)}
                  for f in FLAGS for m in MUTED for c in (False, True)]
SAID = [[], [{"kind": "report", "label": "Unprovoked attack", "count": 1}],
        [{"kind": "report", "label": "Griefing", "count": 3}], [{"kind": "report", "label": "x"}],
        [{"kind": "report", "label": "y", "count": 0}],
        [{"kind": "shared", "label": "Camping", "source": "EXMPL_FRIEND"}],
        [{"kind": "org", "org": "EXMPLORG"}], [{"kind": "former", "org": "Example Org"}],
        [{"kind": "watchlist"}], [{"kind": "other"}, {"kind": "org", "org": "EXMPLTWO"}]]
cases["reasons"] = [{"said": s, "muted": m, "out": audio.reason_words(s, speech, m)}
                    for s in SAID for m in [[], ["reason_org"], ["reason_report", "reason_reports"]]]
FACTS = [{}, {"handle": "EXMPL_PILOT", "range_km": 2.4, "flags": ["PIRACY: EXMPLORG (40 members)"]},
         {"handle": "EXMPL_PILOT", "flags": ["new account (12d)"]},
         {"handle": "EXMPLHANDLE", "range_km": 1.0, "said": SAID[2], "reason": "console line"},
         {"handle": "EXMPLHANDLE", "range_km": 0.0, "reason": "2 unprovoked attacks"},
         {"handle": "EXMPLHANDLE", "said": [], "reason": "fallback line"}, {"count": 6, "flagged": 2}]
cases["lines"] = []
for key in ("flagged", "known", "summary", "nothing"):
    for tpl in (speech[key], audio.LEGACY_DEFAULTS.get(key, ("",))[0]):
        for f in FACTS:
            for sh in (True, False):
                cases["lines"].append({"t": tpl, "facts": f, "speak_handles": sh,
                                       "out": audio.render_line(tpl, f, speech, sh, [])})
cases["samples"] = [{"key": k, "text": t, "sh": sh, "out": audio.sample(k, t, speech, sh)}
                    for k in audio.PHRASE_KEYS for t in (speech[k], "custom {org} {label} {count}", "")
                    for sh in (True, False)]
cases["rates"] = [{"wpm": w, "out": audio._sapi_rate(w)} for w in [175, 100, 156.63, 300, 50, 1, 10000, 0, -5]]

blob = bytearray()
cases["tones"] = []
tmp = Path(tempfile.mkdtemp())
for vol in (0.9, 0.5, 1.0):
    for event, steps in audio.DEFAULT_TONES.items():
        p = tmp / f"{event}.wav"
        audio.write_tone(p, steps, volume=vol)
        data = p.read_bytes()
        cases["tones"].append({"event": event, "volume": vol, "steps": steps, "at": len(blob), "len": len(data)})
        blob += data
(HERE / "tones.bin").write_bytes(bytes(blob))

CONFIGS = [None, b"{}", b'{"mode": "speech", "volume": 1, "rate": 200.0}', b'{"volume": "lots", "rate": true}',
           b'{"volume": Infinity, "speak_handles": 1, "voice": 5}',
           b'{"speech": {"known": "Known contact. {handle}. {range} kilometres. {reason}", "extra": "hi", "n": 1}}',
           b'{"muted": ["flag_piracy", "bogus", 3, "reason_org"]}', b'{"tones": {"trigger": [[500, 40]]}}',
           b'{"tones": []}', b'[1, 2]', b'not json', b'{"save": 1, "mode": "loud"}', b'{"max_spoken_per_burst": 2.0}']
cases["configs"] = []
for i, c in enumerate(CONFIGS):
    p = tmp / f"audio{i}.json"
    if c is not None:
        p.write_bytes(c)
    from dataclasses import asdict
    cfg = audio.AudioConfig.load(p)
    cases["configs"].append({"file": None if c is None else c.decode(), "loaded": json.dumps(asdict(cfg))})
(HERE / "cases.json").write_text(json.dumps(cases, indent=0), encoding="utf-8", newline="\n")
print({k: len(v) for k, v in cases.items()})
