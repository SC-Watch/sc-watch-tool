//! The screen on Linux through X11: the Star Citizen window itself when it
//! is there, else a whole monitor.
//!
//! Proton and Wine run the game as an X11 window - under XWayland on a
//! Wayland desktop such as KDE Plasma - and an X11 client may read another
//! X11 window's pixels (as OBS's window capture does), with no permission
//! prompt. A native Wayland window cannot be read this way; that needs the
//! screen-cast portal (PORT.md, phase 4).

use std::time::{Duration, Instant};

use scw_vision::Image;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, ImageFormat, ImageOrder, Window};
use x11rb::rust_connection::RustConnection;

use crate::{CaptureError, Source};

/// How often a missing game window is looked for again.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// What the game's window is called (its title, or Wine's WM_CLASS).
pub const GAME_WINDOW: [&str; 2] = ["Star Citizen", "starcitizen"];

fn sys(what: &str, e: impl std::fmt::Display) -> CaptureError {
    CaptureError::System(format!("{what}: {e}"))
}

/// One X11 image (ZPixmap, 32 bits a pixel) as BGR.
pub fn to_bgr(data: &[u8], width: usize, height: usize, lsb: bool) -> Option<Image<u8>> {
    if data.len() < width * height * 4 {
        return None;
    }
    let mut out = Image::new(width, height, 3);
    let pixels = data.as_chunks::<4>().0.iter().take(width * height);
    for (px, o) in pixels.zip(out.data.as_chunks_mut::<3>().0.iter_mut()) {
        if lsb {
            o.copy_from_slice(&px[..3]); // B G R x
        } else {
            o.copy_from_slice(&[px[3], px[2], px[1]]); // x R G B
        }
    }
    Some(out)
}

/// Does a window's class or title name one of `hints` (any case)?
pub fn names_match(class: &str, title: &str, hints: &[&str]) -> bool {
    let (class, title) = (class.to_lowercase(), title.to_lowercase());
    hints.iter().any(|h| {
        let h = h.to_lowercase();
        !h.is_empty() && (class.contains(&h) || title.contains(&h))
    })
}

pub struct X11Source {
    conn: RustConnection,
    root: Window,
    lsb: bool,
    monitor: usize,
    hints: Vec<String>,
    window: Option<Window>,
    looked: Option<Instant>,
    /// What the last frame came from.
    pub from: String,
}

impl X11Source {
    /// Monitor `monitor` (1-based) of the default display; the window named
    /// by `hints` instead whenever there is one.
    pub fn open(monitor: usize, hints: &[&str]) -> Result<Self, CaptureError> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| {
            sys(
                "no X display (is this a desktop session, with DISPLAY set?)",
                e,
            )
        })?;
        let setup = conn.setup();
        let root = setup.roots[screen].root;
        let lsb = setup.image_byte_order == ImageOrder::LSB_FIRST;
        let src = X11Source {
            conn,
            root,
            lsb,
            monitor,
            hints: hints.iter().map(|h| h.to_string()).collect(),
            window: None,
            looked: None,
            from: String::new(),
        };
        let monitors = src.monitors();
        if !(1..=monitors.len()).contains(&monitor) {
            return Err(CaptureError::NoMonitor {
                asked: monitor,
                count: monitors.len(),
            });
        }
        Ok(src)
    }

    fn text_property(&self, w: Window, prop: u32, kind: u32) -> String {
        self.conn
            .get_property(false, w, prop, kind, 0, 1024)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.value).replace('\0', " "))
            .unwrap_or_default()
    }

    fn atom(&self, name: &str) -> Option<u32> {
        self.conn
            .intern_atom(true, name.as_bytes())
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
            .filter(|a| *a != 0)
    }

    /// The windows a window manager lists, else the root's children and
    /// theirs.
    fn candidates(&self) -> Vec<Window> {
        if let Some(list) = self.atom("_NET_CLIENT_LIST")
            && let Ok(Ok(r)) = self
                .conn
                .get_property(false, self.root, list, AtomEnum::WINDOW, 0, 4096)
                .map(|c| c.reply())
            && let Some(ws) = r.value32()
        {
            let ws: Vec<Window> = ws.collect();
            if !ws.is_empty() {
                return ws;
            }
        }
        let mut out = Vec::new();
        if let Ok(Ok(top)) = self.conn.query_tree(self.root).map(|c| c.reply()) {
            for w in top.children {
                out.push(w);
                if let Ok(Ok(kids)) = self.conn.query_tree(w).map(|c| c.reply()) {
                    out.extend(kids.children);
                }
            }
        }
        out
    }

    /// The game's window, if it is open.
    pub fn find_window(&self) -> Option<Window> {
        let hints: Vec<&str> = self.hints.iter().map(String::as_str).collect();
        let utf8 = self.atom("UTF8_STRING");
        let net_name = self.atom("_NET_WM_NAME");
        self.candidates().into_iter().find(|&w| {
            let class = self.text_property(w, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into());
            let mut title = self.text_property(w, AtomEnum::WM_NAME.into(), AtomEnum::ANY.into());
            if let (Some(n), Some(u)) = (net_name, utf8) {
                title.push(' ');
                title.push_str(&self.text_property(w, n, u));
            }
            names_match(&class, &title, &hints)
        })
    }

    /// The monitors, as (x, y, width, height); the whole screen when RandR
    /// does not say.
    pub fn monitors(&self) -> Vec<(i16, i16, u16, u16)> {
        if let Ok(Ok(r)) = self
            .conn
            .randr_get_monitors(self.root, true)
            .map(|c| c.reply())
            && !r.monitors.is_empty()
        {
            let mut m: Vec<_> = r
                .monitors
                .iter()
                .map(|m| (m.primary, m.x, m.y, m.width, m.height))
                .collect();
            // the primary first, then left to right (dxcam's order puts the
            // main display first too)
            m.sort_by_key(|&(p, x, y, _, _)| (!p, x, y));
            return m.into_iter().map(|(_, x, y, w, h)| (x, y, w, h)).collect();
        }
        match self.conn.get_geometry(self.root).map(|c| c.reply()) {
            Ok(Ok(g)) => vec![(0, 0, g.width, g.height)],
            _ => Vec::new(),
        }
    }

    fn image(
        &self,
        drawable: u32,
        x: i16,
        y: i16,
        w: u16,
        h: u16,
    ) -> Result<Image<u8>, CaptureError> {
        let r = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, drawable, x, y, w, h, !0)
            .map_err(|e| sys("GetImage", e))?
            .reply()
            .map_err(|e| sys("GetImage", e))?;
        to_bgr(&r.data, usize::from(w), usize::from(h), self.lsb).ok_or_else(|| {
            CaptureError::System(format!("an image of depth {} is not 32-bit", r.depth))
        })
    }

    fn window_image(&self, w: Window) -> Result<Image<u8>, CaptureError> {
        let g = self
            .conn
            .get_geometry(w)
            .map_err(|e| sys("GetGeometry", e))?
            .reply()
            .map_err(|e| sys("GetGeometry", e))?;
        self.image(w, 0, 0, g.width, g.height)
    }
}

impl Source for X11Source {
    fn grab(&mut self) -> Result<Option<Image<u8>>, CaptureError> {
        if self.window.is_none() && self.looked.is_none_or(|t| t.elapsed() >= LOOK_EVERY) {
            self.window = self.find_window();
            self.looked = Some(Instant::now());
        }
        if let Some(w) = self.window {
            match self.window_image(w) {
                Ok(img) => {
                    self.from = format!("the {} window", self.hints.first().map_or("", |h| h));
                    return Ok(Some(img));
                }
                // closed, or not viewable: look again, and use the screen
                Err(_) => self.window = None,
            }
        }
        let Some(&(x, y, w, h)) = self.monitors().get(self.monitor.saturating_sub(1)) else {
            return Ok(None);
        };
        self.from = format!("monitor {}", self.monitor);
        self.image(self.root, x, y, w, h).map(Some)
    }

    fn describe(&self) -> String {
        format!(
            "X11: the {} window when it is open, else monitor {}",
            self.hints.first().map_or("game", |h| h),
            self.monitor
        )
    }

    fn display_name(&self) -> Option<String> {
        std::env::var("DISPLAY").ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_and_names() {
        let lsb = [10u8, 20, 30, 0, 40, 50, 60, 0];
        assert_eq!(
            to_bgr(&lsb, 2, 1, true).unwrap().data,
            vec![10, 20, 30, 40, 50, 60]
        );
        let msb = [0u8, 30, 20, 10, 0, 60, 50, 40];
        assert_eq!(
            to_bgr(&msb, 2, 1, false).unwrap().data,
            vec![10, 20, 30, 40, 50, 60]
        );
        assert!(to_bgr(&lsb, 3, 1, true).is_none(), "short data");
        assert!(names_match(
            "starcitizen.exe starcitizen.exe",
            "",
            &GAME_WINDOW
        ));
        assert!(names_match("", "Star Citizen", &GAME_WINDOW));
        assert!(!names_match(
            "konsole",
            "Star Citizen wiki - Firefox",
            &["starcitizen"]
        ));
        assert!(!names_match("", "", &[""]));
    }
}
