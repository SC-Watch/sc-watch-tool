"""Is Windows HDR on for the display we capture? Read-only.

Measured 2026-11-09 (engineering README, "Capturing the HDR signal itself"):
with Windows HDR on, the 8-bit copy of the desktop that capture tools get -
dxcam, so sc-watch - is sRGB(clip(scRGB, 0, 1)): 80 nits is white, whatever
the "SDR content brightness" setting. Windows SHOWS an SDR game at that
setting (360 nits here) but hands capture a copy that clips everything above
~22% of it. Sunlit snow at ~285 nits on screen arrived as flat white over 80%
of the frame; with Windows HDR off the same place was 0.1% white. Star
Citizen's own HDR option is a second, separate problem - it makes HUD text
bloom - and the game's settings file says whether it is on.

So the watcher checks the display it captures and says so at startup, rather
than letting a user wonder why nothing is read.

    state_of(r"\\\\.\\DISPLAY1") -> DisplayHdr(supported, on, bits, sdr_white_nits) | None
"""

from __future__ import annotations

import ctypes
import sys
from dataclasses import dataclass


@dataclass(frozen=True)
class DisplayHdr:
    device: str
    supported: bool
    on: bool
    bits: int
    sdr_white_nits: float


def _api():
    import ctypes.wintypes as wt

    class LUID(ctypes.Structure):
        _fields_ = [("LowPart", wt.DWORD), ("HighPart", wt.LONG)]

    class RATIONAL(ctypes.Structure):
        _fields_ = [("Numerator", ctypes.c_uint32), ("Denominator", ctypes.c_uint32)]

    class SRC(ctypes.Structure):
        _fields_ = [("adapterId", LUID), ("id", ctypes.c_uint32),
                    ("modeInfoIdx", ctypes.c_uint32), ("statusFlags", ctypes.c_uint32)]

    class TGT(ctypes.Structure):
        _fields_ = [("adapterId", LUID), ("id", ctypes.c_uint32), ("modeInfoIdx", ctypes.c_uint32),
                    ("outputTechnology", ctypes.c_uint32), ("rotation", ctypes.c_uint32),
                    ("scaling", ctypes.c_uint32), ("refreshRate", RATIONAL),
                    ("scanLineOrdering", ctypes.c_uint32), ("targetAvailable", wt.BOOL),
                    ("statusFlags", ctypes.c_uint32)]

    class PATH(ctypes.Structure):
        _fields_ = [("sourceInfo", SRC), ("targetInfo", TGT), ("flags", ctypes.c_uint32)]

    class MODE(ctypes.Structure):
        _fields_ = [("infoType", ctypes.c_uint32), ("id", ctypes.c_uint32),
                    ("adapterId", LUID), ("data", ctypes.c_byte * 48)]

    class HEADER(ctypes.Structure):
        _fields_ = [("type", ctypes.c_uint32), ("size", ctypes.c_uint32),
                    ("adapterId", LUID), ("id", ctypes.c_uint32)]

    class SDRWHITE(ctypes.Structure):
        _fields_ = [("header", HEADER), ("level", ctypes.c_uint32)]

    class ADVCOLOR(ctypes.Structure):
        _fields_ = [("header", HEADER), ("value", ctypes.c_uint32),
                    ("encoding", ctypes.c_uint32), ("bits", ctypes.c_uint32)]

    class SRCNAME(ctypes.Structure):
        _fields_ = [("header", HEADER), ("name", ctypes.c_wchar * 32)]

    return PATH, MODE, SDRWHITE, ADVCOLOR, SRCNAME


def all_displays() -> list[DisplayHdr]:
    """Every active display's HDR state. Empty off Windows or on any error."""
    if sys.platform != "win32":
        return []
    try:
        PATH, MODE, SDRWHITE, ADVCOLOR, SRCNAME = _api()
        u = ctypes.windll.user32
        n_p, n_m = ctypes.c_uint32(), ctypes.c_uint32()
        if u.GetDisplayConfigBufferSizes(2, ctypes.byref(n_p), ctypes.byref(n_m)) != 0:
            return []
        paths, modes = (PATH * n_p.value)(), (MODE * n_m.value)()
        if u.QueryDisplayConfig(2, ctypes.byref(n_p), paths, ctypes.byref(n_m), modes, None) != 0:
            return []
        out = []
        for p in paths[:n_p.value]:
            name = SRCNAME()
            name.header.type, name.header.size = 1, ctypes.sizeof(SRCNAME)
            name.header.adapterId, name.header.id = p.sourceInfo.adapterId, p.sourceInfo.id
            adv = ADVCOLOR()
            adv.header.type, adv.header.size = 9, ctypes.sizeof(ADVCOLOR)
            adv.header.adapterId, adv.header.id = p.targetInfo.adapterId, p.targetInfo.id
            white = SDRWHITE()
            white.header.type, white.header.size = 11, ctypes.sizeof(SDRWHITE)
            white.header.adapterId, white.header.id = p.targetInfo.adapterId, p.targetInfo.id
            if (u.DisplayConfigGetDeviceInfo(ctypes.byref(name)) != 0
                    or u.DisplayConfigGetDeviceInfo(ctypes.byref(adv)) != 0):
                continue
            nits = white.level / 1000 * 80 if u.DisplayConfigGetDeviceInfo(ctypes.byref(white)) == 0 else 0.0
            out.append(DisplayHdr(name.name, bool(adv.value & 1), bool(adv.value & 2),
                                  int(adv.bits), nits))
        return out
    except Exception:
        return []


def state_of(device: str | None) -> DisplayHdr | None:
    """The HDR state of one display, by its Windows name (\\\\.\\DISPLAY1).
    None when it cannot be told - never an exception."""
    if not device:
        return None
    for d in all_displays():
        if d.device.lower() == device.lower():
            return d
    return None


def warning(state: DisplayHdr | None) -> list[str]:
    """Startup lines for a display with Windows HDR on; [] otherwise."""
    if state is None or not state.on:
        return []
    return [
        f"  ! Windows HDR is ON for {state.device} - the screen sc-watch captures.",
        "    With HDR on, the copy of the screen Windows hands to capture tools",
        "    treats 80 nits as white, so most of the picture arrives blown out",
        "    and labels on anything bright are missed. Turn HDR off for this",
        "    display while sc-watch runs: Windows Settings > System > Display >",
        "    Use HDR. Star Citizen's own HDR option should be off as well - it",
        "    makes HUD text bloom.",
    ]


if __name__ == "__main__":
    for d in all_displays():
        print(f"{d.device}: HDR supported {d.supported}, HDR on {d.on}, "
              f"{d.bits} bits/channel, SDR white {d.sdr_white_nits:.0f} nits")
