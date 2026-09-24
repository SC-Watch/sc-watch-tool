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

import math
import queue
import re
import string
import struct
import threading
import time
import wave
from dataclasses import asdict, dataclass, field, fields
from pathlib import Path
import jsonstore
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

# EVERYTHING THAT CAN BE SAID. Each entry: key, group, title, when it is
# said, the placeholders it may use, and its default. The settings page is
# drawn from this list, the saved phrases are checked against it, and nothing
# else in the program decides what is spoken - so a phrase added here is a
# phrase you can change.
#
#   line     a whole utterance, for one kind of event
#   reason   what fills {reason} in "known": the FIRST of these that applies,
#            in this order, because speech is rationed to one reason
#   flag     what fills {flags} in "flagged", one per flag, joined by commas
#
# {distance} is "2.4 kilometres", or NOTHING when there is no range - a
# contact read from chat has none. It replaced "{range} kilometres", which
# said "kilometres" with no number in front of it whenever the range was
# missing. {range} is still there, the bare number, for anyone who wants it.
PHRASES = (
    ("flagged", "line", "Lookup warning",
     "A lookup found an org flag on someone: a piracy, smuggling or "
     "infiltration focus, a syndicate org, or a new account. It arrives a few "
     "seconds after the read, once the lookup answers.",
     ("handle", "distance", "range", "flags"),
     "Warning. {handle}. {distance}. {flags}"),
    ("known", "line", "Known contact",
     "Someone your own records flag - your reports, a flagged org, the watch "
     "list, or reports someone shared with you. Said as soon as the read "
     "settles.",
     ("handle", "distance", "range", "reason"),
     "Known contact. {handle}. {distance}. {reason}"),
    ("summary", "line", "Crowd summary",
     "Too many contacts to name one by one, or more flagged ones than you "
     "allow per read (Audio settings).",
     ("count", "flagged"),
     "{count} contacts, {flagged} flagged"),
    ("nothing", "line", "Nothing found",
     "A read that found nobody. Blank plays the tone alone.",
     (), ""),
    ("reason_report", "reason", "Your report",
     "You have filed one report on them. {label} is the reason's spoken name "
     "- set it per reason under Report reasons, 'spoken as'.",
     ("label",), "{label}"),
    ("reason_reports", "reason", "Your reports",
     "You have filed several; {label} is the reason you used most.",
     ("label", "count"), "{count} reports. {label}"),
    ("reason_shared", "reason", "A shared report",
     "Reports someone else shared with you, and none of your own. {source} is "
     "the name they gave when they exported.",
     ("label", "source"), "Reported by {source}. {label}"),
    ("reason_org", "reason", "Flagged org member",
     "They are on the roster of an org you flagged, or you put them on it.",
     ("org",), "Member of {org}"),
    ("reason_former", "reason", "Former flagged org member",
     "An org you flagged used to list them and no longer does.",
     ("org",), "Formerly {org}"),
    ("reason_watchlist", "reason", "On the watch list",
     "An exact match on a watch list - including the one that came with "
     "sc-watch.",
     (), "On the watch list"),
    ("flag_piracy", "flag", "Piracy org",
     "One of their orgs declares a Piracy focus.",
     ("org",), "Piracy"),
    ("flag_syndicate", "flag", "Syndicate org",
     "One of their orgs is a Syndicate.",
     ("org",), "Syndicate"),
    ("flag_smuggling", "flag", "Smuggling org",
     "One of their orgs declares a Smuggling focus.",
     ("org",), "Smuggling"),
    ("flag_infiltration", "flag", "Infiltration org",
     "One of their orgs declares an Infiltration focus.",
     ("org",), "Infiltration"),
    ("flag_new_account", "flag", "New account",
     "The account is under 90 days old.",
     ("days",), "New account"),
)
PHRASE_KEYS = tuple(k for k, *_ in PHRASES)
ALLOWED = {k: set(ph) for k, _g, _t, _w, ph, _d in PHRASES}

DEFAULT_SPEECH = {k: d for k, _g, _t, _w, _ph, d in PHRASES}
DEFAULT_SPEECH.update({
    # Events with no entry above: never spoken by default, and not offered
    # for editing. A routine contact is a tone - bursts raise it tone-only -
    # and nothing raises "error" any more.
    "contact": "",
    "trigger": "",
    "error": "Alert system error",
})

# What earlier versions saved as their defaults. A copy of one of these in
# audio.json is the old default, not a choice, so it is upgraded on load.
LEGACY_DEFAULTS = {
    "flagged": ("Warning. {handle}. {range} kilometres. {flags}",),
    "known": ("Known contact. {handle}. {range} kilometres. {reason}",),
}
MAX_PHRASE = 200


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
    # Phrases switched OFF. An org flag or a reason that is off stops that
    # thing announcing a contact at all - a syndicate org you do not care
    # about is then an ordinary contact. A whole line that is off keeps its
    # tone and loses its words.
    muted: list = field(default_factory=list)

    @classmethod
    def load(cls, path: Path | None = None) -> "AudioConfig":
        """audio.json over the defaults, one field at a time.

        Each value must have its default's type or it is ignored. This used
        to setattr() whatever the file said: `"volume": "lots"` became the
        string 'lots' and `Infinity` became inf, both handed straight to the
        speech engine. And it checked names with hasattr(), which is true of
        METHODS - `"save": 1` would have replaced save() on the instance.
        """
        base = cls()
        data = jsonstore.read(Path(path) if path else CONFIG_PATH) or {}
        for f in fields(cls):
            if f.name in data:
                setattr(base, f.name,
                        jsonstore.coerce(getattr(base, f.name), data[f.name]))
        base.speech = merge_speech(base.speech)
        base.muted = [k for k in base.muted if isinstance(k, str) and k in ALLOWED]
        return base

    def save(self, path: Path | None = None):
        jsonstore.write(Path(path) if path else CONFIG_PATH, asdict(self))


def merge_speech(saved: dict) -> dict:
    """The saved phrases over the defaults. A key the file lacks - every
    phrase added since it was written - gets its default; a saved copy of an
    OLD default becomes the new one; anything else you wrote is kept."""
    out = dict(DEFAULT_SPEECH)
    for k, v in (saved or {}).items():
        if not isinstance(v, str):
            continue
        out[k] = DEFAULT_SPEECH.get(k, v) if v in LEGACY_DEFAULTS.get(k, ()) else v
    return out


def check_phrase(key: str, text: str) -> str | None:
    """Why a phrase cannot be saved, or None. Only the placeholders that
    phrase can fill: an unknown one used to make the whole line fail to
    format, and the alert went out as silence."""
    if key not in ALLOWED:
        return f"{key} is not a phrase"
    if len(text) > MAX_PHRASE:
        return f"keep it under {MAX_PHRASE} characters"
    try:
        parts = list(string.Formatter().parse(text))
    except ValueError as exc:
        return f"unbalanced braces ({exc})"
    for _lit, name, spec, conv in parts:
        if name is None:
            continue
        if name not in ALLOWED[key]:
            if not ALLOWED[key]:
                return f"{{{name}}} is not available here - this one takes no placeholders"
            ok = ", ".join("{" + p + "}" for p in sorted(ALLOWED[key]))
            return f"{{{name}}} is not available here - this one can use {ok}"
        if spec or conv:
            return f"{{{name}}} takes no formatting"
    return None


class _Blank(dict):
    def __missing__(self, key):
        return ""


# "{range} kilometres" in a phrase written before {distance} existed: with no
# range, the unit has nothing to measure and goes too.
_UNIT_AFTER_RANGE = re.compile(
    r"\{range\}\s*(kilomet(?:re|er)s?|km|klicks?)\b", re.I)


def render(template: str, vals: dict) -> str:
    """Fill a phrase and tidy what an empty placeholder leaves behind."""
    if not template:
        return ""
    if not vals.get("range"):
        template = _UNIT_AFTER_RANGE.sub("{range}", template)
    try:
        text = string.Formatter().vformat(template, (), _Blank(vals))
    except (ValueError, IndexError):
        return ""
    # "Warning. Jimmy. . Piracy" -> "Warning. Jimmy. Piracy"
    text = re.sub(r"\s+", " ", text)
    text = re.sub(r"\s+([.,;:])", r"\1", text)
    text = re.sub(r"([.,;:])(?:\s*[.,;:])+", r"\1", text)
    return text.strip(" .,;:")


def distance_words(rng) -> tuple[str, str]:
    """({range}, {distance}) for a range in km, or ("", "") without one."""
    if rng is None:
        return "", ""
    try:
        num = f"{float(rng):.1f}".rstrip("0").rstrip(".")
    except (TypeError, ValueError):
        return "", ""
    return num, f"{num} kilometre" + ("" if num == "1" else "s")


_FLAG_RE = re.compile(r"^\s*([A-Za-z][A-Za-z ]*?)\s*:\s*([A-Za-z0-9_-]+)")
_DAYS_RE = re.compile(r"(\d+)\s*d\b")


def _flag_parts(f) -> tuple[str | None, dict, str]:
    """One rsi.Citizen.flags string -> (its phrase key, or None for a kind
    with no phrase; the values its phrase can use; words to say without one)."""
    f = str(f)
    if f.lower().startswith("new account"):
        m = _DAYS_RE.search(f)
        return "flag_new_account", {"days": m.group(1) if m else ""}, "New account"
    m = _FLAG_RE.match(f)
    kind = (m.group(1) if m else f.split(":")[0]).strip()
    key = "flag_" + kind.lower().replace(" ", "_")
    return (key if key in ALLOWED else None,
            {"org": _speakable(m.group(2)) if m else ""}, kind.title())


def live_flags(flags, muted=()) -> list:
    """The flags whose phrase is switched on."""
    return [f for f in flags or [] if _flag_parts(f)[0] not in muted]


def flag_words(flags, speech: dict, muted=()) -> str:
    """The {flags} of a lookup warning, from rsi.Citizen.flags strings -
    "PIRACY: SID (40 members)", "Syndicate: SID", "new account (12d)" - each
    through its phrase. The same phrase twice (two piracy orgs) is said once;
    a flag whose phrase is off is not said at all."""
    out = []
    for f in live_flags(flags, muted):
        key, vals, plain = _flag_parts(f)
        said = render(speech.get(key, ""), vals) if key else plain
        if said and said not in out:
            out.append(said)
    return ", ".join(out)


def reason_key(said: dict) -> str | None:
    """Which phrase speaks one reason from Assessment.spoken_all()."""
    kind = said.get("kind", "")
    if kind == "report":
        return "reason_reports" if (said.get("count") or 1) > 1 else "reason_report"
    return {"shared": "reason_shared", "org": "reason_org",
            "former": "reason_former", "watchlist": "reason_watchlist"}.get(kind)


def pick_reason(said, muted=()) -> dict | None:
    """The first reason whose phrase is on. `said` is the list from
    Assessment.spoken_all(), strongest first - or one reason on its own."""
    for r in (said if isinstance(said, list) else [said]):
        if isinstance(r, dict) and reason_key(r) and reason_key(r) not in muted:
            return r
    return None


def reason_words(said, speech: dict, muted=()) -> str:
    """The {reason} of a known-contact line. A plain string is an older
    caller's console line, said as it is."""
    if isinstance(said, str):
        return said
    r = pick_reason(said, muted)
    if r is None:
        return ""
    vals = {"label": r.get("label", ""), "count": r.get("count", ""),
            "source": _speakable_name(r.get("source", "")),
            "org": _speakable_name(r.get("org", ""))}
    return render(speech.get(reason_key(r), ""), vals)


def _speakable_name(name: str) -> str:
    """An org or person's name as given - spaced words are left alone, a
    run-together SID gets the same treatment a handle does."""
    name = (name or "").strip()
    return name if " " in name else _speakable(name)


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

# SpeechVoiceSpeakFlags: 0 is synchronous (SVSFDefault), 16 is SVSFIsNotXML.
_SVSF_SYNC_PLAIN = 16

# An alert still waiting this long is history, and is dropped - tone and all.
# Once speech really spoke every line (it takes 6-7 s each), a fight could
# queue faster than the voice could talk: measured with a fake voice, sixteen
# flagged contacts read over forty seconds of pings left the last warning
# starting 63 s after its contact was read. "Warning, <name>, 3 kilometres"
# a minute late is about someone who has moved on. The queue's own cap (32)
# never came into it. Never applied to the settings page's "hear it".
MAX_ALERT_AGE = 20.0


def _sapi_rate(wpm) -> int:
    """Words per minute -> SAPI's -10..10, on the curve pyttsx3 used, so the
    Speech rate setting sounds as it did before (175 -> 1)."""
    try:
        return max(-10, min(10, int(math.log(float(wpm) / 156.63, 1.11))))
    except (TypeError, ValueError, ZeroDivisionError):
        return 0


class Alerts:
    """Non-blocking audio. Never call this from the read loop and wait."""

    def __init__(self, cfg: AudioConfig | None = None):
        self.cfg = cfg or AudioConfig.load()
        self.available = {"tones": False, "speech": False}
        self._q: queue.Queue = queue.Queue(maxsize=32)
        self._engine = None
        self._winsound = None
        # Health counters. Speech failing silently was the whole problem, so
        # the numbers are kept and reported rather than discarded.
        self.failures = 0
        self.repairs = 0
        self.expired = 0            # alerts dropped for waiting too long
        self.last_error = ""
        # Phrases are re-read when audio.json changes, so an edit on the
        # settings page is heard on the next alert without restarting.
        self._phrases_mtime = self._mtime()

        if self.cfg.mode in ("tones", "both"):
            try:
                import winsound
                self._winsound = winsound
                if not TONE_DIR.exists() or not any(TONE_DIR.glob("*.wav")):
                    make_tones(self.cfg)
                self.available["tones"] = True
            except Exception:
                pass

        # NOTE the voice is NOT built here. It is built on the worker thread,
        # in _run(). See _make_engine() for why that is load-bearing.
        if self.cfg.mode in ("speech", "both"):
            try:
                import comtypes.client                            # noqa: F401
                self.available["speech"] = True
            except Exception:
                pass

        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def _make_engine(self):
        """Build the Windows voice (SAPI 5). MUST be called on the worker thread.

        SAPI is COM: a voice belongs to the apartment of the thread that made
        it, so COM is initialised, and the voice made, on the one thread that
        then speaks with it.

        DIRECTLY, not through pyttsx3. Measured 2026-09-23 at volume 0, three
        lines in a row on one engine, built and driven on one thread exactly as
        this class used it:

            pyttsx3 say() + runAndWait()    5.63 s   0.10 s   0.09 s
            SAPI SpVoice.Speak()            7.08 s   7.48 s   6.88 s

        A three-second line does not finish in a tenth of a second. Through
        pyttsx3 only the FIRST line of a session was ever spoken; every one
        after it returned at once, silently, and raised nothing - so nothing
        counted a failure either. The fix before this one (building the engine
        on this thread) was measured by the run-loop flag coming clear, which
        it did; nobody measured whether sound came out. Speak() here is
        synchronous and returns when the line has actually been said.
        """
        import comtypes
        import comtypes.client
        comtypes.CoInitialize()
        voice = comtypes.client.CreateObject("SAPI.SpVoice")
        voice.Volume = int(round(max(0.0, min(1.0, float(self.cfg.volume))) * 100))
        voice.Rate = _sapi_rate(self.cfg.rate)
        if self.cfg.voice:
            want = self.cfg.voice.lower()
            tokens = voice.GetVoices()
            for i in range(tokens.Count):
                tok = tokens.Item(i)
                if want in (tok.GetDescription() or "").lower():
                    voice.Voice = tok
                    break
        return voice

    # -- public ---------------------------------------------------------

    def event(self, name: str, **fields):
        """Raise an event. Returns immediately; audio happens on the worker."""
        if self.cfg.mode == "off":
            return
        self._refresh_phrases()
        # A lookup warning whose every flag is switched off is no warning:
        # nothing is played, tone included.
        if name == "flagged" and fields.get("flags") and not live_flags(
                fields["flags"], self.cfg.muted):
            return
        try:
            self._q.put_nowait((name, fields, time.monotonic()))
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
        self._refresh_phrases()
        flagged = [c for c in contacts if self._alerting(c)]

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

    def _alerting(self, c: dict) -> bool:
        """Does this contact still warrant a warning, with the phrases that
        are switched off taken away? `said` (structured) decides when present;
        otherwise the older `reason` string does."""
        muted = self.cfg.muted
        if c.get("flags") and live_flags(c["flags"], muted):
            return True
        if c.get("said"):
            return pick_reason(c["said"], muted) is not None
        return bool(c.get("reason"))

    def say(self, text: str):
        """Speak a line as it is - the settings page's "hear it" button."""
        self.event("_text", text=text)

    def close(self):
        self._q.put((None, None, 0.0))

    @staticmethod
    def _mtime() -> float:
        try:
            return CONFIG_PATH.stat().st_mtime
        except OSError:
            return 0.0

    def _refresh_phrases(self):
        m = self._mtime()
        if m != self._phrases_mtime:
            self._phrases_mtime = m
            try:
                fresh = AudioConfig.load()
                self.cfg.speech, self.cfg.muted = fresh.speech, fresh.muted
            except Exception:
                pass            # a half-written file: keep what we had

    # -- worker ---------------------------------------------------------

    def _run(self):
        if self.available["speech"]:
            try:
                self._engine = self._make_engine()
            except Exception as exc:
                self.available["speech"] = False
                self.last_error = f"engine init: {type(exc).__name__}: {exc}"
        while True:
            name, fields, queued = self._q.get()
            if name is None:
                return
            if name != "_text" and time.monotonic() - queued > MAX_ALERT_AGE:
                self.expired += 1
                continue
            try:
                self._play(name, fields or {})
            except Exception as exc:
                # Recorded rather than swallowed. This except used to be a
                # bare `pass`, which is how speech could die completely and
                # the only symptom anyone had was "audio broke".
                self.failures += 1
                self.last_error = f"{name}: {type(exc).__name__}: {exc}"

    def _speak(self, text: str):
        """Say one line and return when it has been said. A failure is
        counted and the voice rebuilt once, so one bad utterance cannot end
        speech for the session.

        As PLAIN TEXT (SVSFIsNotXML). By default SAPI reads a line that starts
        with "<" as markup, and some of what is spoken comes from outside - a
        shared report's author names themselves in a file somebody sent you.
        `<silence msec="999999"/>` as a name would otherwise mute the voice.
        """
        voice = self._engine
        if voice is None:
            return
        try:
            voice.Speak(text, _SVSF_SYNC_PLAIN)
        except Exception as exc:
            self.failures += 1
            self.last_error = f"speak: {type(exc).__name__}: {exc}"
            try:
                self._engine = self._make_engine()
                self.repairs += 1
            except Exception:
                self._engine = None
                self.available["speech"] = False

    def _play(self, name: str, fields: dict):
        tone_only = fields.pop("tone_only", False)
        if name == "_text":
            if self.available["speech"] and fields.get("text"):
                self._speak(str(fields["text"]))
            return
        self._refresh_phrases()

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

        if name in self.cfg.muted:
            return                      # the line is off: its tone, no words
        template = self.cfg.speech.get(name, "")
        if not template:
            return
        text = self._render(template, fields)
        if text:
            self._speak(text)

    def _render(self, template: str, fields: dict) -> str:
        return render_line(template, fields, self.cfg.speech,
                           self.cfg.speak_handles, self.cfg.muted)


def render_line(template: str, fields: dict, speech: dict,
                speak_handles: bool = True, muted=()) -> str:
    """One event's utterance. `said` (a structured reason) is preferred over
    `reason` (a console line, kept for callers that send nothing better)."""
    handle = fields.get("handle", "")
    if not speak_handles:
        handle = "contact"
    rng, dist = distance_words(fields.get("range_km"))
    vals = {
        "handle": _speakable(handle),
        "range": rng, "distance": dist,
        "flags": flag_words(fields.get("flags") or [], speech, muted),
        "reason": reason_words(fields.get("said") or fields.get("reason") or "",
                               speech, muted),
        "count": fields.get("count", ""),
        "flagged": fields.get("flagged", ""),
    }
    return render(template, vals)


# What the "hear it" button plays for each phrase: the whole line it would be
# part of, with made-up names, so a reason or flag is heard in context. MADE
# UP: every one of these lines is an accusation, and it used to be spoken
# about a real player's handle.
_SAMPLE = {"handle": "SAMPLE_PILOT", "range_km": 2.4}
SAMPLES = {
    "flagged": dict(_SAMPLE, flags=["PIRACY: EXMPLORG (40 members)", "new account (12d)"]),
    "known": dict(_SAMPLE, said={"kind": "report", "label": "Unprovoked attack", "count": 1}),
    "summary": {"count": 6, "flagged": 2},
    "nothing": {},
    "reason_report": dict(_SAMPLE, said={"kind": "report", "label": "Unprovoked attack", "count": 1}),
    "reason_reports": dict(_SAMPLE, said={"kind": "report", "label": "Unprovoked attack", "count": 3}),
    "reason_shared": dict(_SAMPLE, said={"kind": "shared", "label": "Camping a location", "source": "Orgmate"}),
    "reason_org": dict(_SAMPLE, said={"kind": "org", "org": "Example Org"}),
    "reason_former": dict(_SAMPLE, said={"kind": "former", "org": "Example Org"}),
    "reason_watchlist": dict(_SAMPLE, said={"kind": "watchlist"}),
    "flag_piracy": dict(_SAMPLE, flags=["PIRACY: EXMPLORG (40 members)"]),
    "flag_syndicate": dict(_SAMPLE, flags=["Syndicate: EXMPLORG"]),
    "flag_smuggling": dict(_SAMPLE, flags=["Smuggling: EXMPLORG"]),
    "flag_infiltration": dict(_SAMPLE, flags=["Infiltration: EXMPLORG"]),
    "flag_new_account": dict(_SAMPLE, flags=["new account (12d)"]),
}


def sample(key: str, text: str, speech: dict, speak_handles: bool = True) -> str:
    """How `text` would sound as phrase `key`, in the line it belongs to."""
    trial = dict(speech)
    trial[key] = text
    group = next((g for k, g, *_ in PHRASES if k == key), "line")
    line = {"reason": "known", "flag": "flagged"}.get(group, key)
    return render_line(trial.get(line, ""), dict(SAMPLES.get(key, {})), trial,
                       speak_handles)


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
        # Invented names only: these lines accuse whoever they name.
        ("contact", {"handle": "EXMPL_CONTACT", "range_km": 4.0, "tone_only": True}),
        ("flagged", {"handle": "SAMPLE_PILOT", "range_km": 2.4,
                     "flags": ["Infiltration: EXMPLORG"]}),
        ("known", {"handle": "OTHER_PILOT", "range_km": 2.8,
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
