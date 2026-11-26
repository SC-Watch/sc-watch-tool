//! Where sc-watch's frames come from: the desktop (Windows Desktop
//! Duplication, as the Python's dxcam and hdr_capture.py, 8-bit or HDR float
//! converted to what the game drew) or saved frames. Every source gives BGR
//! 8-bit images ([`scw_vision::Image`]) through one interface, [`Source`].
//!
//! On Linux, [`x11::X11Source`] reads the game's X11 window (or a monitor)
//! on an X11 desktop, and [`screencast::ScreenCastSource`] the screen a
//! Wayland desktop shares through its screen-cast portal, over PipeWire.

pub mod hdr;
#[cfg(target_os = "linux")]
pub mod pipewire;
#[cfg(target_os = "linux")]
pub mod portal;
pub mod replay;
#[cfg(target_os = "linux")]
pub mod screencast;
pub mod spa;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "linux")]
pub mod x11;

use std::fmt;

use scw_vision::Image;

pub use hdr::HdrCapture;
pub use replay::ReplaySource;

/// Why a source could not give frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// No such monitor (1-based), and how many there are.
    NoMonitor { asked: usize, count: usize },
    /// The display is rotated; the capture does not turn frames yet.
    Rotated,
    /// The operating system refused, with its message.
    System(String),
    /// A saved frame could not be read.
    File(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::NoMonitor { asked, count } => {
                write!(
                    f,
                    "monitor {asked} does not exist ({count} on this adapter)"
                )
            }
            CaptureError::Rotated => write!(f, "rotated displays are not supported yet"),
            CaptureError::System(s) | CaptureError::File(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for CaptureError {}

/// How Linux is captured: decided by the desktop, not the build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxCapture {
    /// The X server's windows and screens (an X11 desktop).
    X11,
    /// The screen-cast portal and PipeWire (a Wayland desktop, where X11
    /// sees only XWayland's windows - not a game on Wine's Wayland driver).
    Portal,
}

impl LinuxCapture {
    /// `SCW_CAPTURE` (`x11` or `portal`) when set, else the portal on a
    /// Wayland session (`WAYLAND_DISPLAY`), else X11.
    pub fn choose(env: &dyn Fn(&str) -> Option<String>) -> LinuxCapture {
        match env("SCW_CAPTURE")
            .map(|v| v.trim().to_lowercase())
            .as_deref()
        {
            Some("x11") => return LinuxCapture::X11,
            Some("portal") => return LinuxCapture::Portal,
            _ => {}
        }
        if env("WAYLAND_DISPLAY").is_some_and(|d| !d.trim().is_empty()) {
            LinuxCapture::Portal
        } else {
            LinuxCapture::X11
        }
    }
}

/// A source of frames.
pub trait Source {
    /// The next frame, or None when there is nothing new: a live screen that
    /// has not changed since the last grab (as dxcam's grab), a capture that
    /// was lost and is being reopened, or a replay that has run out.
    fn grab(&mut self) -> Result<Option<Image<u8>>, CaptureError>;

    /// One line for the startup banner.
    fn describe(&self) -> String;

    /// Windows' name for the display (`\\.\DISPLAY1`), when there is one.
    fn display_name(&self) -> Option<String> {
        None
    }

    /// True once a replay has given its last frame; a screen never runs out.
    fn exhausted(&self) -> bool {
        false
    }

    /// The frames' size, when it is known without grabbing one.
    fn size(&self) -> Option<(usize, usize)> {
        None
    }

    /// Where the captured screen's top-left corner is on the desktop (what
    /// an overlay window over it is placed by).
    fn origin(&self) -> (i32, i32) {
        (0, 0)
    }

    /// Only the part (x, y, width, height) of the next frame - the mining
    /// overlay reads a small band many times a second. By default the whole
    /// frame, cut down; None too when the part is not inside it.
    fn grab_region(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
    ) -> Result<Option<Image<u8>>, CaptureError> {
        Ok(self.grab()?.and_then(|f| crop(&f, x, y, w, h)))
    }
}

/// The part (x, y, width, height) of an image, when it lies inside it.
pub fn crop(img: &Image<u8>, x: usize, y: usize, w: usize, h: usize) -> Option<Image<u8>> {
    if w == 0 || h == 0 || x + w > img.width || y + h > img.height {
        return None;
    }
    let cn = img.channels;
    let mut out = Image::new(w, h, cn);
    for r in 0..h {
        let src = img.row(y + r);
        out.data[r * w * cn..(r + 1) * w * cn].copy_from_slice(&src[x * cn..(x + w) * cn]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::LinuxCapture;

    #[test]
    fn linux_capture_follows_the_desktop() {
        let with = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let choose = |pairs| LinuxCapture::choose(&with(pairs));
        assert_eq!(choose(&[]), LinuxCapture::X11);
        assert_eq!(
            choose(&[("WAYLAND_DISPLAY", "wayland-0")]),
            LinuxCapture::Portal
        );
        assert_eq!(choose(&[("WAYLAND_DISPLAY", " ")]), LinuxCapture::X11);
        assert_eq!(
            choose(&[("WAYLAND_DISPLAY", "wayland-0"), ("SCW_CAPTURE", "X11")]),
            LinuxCapture::X11
        );
        assert_eq!(choose(&[("SCW_CAPTURE", "portal")]), LinuxCapture::Portal);
        assert_eq!(
            choose(&[("SCW_CAPTURE", "other"), ("DISPLAY", ":0")]),
            LinuxCapture::X11
        );
    }
}
