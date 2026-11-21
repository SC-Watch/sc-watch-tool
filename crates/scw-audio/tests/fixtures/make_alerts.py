"""audio.Alerts' own decisions - which tones play and what is said, in
order - for tests/alerts.rs to require. The voice and the tone player are
replaced by recorders; everything else (the queue, the rationing, the
muted rules, render_line) is the Python's own.

    python make_alerts.py      (sc-watch's build environment, on Windows)

Made-up names only. Writes alerts.json.
"""
import json
import sys
import tempfile
from dataclasses import asdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[3]))
import paths  # noqa: E402

paths.DATA_ROOT = Path(tempfile.mkdtemp())      # never the real data folder
import audio  # noqa: E402


class Recorder(audio.Alerts):
    """Alerts with a recording voice and tone player."""

    def __init__(self, cfg, log):
        self.log = log

        class Voice:
            def Speak(_self, text, flags):
                log.append(["speak", text])

        class Tones:
            SND_FILENAME = 0x20000

            def PlaySound(_self, path, flags):
                log.append(["tone", Path(path).stem])

        self._voice = Voice()
        super().__init__(cfg)
        self._winsound = Tones()        # before anything is queued

    def _make_engine(self):
        return self._voice


PILOT = {"handle": "EXMPL_PILOT", "range_km": 2.4}
CONTACTS = {
    "none": [],
    "one routine": [{"handle": "EXMPL0NAME", "range_km": 4.0}],
    "three routine": [{"handle": f"EXMPL_{i}", "range_km": 1.0 + i} for i in range(3)],
    "five routine": [{"handle": f"EXMPL_{i}", "range_km": 1.0 + i} for i in range(5)],
    "flagged": [dict(PILOT, flags=["PIRACY: EXMPLORG (40 members)"])],
    "flagged muted": [dict(PILOT, flags=["Syndicate: EXMPLTWO"])],
    "known": [dict(PILOT, reason="2 unprovoked attacks",
                   said=[{"kind": "report", "label": "Unprovoked attack", "count": 2}])],
    "known said only": [dict(PILOT, said=[{"kind": "org", "org": "EXMPLORG"}])],
    "known org muted": [dict(PILOT, reason="member", said=[{"kind": "org", "org": "EXMPLORG"}])],
    "crowd with flags": [dict(PILOT, flags=["Smuggling: EXMPLBIG"])] +
                        [dict(PILOT, handle=f"EXMPL_F{i}", flags=["new account (12d)"]) for i in range(3)] +
                        [{"handle": f"EXMPL_{i}", "range_km": 5.0 + i} for i in range(4)],
    "duplicates": [{"handle": "EXMPL0NAME", "range_km": 4.0}, {"handle": "EXMPL0NAME", "range_km": 4.0},
                   dict(PILOT, flags=["Infiltration: EXMPLORG"])],
    "no range": [{"handle": "EXMPLHANDLE", "flags": ["PIRACY: EXMPLORG (40 members)"]}],
}
CONFIGS = {
    "default": {},
    "tones only": {"mode": "tones"},
    "speech only": {"mode": "speech"},
    "off": {"mode": "off"},
    "no handles": {"speak_handles": False},
    "one spoken": {"max_spoken_per_burst": 1},
    "muted": {"muted": ["flag_syndicate", "reason_org", "known"]},
    "summarise above 1": {"summarise_above": 1},
}
cases = []
for cname, overrides in CONFIGS.items():
    for kname, contacts in CONTACTS.items():
        cfg = audio.AudioConfig()
        for k, v in overrides.items():
            setattr(cfg, k, v)
        log = []
        a = Recorder(cfg, log)
        a.burst_result([dict(c) for c in contacts])
        a.event("trigger")
        a.say("a line as it is")
        a.close()
        a._thread.join(10)
        cases.append({"config": asdict(cfg), "contacts": contacts, "name": f"{cname} / {kname}", "log": log})
(HERE / "alerts.json").write_text(json.dumps(cases, indent=0), encoding="utf-8", newline="\n")
print(len(cases), "cases,", sum(len(c["log"]) for c in cases), "sounds and lines")
