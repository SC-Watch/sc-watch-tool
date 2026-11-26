//! The Windows desktop as a frame source: DXGI Desktop Duplication, as the
//! Python's dxcam (8-bit) and hdr_capture.py (float, converted) - the same
//! adapter and output order, so monitor N is the same screen, the same
//! zero-wait grab (nothing new -> no frame), the copy to a staging texture
//! released early. Plus display_hdr.py: whether Windows HDR is on for a
//! display, and its SDR white level.

use std::time::{Duration, Instant};

use scw_vision::Image;
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
    DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    IDXGIAdapter, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutput5,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::UI::HiDpi::{PROCESS_PER_MONITOR_DPI_AWARE, SetProcessDpiAwareness};
use windows::core::Interface;

use crate::hdr::{HdrCapture, bgra_to_bgr, lut_for, to_bgr};
use crate::{CaptureError, Source};

/// How often the SDR white level is read again while converting.
const WHITE_RECHECK: Duration = Duration::from_secs(5);
/// After a lost duplication, how often to try again.
const REOPEN_EVERY: Duration = Duration::from_secs(1);

fn sys(what: &str, e: windows::core::Error) -> CaptureError {
    CaptureError::System(format!("{what}: {e}"))
}

fn wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

// ---- displays and their HDR state -------------------------------------------------------

/// One active display's HDR state (display_hdr.DisplayHdr).
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayHdr {
    /// Windows' name for it, `\\.\DISPLAY1`.
    pub device: String,
    pub supported: bool,
    pub on: bool,
    pub bits: u32,
    /// "SDR content brightness", in nits; 0 when Windows does not say.
    pub sdr_white_nits: f64,
}

/// Every active display's HDR state; empty on any error.
pub fn all_displays() -> Vec<DisplayHdr> {
    unsafe {
        let (mut n_paths, mut n_modes) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes).is_err() {
            return Vec::new();
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_paths,
            paths.as_mut_ptr(),
            &mut n_modes,
            modes.as_mut_ptr(),
            None,
        )
        .is_err()
        {
            return Vec::new();
        }
        let mut out = Vec::new();
        for p in &paths[..n_paths as usize] {
            let header = |kind, size: usize, adapter, id| DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: kind,
                size: size as u32,
                adapterId: adapter,
                id,
            };
            let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
                    p.sourceInfo.adapterId,
                    p.sourceInfo.id,
                ),
                ..Default::default()
            };
            let mut adv = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
                header: header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                    size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(),
                    p.targetInfo.adapterId,
                    p.targetInfo.id,
                ),
                ..Default::default()
            };
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                    size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(),
                    p.targetInfo.adapterId,
                    p.targetInfo.id,
                ),
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut name.header) != 0
                || DisplayConfigGetDeviceInfo(&mut adv.header) != 0
            {
                continue;
            }
            let nits = if DisplayConfigGetDeviceInfo(&mut white.header) == 0 {
                white.SDRWhiteLevel as f64 / 1000.0 * 80.0
            } else {
                0.0
            };
            let flags = adv.Anonymous.value;
            out.push(DisplayHdr {
                device: wide(&name.viewGdiDeviceName),
                supported: flags & 1 != 0,
                on: flags & 2 != 0,
                bits: adv.bitsPerColorChannel,
                sdr_white_nits: nits,
            });
        }
        out
    }
}

/// One display's HDR state by its Windows name; None when it cannot be told.
pub fn state_of(device: &str) -> Option<DisplayHdr> {
    all_displays()
        .into_iter()
        .find(|d| d.device.eq_ignore_ascii_case(device))
}

// ---- outputs, in dxcam's order ------------------------------------------------------------

/// A monitor as the capture sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputInfo {
    /// 1-based, as `--monitor`.
    pub monitor: usize,
    pub device: String,
    pub width: i32,
    pub height: i32,
    pub rotated: bool,
    /// The monitor's top-left corner on the desktop.
    pub left: i32,
    pub top: i32,
}

/// The first adapter that has outputs, and its outputs in order -
/// dxcam.create()'s device 0.
fn first_adapter_outputs() -> Result<(IDXGIAdapter1, Vec<IDXGIOutput>), CaptureError> {
    unsafe {
        // As dxcam does before it reads an output: per-monitor DPI aware, so
        // the desktop coordinates are real pixels - and DuplicateOutput1, the
        // float capture, refuses any process that is not. (Fails harmlessly
        // when the process's awareness is already set.)
        let _ = SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| sys("CreateDXGIFactory1", e))?;
        let mut i = 0;
        loop {
            let adapter = match factory.EnumAdapters1(i) {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(sys("EnumAdapters1", e)),
            };
            let mut outputs = Vec::new();
            let mut j = 0;
            loop {
                match adapter.EnumOutputs(j) {
                    Ok(o) => outputs.push(o),
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(sys("EnumOutputs", e)),
                }
                j += 1;
            }
            if !outputs.is_empty() {
                return Ok((adapter, outputs));
            }
            i += 1;
        }
        Err(CaptureError::System(
            "no display adapter with outputs".into(),
        ))
    }
}

fn info_of(monitor: usize, output: &IDXGIOutput) -> Result<OutputInfo, CaptureError> {
    let d = unsafe { output.GetDesc() }.map_err(|e| sys("GetDesc", e))?;
    let r = d.DesktopCoordinates;
    Ok(OutputInfo {
        monitor,
        device: wide(&d.DeviceName),
        width: r.right - r.left,
        height: r.bottom - r.top,
        rotated: d.Rotation != DXGI_MODE_ROTATION_IDENTITY
            && d.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED,
        left: r.left,
        top: r.top,
    })
}

/// The monitors the capture can open, numbered as `--monitor`.
pub fn monitors() -> Result<Vec<OutputInfo>, CaptureError> {
    let (_, outputs) = first_adapter_outputs()?;
    outputs
        .iter()
        .enumerate()
        .map(|(i, o)| info_of(i + 1, o))
        .collect()
}

// ---- the capture ------------------------------------------------------------------------------

/// The live desktop of one monitor.
pub struct DesktopSource {
    info: OutputInfo,
    output: IDXGIOutput,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// Offer Windows float frames first.
    float: bool,
    dup: Option<IDXGIOutputDuplication>,
    /// The first frame of a new duplication is Windows' blank starting
    /// surface (black, and float whatever was asked for): it is passed over.
    primed: bool,
    staging: Option<(ID3D11Texture2D, D3D11_TEXTURE2D_DESC)>,
    reopen_at: Instant,
    sdr_white_nits: f64,
    lut: Option<Box<[u8; 65536]>>,
    white_checked: Instant,
    /// What Windows last delivered.
    format: Option<DXGI_FORMAT>,
    pub frames: u64,
    pub convert: Duration,
}

impl DesktopSource {
    /// Opens monitor `monitor` (1-based). With `hdr` Auto and Windows HDR on
    /// for that display, frames are taken as float and converted at its SDR
    /// white level; otherwise 8-bit, as dxcam.
    pub fn open(monitor: usize, hdr: HdrCapture) -> Result<Self, CaptureError> {
        let (adapter, outputs) = first_adapter_outputs()?;
        if !(1..=outputs.len()).contains(&monitor) {
            return Err(CaptureError::NoMonitor {
                asked: monitor,
                count: outputs.len(),
            });
        }
        let output = outputs[monitor - 1].clone();
        let info = info_of(monitor, &output)?;
        if info.rotated {
            return Err(CaptureError::Rotated);
        }
        let state = state_of(&info.device);
        let float = hdr.wanted(state.as_ref().map(|s| s.on));
        let (mut device, mut context) = (None, None);
        unsafe {
            let adapter: IDXGIAdapter = adapter.cast().map_err(|e| sys("adapter", e))?;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                Some(&[
                    D3D_FEATURE_LEVEL_11_0,
                    D3D_FEATURE_LEVEL_10_1,
                    D3D_FEATURE_LEVEL_10_0,
                ]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| sys("D3D11CreateDevice", e))?;
        }
        let sdr_white_nits = state.map_or(80.0, |s| s.sdr_white_nits);
        let mut src = Self {
            info,
            output,
            device: device.ok_or(CaptureError::System("no D3D11 device".into()))?,
            context: context.ok_or(CaptureError::System("no D3D11 context".into()))?,
            float,
            dup: None,
            primed: false,
            staging: None,
            reopen_at: Instant::now(),
            sdr_white_nits,
            lut: float.then(|| lut_for(sdr_white_nits)),
            white_checked: Instant::now(),
            format: None,
            frames: 0,
            convert: Duration::ZERO,
        };
        src.dup = Some(src.duplicate()?);
        // As hdr_capture.py: wait up to a second for the first frame (the
        // blank one, passed over), so the source is ready when it returns.
        let deadline = Instant::now() + Duration::from_secs(1);
        while !src.primed && Instant::now() < deadline {
            src.grab()?;
            if !src.primed {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if !src.primed {
            return Err(CaptureError::System(
                "no frame from the desktop capture within a second".into(),
            ));
        }
        Ok(src)
    }

    fn duplicate(&self) -> Result<IDXGIOutputDuplication, CaptureError> {
        unsafe {
            if self.float {
                let o5: IDXGIOutput5 = self
                    .output
                    .cast()
                    .map_err(|e| sys("this Windows has no DuplicateOutput1", e))?;
                let formats = [DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM];
                return o5
                    .DuplicateOutput1(&self.device, 0, &formats)
                    .map_err(|e| sys("DuplicateOutput1", e));
            }
            // dxcam: DuplicateOutput1 with 8-bit only, falling back to the old call.
            if let Ok(o5) = self.output.cast::<IDXGIOutput5>()
                && let Ok(d) = o5.DuplicateOutput1(&self.device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM])
            {
                return Ok(d);
            }
            let o1: IDXGIOutput1 = self.output.cast().map_err(|e| sys("IDXGIOutput1", e))?;
            o1.DuplicateOutput(&self.device)
                .map_err(|e| sys("DuplicateOutput", e))
        }
    }

    /// Converts float frames at this SDR white level instead of the one
    /// Windows reports (until Windows reports a change).
    pub fn set_sdr_white(&mut self, nits: f64) {
        self.sdr_white_nits = nits;
        self.lut = Some(lut_for(nits));
    }

    pub fn info(&self) -> &OutputInfo {
        &self.info
    }

    /// True while Windows hands over float frames and they are converted.
    pub fn converting(&self) -> bool {
        self.format == Some(DXGI_FORMAT_R16G16B16A16_FLOAT)
    }

    /// After Windows took the duplication away (mode change, secure desktop,
    /// HDR toggled): try again, at most once a second.
    fn reopen(&mut self) {
        self.dup = None;
        let now = Instant::now();
        if now < self.reopen_at {
            return;
        }
        self.reopen_at = now + REOPEN_EVERY;
        self.dup = self.duplicate().ok();
        self.primed = false;
    }

    fn refresh_white(&mut self) {
        if !self.float || self.white_checked.elapsed() < WHITE_RECHECK {
            return;
        }
        self.white_checked = Instant::now();
        if let Some(st) = state_of(&self.info.device)
            && st.on
            && (st.sdr_white_nits - self.sdr_white_nits).abs() > 0.5
        {
            self.sdr_white_nits = st.sdr_white_nits;
            self.lut = Some(lut_for((st.sdr_white_nits * 10.0).round() / 10.0));
        }
    }

    fn staging_for(
        &mut self,
        desc: &D3D11_TEXTURE2D_DESC,
    ) -> Result<ID3D11Texture2D, CaptureError> {
        if let Some((tex, d)) = &self.staging
            && (d.Width, d.Height, d.Format) == (desc.Width, desc.Height, desc.Format)
        {
            return Ok(tex.clone());
        }
        let d = D3D11_TEXTURE2D_DESC {
            Width: desc.Width,
            Height: desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.Format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex = None;
        unsafe { self.device.CreateTexture2D(&d, None, Some(&mut tex)) }
            .map_err(|e| sys("CreateTexture2D", e))?;
        let tex = tex.ok_or(CaptureError::System("no staging texture".into()))?;
        self.staging = Some((tex.clone(), d));
        Ok(tex)
    }
}

impl DesktopSource {
    /// The next frame, whole or only `part` (x, y, width, height): the
    /// whole texture is copied on the GPU either way, but only the part is
    /// read back and converted - for the mining overlay's band, a few
    /// hundredths of the work of a 5120x1440 frame (an HDR one most of all).
    fn take(
        &mut self,
        part: Option<(usize, usize, usize, usize)>,
    ) -> Result<Option<Image<u8>>, CaptureError> {
        if self.dup.is_none() {
            self.reopen();
        }
        let Some(dup) = self.dup.clone() else {
            return Ok(None);
        };
        self.refresh_white();
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe { dup.AcquireNextFrame(0, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(_) => {
                // Access lost, or a system transition: reopen, as dxcam does.
                self.reopen();
                return Ok(None);
            }
        }
        // Copy to the staging texture, then hand the frame back at once.
        let copied = (|| {
            let tex: ID3D11Texture2D = resource
                .ok_or(CaptureError::System("no frame resource".into()))?
                .cast()
                .map_err(|e| sys("frame texture", e))?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { tex.GetDesc(&mut desc) };
            let staging = self.staging_for(&desc)?;
            unsafe { self.context.CopyResource(&staging, &tex) };
            Ok::<_, CaptureError>((staging, desc))
        })();
        unsafe {
            let _ = dup.ReleaseFrame();
        }
        let (staging, desc) = copied?;
        if !self.primed {
            self.primed = true;
            return Ok(None);
        }
        let (w, h) = (desc.Width as usize, desc.Height as usize);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| sys("Map", e))?;
        }
        let pitch = mapped.RowPitch as usize;
        // The part, inside the frame, or the whole frame.
        let (px, py, pw, ph) = match part {
            Some((x, y, pw, ph)) if pw > 0 && ph > 0 && x + pw <= w && y + ph <= h => {
                (x, y, pw, ph)
            }
            Some(_) => {
                unsafe { self.context.Unmap(&staging, 0) };
                return Ok(None);
            }
            None => (0, 0, w, h),
        };
        let whole = (px, py, pw, ph) == (0, 0, w, h);
        let img = if desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT {
            let t = Instant::now();
            // One straight copy out of the mapped memory first: scattered
            // table reads are far slower there than from ordinary memory.
            let all =
                unsafe { std::slice::from_raw_parts(mapped.pData as *const u16, pitch / 2 * h) };
            let (raw, w, h, pitch): (Vec<u16>, usize, usize, usize) = if whole {
                (all.to_vec(), w, h, pitch)
            } else {
                let mut out = Vec::with_capacity(pw * 4 * ph);
                for r in py..py + ph {
                    let start = r * (pitch / 2) + px * 4;
                    out.extend_from_slice(&all[start..start + pw * 4]);
                }
                (out, pw, ph, pw * 8)
            };
            // A float frame when 8-bit was asked for (only the blank first
            // frame has been seen to, and it is passed over): converted at 80
            // nits, which is what Windows' own 8-bit copy of an HDR desktop
            // is - measured on this capture, 97.8% of values identical and
            // 99.96% within 1 of Windows' copy. (dxcam would read it as 8-bit.)
            let white = if self.float {
                self.sdr_white_nits
            } else {
                80.0
            };
            let lut = self.lut.get_or_insert_with(|| lut_for(white));
            let img = to_bgr(&raw, w, h, pitch / 2, lut);
            self.convert += t.elapsed();
            img
        } else {
            let raw = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, pitch * h) };
            if whole {
                bgra_to_bgr(raw, w, h, pitch)
            } else {
                let mut out = Vec::with_capacity(pw * 4 * ph);
                for r in py..py + ph {
                    let start = r * pitch + px * 4;
                    out.extend_from_slice(&raw[start..start + pw * 4]);
                }
                bgra_to_bgr(&out, pw, ph, pw * 4)
            }
        };
        unsafe { self.context.Unmap(&staging, 0) };
        self.format = Some(desc.Format);
        self.frames += 1;
        Ok(Some(img))
    }
}

impl Source for DesktopSource {
    fn grab(&mut self) -> Result<Option<Image<u8>>, CaptureError> {
        self.take(None)
    }

    fn grab_region(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
    ) -> Result<Option<Image<u8>>, CaptureError> {
        self.take(Some((x, y, w, h)))
    }

    /// Unrotated monitors only: a rotated one's frames are learnt by
    /// grabbing one.
    fn size(&self) -> Option<(usize, usize)> {
        (!self.info.rotated).then_some((self.info.width as usize, self.info.height as usize))
    }

    fn origin(&self) -> (i32, i32) {
        (self.info.left, self.info.top)
    }

    fn describe(&self) -> String {
        let what = match (self.float, self.format) {
            (true, Some(DXGI_FORMAT_R16G16B16A16_FLOAT)) => format!(
                "HDR (float, converted at SDR white {:.0} nits)",
                self.sdr_white_nits
            ),
            (true, Some(_)) => "HDR capture asked for, but Windows gave 8-bit frames".into(),
            (true, None) => "HDR capture (no frame yet)".into(),
            (false, _) => "8-bit".into(),
        };
        format!(
            "monitor {} ({}, {}x{}), {what}",
            self.info.monitor, self.info.device, self.info.width, self.info.height
        )
    }

    fn display_name(&self) -> Option<String> {
        Some(self.info.device.clone())
    }
}
