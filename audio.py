# sc-watch - reads player handles off the Star Citizen HUD.
# Copyright (C) 2026 SC-Watch
#
# This program is free software: you can redistribute it and/or modify it
# under the terms of the GNU General Public License as published by the Free
# Software Foundation, either version 3 of the License, or (at your option)
# any later version.
#
# This program is distributed in the hope that it will be useful, but WITHOUT
# ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
# FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
# more details. You should have received a copy of the GNU General Public
# License along with this program. If not, see <https://www.gnu.org/licenses/>.

"""
Audible alerts: tones, speech, or both.

The console is useless while you are flying, which is the whole point of this
module. Two problems shape the design:

  SPEECH IS SLOW      "Jimmy Hawking, two point four kilometres, infiltration
                      org" takes about four seconds. Five contacts at once is
                      twenty seconds of talking, by which time you have flown
                      somewhere else. So speech is rationed: flagged contacts
                      first, a cap per burst, and a summary instead of a list
                      when there are too many.

  HANDLES DO NOT      TTS mangles WZOMBIETPANDAF, and a mangled name is worse
  PRONOUNCE           than no name. For a warning system "flagged contact, two
                      kilometres" is the actionable part; the name is what the
                      screen is for. Speaking the handle is therefore optional
                      and off by default for routine contacts.

Hence the default mode is `both`: tones for routine events, speech kept for
things that actually warrant interrupting you.

Everything is configured from audio.json so the UI can edit it later, and the
generated tones are plain WAV files you can replace with your own.

    python audio.py --demo          # play every event
    python audio.py --make-tones    # regenerate the WAVs
"""

from __future__ import annotations

import json
import math
import queue
import struct
import threading
import wave
from dataclasses import asdict, dataclass, field
from pathlib import Path
import paths

CONFIG_PATH = paths.data("audio.json")
# Writable: absent, make_tones() generates them. They ship with the build,
# but they are also replaceable by anyone who wants their own sounds.
TONE_DIR = paths.data_dir("tones")

# Every event the pipeline can raise. Both modes key off this same set, so a
# new event only has to be defined once.
EVENTS = ("trigger", "contact", "flagged", "known", "nothing", "error")

# freq/duration pairs per event, in Hz and ms. Chosen to be distinguishable
# without thinking about it: rising = fine, falling = attention, buzz = broken.
DEFAULT_TONES = {
    "trigger": [(660, 60)],
    "contact": [(780, 70), (980, 90)],
    "flagged": [(1180, 90), (880, 90), (1180, 140)],
    "known":   [(1180, 90), (760, 160)],
    "nothing": [(420, 110)],
    "error":   [(240, 180), (200, 220)],
}

DEFAULT_SPEECH = {
    # {handle} {range} {flags} {count} {reason}
    "flagged": "Warning. {handle}. {range} kilometres. {flags}",
    "known":   "Known contact. {handle}. {range} kilometres. {reason}",
    "contact": "",           # silent by default - tones cover routine contacts
    "nothing": "",
    "trigger": "",
    "error":   "Alert system error",
    "summary": "{count} contacts, {flagged} flagged",
}


@dataclass
class AudioConfig:
    mode: str = "both"           # off | tones | speech | both
    volume: float = 0.9
    rate: int = 175              # words per minute
    voice: str = ""              # substring match on an installed voice name
    speak_handles: bool = True   # off -> "flagged contact" instead of the name
    max_spoken_per_burst: int = 2
    summarise_above: int = 3     # more contacts than this -> speak a summary
    tones: dict = field(default_factory=lambda: {k: list(v) for k, v in
                                                 DEFAULT_TONES.items()})
    speech: dict = field(default_factory=lambda: dict(DEFAULT_SPEECH))

    @classmethod
    def load(cls, path: Path | None = None) -> "AudioConfig":
        path = Path(path) if path else CONFIG_PATH
        if path.exists():
            try:
                data = json.loads(path.read_text(encoding="utf-8-sig"))
                base = cls()
                for k, v in data.items():
                    if hasattr(base, k):
                        setattr(base, k, v)
                return base
            except Exception:
                pass
        return cls()

    def save(self, path: Path | None = None):
        path = Path(path) if path else CONFIG_PATH
        path.write_text(json.dumps(asdict(self), indent=2))


# --------------------------------------------------------------------------
# Tone generation - stdlib only, so the WAVs are yours to replace
# --------------------------------------------------------------------------


def write_tone(path: Path, steps, volume=0.9, rate=44100):
    """Sine segments with a short fade in/out, so they don't click."""
    frames = bytearray()
    for freq, ms in steps:
        n = int(rate * ms / 1000)
        fade = max(1, int(rate * 0.006))
        for i in range(n):
            env = min(1.0, i / fade, (n - i) / fade)
            v = math.sin(2 * math.pi * freq * i / rate) * env * volume
            frames += struct.pack("<h", int(max(-1.0, min(1.0, v)) * 32767))
    path.parent.mkdir(exist_ok=True)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(bytes(frames))


def make_tones(cfg: AudioConfig | None = None) -> list[Path]:
    cfg = cfg or AudioConfig()
    out = []
    for event, steps in cfg.tones.items():
        p = TONE_DIR / f"{event}.wav"
        write_tone(p, [tuple(s) for s in steps], volume=cfg.volume)
        out.append(p)
    return out


# --------------------------------------------------------------------------
# Player
# --------------------------------------------------------------------------


class Alerts:
    """Non-blocking audio. Never call this from the read loop and wait."""

    def __init__(self, cfg: AudioConfig | None = None):
        self.cfg = cfg or AudioConfig.load()
        self.available = {"tones": False, "speech": False}
        self._q: queue.Queue = queue.Queue(maxsize=32)
        self._engine = None
        self._winsound = None

        if self.cfg.mode in ("tones", "both"):
            try:
                import winsound
                self._winsound = winsound
                if not TONE_DIR.exists() or not any(TONE_DIR.glob("*.wav")):
                    make_tones(self.cfg)
                self.available["tones"] = True
            except Exception:
                pass

        if self.cfg.mode in ("speech", "both"):
            try:
                import pyttsx3
                self._engine = pyttsx3.init()
                self._engine.setProperty("rate", self.cfg.rate)
                self._engine.setProperty("volume", self.cfg.volume)
                if self.cfg.voice:
                    for v in self._engine.getProperty("voices"):
                        if self.cfg.voice.lower() in (v.name or "").lower():
                            self._engine.setProperty("voice", v.id)
                            break
                self.available["speech"] = True
            except Exception:
                pass

        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    # -- public ---------------------------------------------------------

    def event(self, name: str, **fields):
        """Raise an event. Returns immediately; audio happens on the worker."""
        if self.cfg.mode == "off":
            return
        try:
            self._q.put_nowait((name, fields))
        except queue.Full:
            # Dropping is correct: stale alerts are worse than missing ones,
            # and a backed-up queue would talk over the next encounter.
            pass

    def burst_result(self, contacts):
        """Announce a whole burst, rationing speech.

        contacts: list of dicts with handle, range_km, flags (list), reason.
        """
        if self.cfg.mode == "off":
            return
        if not contacts:
            self.event("nothing")
            return

        flagged = [c for c in contacts if c.get("flags") or c.get("reason")]

        # Too many to read out: say how many, not who. A list of six handles
        # takes half a minute and by then it is history.
        if len(contacts) > self.cfg.summarise_above and not flagged:
            self.event("contact", tone_only=True)
            self.event("summary", count=len(contacts), flagged=len(flagged))
            return

        for c in flagged[:self.cfg.max_spoken_per_burst]:
            self.event("known" if c.get("reason") else "flagged", **c)
        if len(flagged) > self.cfg.max_spoken_per_burst:
            self.event("summary", count=len(contacts), flagged=len(flagged))
        for c in contacts:
            if c not in flagged:
                self.event("contact", tone_only=True, **c)

    def close(self):
        self._q.put((None, None))

    # -- worker ---------------------------------------------------------

    def _run(self):
        while True:
            name, fields = self._q.get()
            if name is None:
                return
            try:
                self._play(name, fields or {})
            except Exception:
                pass

    def _play(self, name: str, fields: dict):
        tone_only = fields.pop("tone_only", False)

        if (self.available["tones"] and name in EVENTS
                and self.cfg.mode in ("tones", "both")):
            path = TONE_DIR / f"{name}.wav"
            if path.exists():
                # SND_ASYNC would cut the previous tone off mid-play; these
                # are under 300ms, so letting them finish keeps them legible.
                self._winsound.PlaySound(str(path), self._winsound.SND_FILENAME)

        if tone_only or not self.available["speech"]:
            return
        if self.cfg.mode not in ("speech", "both"):
            return

        template = self.cfg.speech.get(name, "")
        if not template:
            return
        text = self._render(template, fields)
        if text:
            self._engine.say(text)
            self._engine.runAndWait()

    def _render(self, template: str, fields: dict) -> str:
        handle = fields.get("handle", "")
        if not self.cfg.speak_handles:
            handle = "contact"
        rng = fields.get("range_km")
        flags = fields.get("flags") or []
        vals = {
            "handle": _speakable(handle),
            "range": f"{rng:.1f}".rstrip("0").rstrip(".") if rng is not None else "",
            "flags": ", ".join(f.split(":")[0] for f in flags),
            "reason": (fields.get("reason") or ""),
            "count": fields.get("count", ""),
            "flagged": fields.get("flagged", ""),
        }
        try:
            return " ".join(template.format(**vals).split())
        except Exception:
            return ""


def _speakable(handle: str) -> str:
    """Give TTS a fighting chance at a handle.

    Underscores read as silence, and a run of consonants like WZOMBIET comes
    out as noise. Splitting on separators and spacing out unpronounceable runs
    is not perfect, but it beats the raw string.
    """
    if not handle:
        return ""
    s = handle.replace("_", " ").replace("-", " ")
    out = []
    for word in s.split():
        vowels = sum(c in "AEIOUY" for c in word.upper())
        if len(word) > 3 and vowels <= 1:
            out.append(" ".join(word))  # spell it out
        else:
            out.append(word.title())
    return " ".join(out)


def main():
    import argparse
    import time
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--demo", action="store_true", help="play every event")
    ap.add_argument("--make-tones", action="store_true")
    ap.add_argument("--mode", choices=("off", "tones", "speech", "both"))
    ap.add_argument("--save", action="store_true", help="write audio.json")
    args = ap.parse_args()

    cfg = AudioConfig.load()
    if args.mode:
        cfg.mode = args.mode
    if args.make_tones:
        for p in make_tones(cfg):
            print(f"  wrote {p}")
    if args.save:
        cfg.save()
        print(f"saved {CONFIG_PATH.name}")

    a = Alerts(cfg)
    print(f"mode={cfg.mode}  tones={a.available['tones']}  "
          f"speech={a.available['speech']}")
    if not args.demo:
        return 0

    demos = [
        ("trigger", {}),
        ("contact", {"handle": "KELGRO", "range_km": 4.0, "tone_only": True}),
        ("flagged", {"handle": "JIMMY_HAWKING", "range_km": 2.4,
                     "flags": ["Infiltration: MERRILLS"]}),
        ("known", {"handle": "WZOMBIETPANDAF", "range_km": 2.8,
                   "reason": "2 unprovoked attacks"}),
        ("nothing", {}),
        ("error", {}),
    ]
    for name, fields in demos:
        print(f"  {name}: {fields.get('handle', '')}")
        a.event(name, **fields)
        time.sleep(2.2)
    time.sleep(1)
    a.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
