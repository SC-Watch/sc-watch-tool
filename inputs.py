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
Bindings: keyboard keys, joystick buttons, and combinations of both.

    python inputs.py --list        what is plugged in
    python inputs.py --listen      press something; it tells you what to bind

Written because a HOTAS setup has no spare keyboard hand. A binding is a set of
inputs that must all be held at once, so `rctrl+joy0.b24` means exactly what it
looks like.

WHY POLLING, STILL
------------------
The keyboard half keeps the property the original KeyWatcher was built around:
GetAsyncKeyState installs nothing, intercepts nothing and sends nothing - it
reads a flag Windows already maintains. The joystick half is the same shape:
joyGetPosEx asks the driver for the current state of a device. Neither hooks
input, neither registers a hotkey, and neither swallows the press - so the
button still reaches the game, which matters when it is also bound to something
in flight.

WHY winmm AND NOT DirectInput/pygame
------------------------------------
Measured on this machine: joyGetDevCapsW enumerates all four sticks and reports
32 buttons on three of them, which covers every button a VKB Gladiator exposes.
DirectInput through pygame would add a 30 MB dependency to read the same
numbers. The legacy API's real limits are 32 buttons and 6 axes per device and
16 devices total; a stick with more than 32 buttons would need the newer API,
and `capabilities()` says so rather than silently truncating.

IDENTITY
--------
Device index (joy0, joy1) is what the driver hands out and can change when you
replug. VID/PID is stable, so a binding can name either:

    joy0.b24              index 0, button 24
    vid231d:0126.b7       whichever index that device currently has
    rctrl+vid231d:0126.b7 both held together

Every connected stick here reports the same generic name ("HID-compliant game
controller"), so names are useless for telling four VKB devices apart - which
is why `--listen` exists. Press the thing you want and it prints the binding.

Buttons are numbered from 1, matching how joystick test dialogs and game
binding screens label them.
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes as w
import time
from dataclasses import dataclass

# ---------------------------------------------------------------- keyboard --

VK_CODES = {
    "TAB": 0x09, "SPACE": 0x20, "ENTER": 0x0D, "ESC": 0x1B,
    "SHIFT": 0x10, "CTRL": 0x11, "ALT": 0x12,
    "LSHIFT": 0xA0, "RSHIFT": 0xA1, "LCTRL": 0xA2, "RCTRL": 0xA3,
    "LALT": 0xA4, "RALT": 0xA5,
    "CAPSLOCK": 0x14, "BACKSPACE": 0x08, "INSERT": 0x2D, "DELETE": 0x2E,
    "HOME": 0x24, "END": 0x23, "PAGEUP": 0x21, "PAGEDOWN": 0x22,
    "UP": 0x26, "DOWN": 0x28, "LEFT": 0x25, "RIGHT": 0x27,
    "NUMLOCK": 0x90, "SCROLLLOCK": 0x91, "PAUSE": 0x13,
    "MINUS": 0xBD, "EQUALS": 0xBB, "LBRACKET": 0xDB, "RBRACKET": 0xDD,
    "SEMICOLON": 0xBA, "QUOTE": 0xDE, "COMMA": 0xBC, "PERIOD": 0xBE,
    "SLASH": 0xBF, "BACKSLASH": 0xDC, "GRAVE": 0xC0,
    **{f"NUMPAD{i}": 0x60 + i for i in range(10)},
    **{f"F{i}": 0x6F + i for i in range(1, 25)},
    **{c: ord(c) for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"},
}
VK_NAMES = {v: k for k, v in reversed(list(VK_CODES.items()))}

_user32 = ctypes.WinDLL("user32", use_last_error=True)
_user32.GetAsyncKeyState.argtypes = [ctypes.c_int]
_user32.GetAsyncKeyState.restype = ctypes.c_short


def key_down(vk: int) -> bool:
    """Is this key held right now? (0x8000 = currently down.)"""
    return bool(_user32.GetAsyncKeyState(vk) & 0x8000)


# ---------------------------------------------------------------- joystick --

_winmm = ctypes.WinDLL("winmm")
JOYERR_NOERROR = 0
_JOY_RETURNBUTTONS = 0x00000080
_JOY_RETURNall = 0x000000FF


class JOYCAPS(ctypes.Structure):
    _fields_ = [
        ("wMid", w.WORD), ("wPid", w.WORD), ("szPname", ctypes.c_wchar * 32),
        ("wXmin", w.UINT), ("wXmax", w.UINT), ("wYmin", w.UINT),
        ("wYmax", w.UINT), ("wZmin", w.UINT), ("wZmax", w.UINT),
        ("wNumButtons", w.UINT), ("wPeriodMin", w.UINT), ("wPeriodMax", w.UINT),
        ("wRmin", w.UINT), ("wRmax", w.UINT), ("wUmin", w.UINT),
        ("wUmax", w.UINT), ("wVmin", w.UINT), ("wVmax", w.UINT),
        ("wCaps", w.UINT), ("wMaxAxes", w.UINT), ("wNumAxes", w.UINT),
        ("wMaxButtons", w.UINT), ("szRegKey", ctypes.c_wchar * 32),
        ("szOEMVxD", ctypes.c_wchar * 260),
    ]


class JOYINFOEX(ctypes.Structure):
    _fields_ = [
        ("dwSize", w.DWORD), ("dwFlags", w.DWORD),
        ("dwXpos", w.DWORD), ("dwYpos", w.DWORD), ("dwZpos", w.DWORD),
        ("dwRpos", w.DWORD), ("dwUpos", w.DWORD), ("dwVpos", w.DWORD),
        ("dwButtons", w.DWORD), ("dwButtonNumber", w.DWORD),
        ("dwPOV", w.DWORD), ("dwReserved1", w.DWORD), ("dwReserved2", w.DWORD),
    ]


@dataclass(frozen=True)
class Device:
    index: int
    vid: int
    pid: int
    name: str
    buttons: int
    axes: int
    truncated: bool = False       # device has more buttons than winmm reports

    @property
    def ident(self) -> str:
        return f"vid{self.vid:04x}:{self.pid:04x}"

    def describe(self) -> str:
        extra = "  (more than 32 buttons - only the first 32 are readable)" \
                if self.truncated else ""
        return (f"joy{self.index}  {self.ident}  {self.name}  "
                f"{self.buttons} buttons, {self.axes} axes{extra}")


def devices() -> list[Device]:
    """Every joystick the driver can see, in index order."""
    out = []
    for i in range(16):
        caps = JOYCAPS()
        if _winmm.joyGetDevCapsW(i, ctypes.byref(caps),
                                 ctypes.sizeof(caps)) != JOYERR_NOERROR:
            continue
        # A device present in the table is not necessarily attached; poll it.
        info = JOYINFOEX()
        info.dwSize = ctypes.sizeof(info)
        info.dwFlags = _JOY_RETURNall
        if _winmm.joyGetPosEx(i, ctypes.byref(info)) != JOYERR_NOERROR:
            continue
        out.append(Device(index=i, vid=caps.wMid, pid=caps.wPid,
                          name=caps.szPname, buttons=caps.wNumButtons,
                          axes=caps.wNumAxes,
                          truncated=caps.wMaxButtons > 32))
    return out


def buttons_down(index: int) -> int:
    """Bitmask of held buttons, or 0 if the device is gone.

    A stick can be unplugged mid-session. That is not an error worth stopping
    for - it is a binding that stops firing until it comes back.
    """
    info = JOYINFOEX()
    info.dwSize = ctypes.sizeof(info)
    info.dwFlags = _JOY_RETURNBUTTONS
    if _winmm.joyGetPosEx(index, ctypes.byref(info)) != JOYERR_NOERROR:
        return 0
    return int(info.dwButtons)


# ----------------------------------------------------------------- binding --

class BindError(ValueError):
    pass


@dataclass(frozen=True)
class Token:
    """One input that must be held. Either a key or a joystick button."""
    kind: str          # "key" | "joy"
    vk: int = 0
    index: int | None = None    # joystick index, when bound by index
    ident: str = ""             # "vid231d:0126", when bound by VID/PID
    button: int = 0             # 1-based

    def resolve(self, devs: list[Device]) -> int | None:
        """Which joystick index this token means right now, if any."""
        if self.kind != "joy":
            return None
        if self.ident:
            for d in devs:
                if d.ident == self.ident:
                    return d.index
            return None
        return self.index

    def label(self) -> str:
        if self.kind == "key":
            return VK_NAMES.get(self.vk, f"vk{self.vk:02x}").lower()
        who = self.ident or f"joy{self.index}"
        return f"{who}.b{self.button}"


def parse(spec: str) -> list[Token]:
    """Turn 'rctrl+joy0.b24' into tokens. Raises BindError on nonsense.

    Accepted parts:
        A, F9, RCTRL, NUMPAD3      a keyboard key
        joy0.b24                   joystick by driver index
        vid231d:0126.b7            joystick by VID/PID, survives replugging
    """
    spec = (spec or "").strip()
    if not spec or spec.lower() == "off":
        return []
    tokens = []
    for part in spec.split("+"):
        p = part.strip()
        if not p:
            raise BindError(f"empty part in {spec!r}")
        low = p.lower()
        if "." in low and (low.startswith("joy") or low.startswith("vid")):
            who, _, btn = low.partition(".")
            if not btn.startswith("b") or not btn[1:].isdigit():
                raise BindError(f"expected .b<number> in {p!r}")
            n = int(btn[1:])
            if not 1 <= n <= 32:
                raise BindError(f"button {n} out of range 1-32 in {p!r}")
            if low.startswith("joy"):
                if not who[3:].isdigit():
                    raise BindError(f"expected joy<number> in {p!r}")
                tokens.append(Token("joy", index=int(who[3:]), button=n))
            else:
                # vid231d:0126 -> "vid" + 4 hex + ":" + 4 hex = 12 chars,
                # colon at index 7.
                body = who[3:]
                vid, _, pid = body.partition(":")
                if len(vid) != 4 or len(pid) != 4:
                    raise BindError(f"expected vidXXXX:YYYY in {p!r}")
                try:
                    int(vid, 16), int(pid, 16)
                except ValueError:
                    raise BindError(f"vid/pid must be hex in {p!r}") from None
                tokens.append(Token("joy", ident=who, button=n))
        else:
            vk = VK_CODES.get(p.upper())
            if vk is None:
                raise BindError(f"unknown key {p!r}")
            tokens.append(Token("key", vk=vk))
    if not tokens:
        raise BindError("nothing to bind")
    return tokens


class Binding:
    """A binding, polled. Fires once per press, not once per poll.

    Edge detection is done here rather than leaning on GetAsyncKeyState's
    "pressed since last call" bit, because that bit exists only for keys and a
    combination has to agree about when it became true. The rule is the same
    either way: the binding fires on the transition from not-all-held to
    all-held.
    """

    def __init__(self, spec: str):
        self.spec = spec
        self.tokens = parse(spec)
        self._was_down = False
        self._devs = devices()
        self._devs_checked = time.time()

    def __bool__(self) -> bool:
        return bool(self.tokens)

    @property
    def missing(self) -> list[str]:
        """Joystick parts of this binding that are not currently plugged in."""
        return [t.label() for t in self.tokens
                if t.kind == "joy" and t.resolve(self._devs) is None]

    def _refresh_devices(self):
        # Re-enumerating is ~0.1 ms per device but pointless every poll. Once a
        # second is fast enough to notice a stick being plugged back in.
        if time.time() - self._devs_checked > 1.0:
            self._devs = devices()
            self._devs_checked = time.time()

    def held(self) -> bool:
        """Are all parts held right now?"""
        if not self.tokens:
            return False
        self._refresh_devices()
        masks: dict[int, int] = {}
        for t in self.tokens:
            if t.kind == "key":
                if not key_down(t.vk):
                    return False
            else:
                idx = t.resolve(self._devs)
                if idx is None:
                    return False
                if idx not in masks:
                    masks[idx] = buttons_down(idx)
                if not masks[idx] & (1 << (t.button - 1)):
                    return False
        return True

    def partial(self) -> tuple[int, int, list[str]]:
        """How much of this binding is held: (held, total, missing labels).

        A combination fails silently - nothing happens, and nothing says why.
        The watcher shows this on its idle line so a binding that is three
        quarters satisfied looks different from one nothing is touching.
        """
        self._refresh_devices()
        masks: dict[int, int] = {}
        held, missing = 0, []
        for t in self.tokens:
            if t.kind == "key":
                on = key_down(t.vk)
            else:
                idx = t.resolve(self._devs)
                if idx is None:
                    on = False
                else:
                    if idx not in masks:
                        masks[idx] = buttons_down(idx)
                    on = bool(masks[idx] & (1 << (t.button - 1)))
            if on:
                held += 1
            else:
                missing.append(t.label())
        return held, len(self.tokens), missing

    def pressed(self) -> bool:
        """True once, on the transition into all-held."""
        now = self.held()
        fired = now and not self._was_down
        self._was_down = now
        return fired

    def drain(self):
        """Forget the current state, so a press held through a long read does
        not fire again the moment we start polling. Matches KeyWatcher.drain."""
        self._was_down = self.held()

    def label(self) -> str:
        return " + ".join(t.label() for t in self.tokens)


# -------------------------------------------------------------------- CLI ---

def _listen(seconds: float = 60.0):
    """Print whatever gets pressed, as a binding string you can paste."""
    devs = devices()
    print(f"listening for {seconds:.0f}s - press a key or a joystick button.")
    print("hold modifiers with it to see a combination. Ctrl+C to stop.\n")
    if not devs:
        print("  (no joysticks detected)\n")
    for d in devs:
        print(f"  {d.describe()}")
    print()

    prev_btn = {d.index: buttons_down(d.index) for d in devs}
    prev_keys: set[int] = set()
    end = time.time() + seconds
    MODS = {0xA0: "lshift", 0xA1: "rshift", 0xA2: "lctrl", 0xA3: "rctrl",
            0xA4: "lalt", 0xA5: "ralt"}
    try:
        while time.time() < end:
            mods = [n for vk, n in MODS.items() if key_down(vk)]
            pre = "+".join(mods) + "+" if mods else ""

            for d in devs:
                cur = buttons_down(d.index)
                new = cur & ~prev_btn.get(d.index, 0)
                prev_btn[d.index] = cur
                for b in range(32):
                    if new & (1 << b):
                        print(f"  {pre}joy{d.index}.b{b+1}"
                              f"        (stable: {pre}{d.ident}.b{b+1})")

            for name, vk in VK_CODES.items():
                if vk in MODS:
                    continue
                if key_down(vk):
                    if vk not in prev_keys:
                        prev_keys.add(vk)
                        print(f"  {pre}{name.lower()}")
                else:
                    prev_keys.discard(vk)

            time.sleep(0.02)
    except KeyboardInterrupt:
        pass
    print("\ndone.")


def main() -> int:
    import argparse
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--list", action="store_true", help="show connected devices")
    ap.add_argument("--listen", action="store_true",
                    help="print what you press, as a binding string")
    ap.add_argument("--seconds", type=float, default=60.0)
    ap.add_argument("--test", metavar="SPEC",
                    help="poll one binding and report when it fires")
    args = ap.parse_args()

    if args.list or not (args.listen or args.test):
        devs = devices()
        print(f"{len(devs)} joystick(s):")
        for d in devs:
            print(f"  {d.describe()}")
        if not devs:
            print("  none detected")
        print("\nkeyboard names: " + ", ".join(sorted(VK_CODES)[:14]) + ", ...")
    if args.listen:
        _listen(args.seconds)
    if args.test:
        try:
            b = Binding(args.test)
        except BindError as exc:
            print(f"bad binding: {exc}")
            return 2
        print(f"watching {b.label()} for {args.seconds:.0f}s"
              + (f"  [not plugged in: {', '.join(b.missing)}]" if b.missing else ""))
        print("hold the binding. A combination only fires when EVERY part is")
        print("held at the same moment, so the column that stays 'no' is the")
        print("one to look at.")
        print()
        header = "  " + "  ".join(f"{t.label():>16}" for t in b.tokens) + "   FIRES"
        print(header)
        print("  " + "-" * (len(header) - 2))
        end = time.time() + args.seconds
        n = 0
        last = None
        # Per-token high-water marks: a part that is NEVER true is a different
        # problem from one that is true but never at the same time as the rest.
        ever = {t.label(): False for t in b.tokens}
        try:
            while time.time() < end:
                devs = b._devs
                b._refresh_devices()
                states = []
                for t in b.tokens:
                    if t.kind == "key":
                        on = key_down(t.vk)
                    else:
                        idx = t.resolve(b._devs)
                        on = bool(idx is not None
                                  and buttons_down(idx) & (1 << (t.button - 1)))
                    ever[t.label()] |= on
                    states.append(on)
                fired = b.pressed()
                if fired:
                    n += 1
                row = "  " + "  ".join(f"{('YES' if s else 'no'):>16}"
                                       for s in states)
                row += f"   {'*** FIRED ***' if fired else ('held' if all(states) else '')}"
                if row != last or fired:
                    print(row)
                    last = row
                time.sleep(0.03)
        except KeyboardInterrupt:
            pass
        print()
        print(f"done - {n} press(es)")
        never = [k for k, v in ever.items() if not v]
        if never:
            print(f"NEVER saw: {', '.join(never)}")
            print("  Those parts never went true at all, so the combination "
                  "could not fire.")
            print("  If one is a key produced by joy-to-key software, check it "
                  "is HELD while")
            print("  the button is down rather than sent as a single tap.")
        elif n == 0:
            print("Every part went true at some point, but never all at once - "
                  "so the")
            print("  software output and the physical button are not "
                  "overlapping. Bind the")
            print("  physical button on its own, or lengthen the mapped key's "
                  "hold time.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
