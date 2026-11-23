//! Where sc-watch's frames come from: the desktop (Windows Desktop
//! Duplication, as the Python's dxcam and hdr_capture.py, 8-bit or HDR float
//! converted to what the game drew) or saved frames. Every source gives BGR
//! 8-bit images ([`scw_vision::Image`]) through one interface, [`Source`].
//!
//! On Linux, [`x11::X11Source`] reads the game's X11 window (or a monitor);
//! the screen-cast portal for native Wayland windows is still to come.

pub mod hdr;
pub mod replay;
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
}
