"""Capture the game's picture correctly when Windows HDR is on.

With Windows HDR on, the screen copy every capture tool gets (dxcam's
included) treats 80 nits as white: anything brighter - most of a bright scene,
and the HUD text, whose faint glow turns into a halo - arrives as flat white.
That is how sc-watch's frames looked with Windows HDR on, and it is why NVIDIA
RTX HDR (which needs Windows HDR on) made IRIS misread a quarter of the plates
(engineering README, "RTX HDR and the float capture").

Windows will hand over the real picture instead: the same desktop duplication,
asked for R16G16B16A16_FLOAT, gives scRGB - linear light, 1.0 = 80 nits,
nothing clipped. For a game drawing an SDR picture (Star Citizen with its own
HDR option off, RTX HDR on or not) the desktop holds that picture at the "SDR
white" level, so

    8-bit = sRGB(clip(scRGB / (SDR white / 80), 0, 1))

IS the game's own image - measured: the brightest pixel was exactly SDR white,
and the converted plates were crisp. Done through a 65,536-entry table over
the float16 bit patterns, so a frame is one table read per value (~35 ms at
5120x1440 on the development PC, identical to the formula).

Built on dxcam's own pieces (sc-watch's capture library, pinned in
requirements.txt): its adapter and output order - so --monitor N is the same
screen it always was - its staging surface, and its duplicator, which only
ever asks Windows for 8-bit frames and is taught here to ask for float ones
first. If Windows answers with 8-bit frames anyway (HDR off, or a driver that
refuses), they are passed through unchanged, as dxcam would. A process can
duplicate an output only once, so this REPLACES dxcam's capture rather than
running beside it.
"""

from __future__ import annotations

import ctypes
import time
from functools import lru_cache

import numpy as np

FP16 = 10                      # DXGI_FORMAT_R16G16B16A16_FLOAT
BGRA8 = 87                     # DXGI_FORMAT_B8G8R8A8_UNORM
WHITE_RECHECK_S = 5.0          # how often the SDR white level is read again
REOPEN_EVERY_S = 1.0           # after a lost duplication, how often to retry


class HdrUnavailable(RuntimeError):
    """The HDR capture could not be set up; use the ordinary capture."""


# ---- the conversion -----------------------------------------------------------

def _srgb(x: np.ndarray) -> np.ndarray:
    return np.where(x <= 0.0031308, 12.92 * x, 1.055 * np.power(x, 1 / 2.4) - 0.055)


@lru_cache(maxsize=8)
def lut_for(sdr_white_nits: float) -> np.ndarray:
    """uint8 output for every float16 bit pattern: SDR white -> 255, black -> 0,
    brighter than white -> 255, negative / NaN -> 0."""
    white = max(float(sdr_white_nits), 1.0) / 80.0
    vals = np.arange(65536, dtype=np.uint16).view(np.float16).astype(np.float64)
    vals = np.nan_to_num(vals, nan=0.0, posinf=1e9, neginf=0.0)
    lut = np.round(255 * _srgb(np.clip(vals / white, 0.0, 1.0))).astype(np.uint8)
    lut.setflags(write=False)
    return lut


def to_bgr(rgba16: np.ndarray, lut: np.ndarray) -> np.ndarray:
    """(H, W, 4) RGBA float16 - given as its uint16 bit patterns - to a new,
    contiguous (H, W, 3) BGR uint8 image. Plain indexing, not np.take: on
    this channel-reversed view take measured 83 ms a frame, indexing 35."""
    return lut[rgba16[..., 2::-1]]


def wanted(setting: str, state) -> bool:
    """Use the HDR capture? Only when asked ('auto') and Windows says HDR is
    on for the display; unknown means no."""
    return setting == "auto" and state is not None and bool(getattr(state, "on", False))


# ---- finding the output, the way dxcam does --------------------------------------

def _output(monitor: int):
    """(Device, Output) for a 1-based monitor index, in dxcam.create()'s
    order: the first adapter that has outputs, then its outputs in order."""
    from dxcam.core.device import Device
    from dxcam.core.output import Output
    from dxcam.util.io import enum_dxgi_adapters

    for p_adapter in enum_dxgi_adapters():
        device = Device(p_adapter)
        outs = device.enum_outputs()
        if outs:
            if not 1 <= monitor <= len(outs):
                raise HdrUnavailable(f"monitor {monitor} does not exist ({len(outs)} on this adapter)")
            return device, Output(outs[monitor - 1])
    raise HdrUnavailable("no display adapter with outputs")


def output_name(monitor: int) -> str | None:
    """Windows' name (\\\\.\\DISPLAY1) for a monitor index - without opening a
    duplication, which would block the capture opened after it."""
    try:
        return _output(monitor)[1].devicename
    except Exception:
        return None


# ---- the capture ------------------------------------------------------------------

def _duplicator_class():
    from dxcam._libs.dxgi import DXGI_OUTDUPL_FLAG_NONE, IDXGIOutput5
    from dxcam.core.dxgi_duplicator import DXGIDuplicator
    import comtypes

    class FloatDuplicator(DXGIDuplicator):
        """dxcam's duplicator, offering Windows float frames before 8-bit."""

        def _duplicate_output(self, output, device) -> None:
            try:
                o5 = output.output.QueryInterface(IDXGIOutput5)
            except comtypes.COMError as exc:
                raise HdrUnavailable("this Windows has no DuplicateOutput1") from exc
            formats = (ctypes.c_uint * 2)(FP16, BGRA8)
            o5.DuplicateOutput1(ctypes.cast(device.device, ctypes.c_void_p),
                                DXGI_OUTDUPL_FLAG_NONE, len(formats), formats,
                                ctypes.byref(self.duplicator))

    return FloatDuplicator


class HdrScreenSource:
    """Live capture of the HDR picture, converted to what the game drew.
    Same interface as watch.ScreenSource: grab() -> BGR uint8 or None."""

    replay = False
    exhausted = False
    hdr = True

    def __init__(self, monitor: int, sdr_white_nits: float):
        from dxcam.core.stagesurf import StageSurface
        self._StageSurface = StageSurface
        self._Dup = _duplicator_class()
        self.monitor = monitor
        self._device, self._output = _output(monitor)
        if self._output.rotation_angle != 0:
            raise HdrUnavailable("rotated displays are not supported by the HDR capture")
        self.name = self._output.devicename
        self.sdr_white_nits = float(sdr_white_nits)
        self._lut = lut_for(self.sdr_white_nits)
        self._white_checked = time.monotonic()
        self._dup = self._stage = self._buf = None
        self._reopen_at = 0.0
        self.format = None                     # what Windows actually delivers
        self.frames = 0
        self.convert_s = 0.0
        try:
            self._dup = self._Dup(output=self._output, device=self._device)
            first = None
            deadline = time.monotonic() + 1.0
            while first is None and time.monotonic() < deadline:
                first = self.grab()
                if first is None:
                    time.sleep(0.02)
            if first is None:
                raise HdrUnavailable("no frame from the HDR capture within a second")
        except Exception:
            self.release()
            raise

    def display_name(self) -> str | None:
        return self.name

    def _refresh_white(self):
        now = time.monotonic()
        if now - self._white_checked < WHITE_RECHECK_S:
            return
        self._white_checked = now
        try:
            import display_hdr
            st = display_hdr.state_of(self.name)
        except Exception:
            return
        if st is not None and st.on and abs(st.sdr_white_nits - self.sdr_white_nits) > 0.5:
            self.sdr_white_nits = st.sdr_white_nits
            self._lut = lut_for(round(st.sdr_white_nits, 1))

    def _reopen(self):
        """After Windows took the duplication away (mode change, secure
        desktop, HDR toggled): drop it, and try again at most once a second."""
        now = time.monotonic()
        if self._dup is not None:
            try:
                self._dup.release()
            except Exception:
                pass
            self._dup = None
        if now < self._reopen_at:
            return
        self._reopen_at = now + REOPEN_EVERY_S
        try:
            self._dup = self._Dup(output=self._output, device=self._device)
        except Exception:
            self._dup = None

    def grab(self):
        if self._dup is None:
            self._reopen()
            if self._dup is None:
                return None
        self._refresh_white()
        try:
            ok = self._dup.update_frame()
        except Exception:                      # anything else Windows raises
            ok = False
        if not ok:
            self._reopen()
            return None
        if not self._dup.updated:
            return None
        from dxcam._libs.d3d11 import D3D11_TEXTURE2D_DESC
        try:
            tex = self._dup.texture
            desc = D3D11_TEXTURE2D_DESC()
            tex.GetDesc(ctypes.byref(desc))
            H, W, fmt = desc.Height, desc.Width, desc.Format
            st = self._stage
            if st is None or st.dxgi_format != fmt or (st.width, st.height) != (W, H):
                if st is not None:
                    st.release()
                self._stage = st = self._StageSurface(dxgi_format=fmt, output=self._output,
                                                      device=self._device)
            self._device.im_context.CopyResource(st.texture, tex)
        finally:
            self._dup.release_frame()
        self.format = fmt
        rect = st.map()
        try:
            addr = ctypes.cast(rect.pBits, ctypes.c_void_p).value
            if fmt == FP16:
                t = time.perf_counter()
                n = rect.Pitch // 2 * H
                raw = np.ctypeslib.as_array((ctypes.c_uint16 * n).from_address(addr))
                # One straight copy out of the mapped staging memory first:
                # the table lookup's scattered reads are far slower there
                # (78 ms a frame) than from ordinary memory. The buffer is
                # reused, so its pages are already in place.
                if self._buf is None or self._buf.size != n:
                    self._buf = np.empty(n, np.uint16)
                np.copyto(self._buf, raw)
                img = to_bgr(self._buf.reshape(H, rect.Pitch // 8, 4)[:, :W], self._lut)
                self.convert_s += time.perf_counter() - t
            else:                              # 8-bit after all: as dxcam gives it
                n = rect.Pitch * H
                raw = np.ctypeslib.as_array((ctypes.c_uint8 * n).from_address(addr))
                img = np.ascontiguousarray(raw.reshape(H, rect.Pitch // 4, 4)[:, :W, :3])
        finally:
            st.unmap()
        self.frames += 1
        return img

    @property
    def converting(self) -> bool:
        """True while Windows hands over float frames and they are converted."""
        return self.format == FP16

    def describe(self) -> str:
        if self.format == FP16:
            return f"HDR (float, converted at SDR white {self.sdr_white_nits:.0f} nits)"
        return "HDR capture asked for, but Windows gave 8-bit frames"

    def release(self):
        if self._stage is not None:
            try:
                self._stage.release()
            except Exception:
                pass
            self._stage = None
        if self._dup is not None:
            try:
                self._dup.release()
            except Exception:
                pass
            self._dup = None
