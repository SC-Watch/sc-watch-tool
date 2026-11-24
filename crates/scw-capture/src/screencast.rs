//! The screen on a Wayland desktop: shared through the screen-cast portal
//! ([`crate::portal`]) and read over PipeWire ([`crate::pipewire`]). This is
//! how sc-watch sees a game that draws a native Wayland window - Wine's
//! Wayland driver - which no X11 client can read.
//!
//! The desktop asks once which screen to share; the answer's restore token
//! is kept in `token_file` so later starts (and the watcher's restarts)
//! share it again without asking.

use std::path::{Path, PathBuf};
use std::time::Duration;

use scw_vision::Image;

use crate::pipewire::{PwCapture, Target};
use crate::portal::{self, Grant, MONITOR, WINDOW};
use crate::{CaptureError, Source};

/// How long a grab waits for the next picture. A game redraws far more
/// often than this; a desktop sends nothing while the screen is still.
const WAIT: Duration = Duration::from_millis(250);

pub struct ScreenCastSource {
    // The stream goes before the share it reads.
    stream: PwCapture,
    grant: Grant,
    what: String,
}

fn read_token(path: &Path) -> Option<String> {
    let t = std::fs::read_to_string(path).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

impl ScreenCastSource {
    /// Shares a screen (asking the user unless `token_file` holds a token
    /// the desktop still honours) and starts reading it. `say` is told what
    /// is happening, since the desktop's dialog can wait on the user.
    pub fn open(token_file: Option<PathBuf>, say: &dyn Fn(&str)) -> Result<Self, CaptureError> {
        let restore = token_file.as_deref().and_then(read_token);
        let mut grant =
            portal::share_screen(MONITOR, restore.as_deref(), say).map_err(CaptureError::System)?;
        if let Some(path) = &token_file {
            // The old token is spent either way; keep the new one, if any.
            let _ = match &grant.restore_token {
                Some(t) => std::fs::write(path, format!("{t}\n")),
                None => std::fs::remove_file(path),
            };
        }
        let stream = PwCapture::open(grant.fd.take(), Target::Id(grant.node), "sc-watch")
            .map_err(CaptureError::System)?;
        let format = stream
            .wait_format(Duration::from_secs(5))
            .map_err(CaptureError::System)?;
        let kind = match grant.source_type {
            Some(WINDOW) => "the shared window",
            _ => "the shared screen",
        };
        let what = format!(
            "{kind}, {}x{} (desktop screen-cast portal v{}, PipeWire)",
            format.width, format.height, grant.version
        );
        Ok(ScreenCastSource {
            stream,
            grant,
            what,
        })
    }

    /// The portal's own report of the screen's size on the desktop.
    pub fn desktop_size(&self) -> Option<(i32, i32)> {
        self.grant.size
    }
}

impl Source for ScreenCastSource {
    fn grab(&mut self) -> Result<Option<Image<u8>>, CaptureError> {
        self.stream.next_frame(WAIT)
    }

    fn describe(&self) -> String {
        self.what.clone()
    }
}
