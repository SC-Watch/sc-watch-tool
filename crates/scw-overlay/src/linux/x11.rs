//! The overlay on X11: an override-redirect window - the window manager
//! leaves it alone, so it stays where it is put and over the rest - with a
//! 32-bit visual, see-through under a compositor, and an empty input shape,
//! so clicks go to the game under it.

use x11rb::connection::Connection;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    ClipOrdering, ColormapAlloc, ConfigureWindowAux, ConnectionExt as _, CreateGCAux,
    CreateWindowAux, ImageFormat, StackMode, VisualClass, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use super::Backend;
use crate::Drawn;

/// Rows of image per request, well inside X's request limit.
const MAX_REQUEST_BYTES: usize = 200 * 1024;

pub(crate) struct X11 {
    conn: RustConnection,
    pub(crate) window: u32,
    gc: u32,
    depth: u8,
    left: i32,
    top: i32,
    mapped: bool,
}

fn err(e: impl std::fmt::Display) -> String {
    format!("X11: {e}")
}

impl X11 {
    /// The window over a monitor whose top-left corner is at (left, top).
    pub(crate) fn open(left: i32, top: i32) -> Result<X11, String> {
        let (conn, screen_num) = x11rb::connect(None).map_err(err)?;
        let screen = conn.setup().roots[screen_num].clone();
        // a 32-bit visual, for see-through pixels; the screen's own without
        let (depth, visual) = screen
            .allowed_depths
            .iter()
            .filter(|d| d.depth == 32)
            .find_map(|d| {
                d.visuals
                    .iter()
                    .find(|v| v.class == VisualClass::TRUE_COLOR)
                    .map(|v| (32u8, v.visual_id))
            })
            .unwrap_or((screen.root_depth, screen.root_visual));
        let colormap = conn.generate_id().map_err(err)?;
        conn.create_colormap(ColormapAlloc::NONE, colormap, screen.root, visual)
            .map_err(err)?;
        let window = conn.generate_id().map_err(err)?;
        conn.create_window(
            depth,
            window,
            screen.root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new()
                .background_pixel(0)
                .border_pixel(0)
                .override_redirect(1)
                .colormap(colormap),
        )
        .map_err(err)?;
        // clicks go through: an empty input shape
        conn.shape_rectangles(
            shape::SO::SET,
            shape::SK::INPUT,
            ClipOrdering::UNSORTED,
            window,
            0,
            0,
            &[],
        )
        .map_err(err)?;
        let gc = conn.generate_id().map_err(err)?;
        conn.create_gc(gc, window, &CreateGCAux::new())
            .map_err(err)?;
        conn.flush().map_err(err)?;
        Ok(X11 {
            conn,
            window,
            gc,
            depth,
            left,
            top,
            mapped: false,
        })
    }
}

impl Backend for X11 {
    fn show(&mut self, d: &Drawn) -> Result<(), String> {
        if d.width == 0 || d.height == 0 {
            self.hide();
            return Ok(());
        }
        self.conn
            .configure_window(
                self.window,
                &ConfigureWindowAux::new()
                    .x(self.left + d.x)
                    .y(self.top + d.y)
                    .width(d.width as u32)
                    .height(d.height as u32)
                    .stack_mode(StackMode::ABOVE),
            )
            .map_err(err)?;
        if !self.mapped {
            self.conn.map_window(self.window).map_err(err)?;
            self.mapped = true;
        }
        let stride = d.width * 4;
        let rows = (MAX_REQUEST_BYTES / stride).max(1);
        let mut y = 0;
        while y < d.height {
            let n = rows.min(d.height - y);
            self.conn
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    self.window,
                    self.gc,
                    d.width as u16,
                    n as u16,
                    0,
                    y as i16,
                    0,
                    self.depth,
                    &d.bgra[y * stride..(y + n) * stride],
                )
                .map_err(err)?;
            y += n;
        }
        self.conn.flush().map_err(err)
    }

    fn hide(&mut self) {
        if self.mapped {
            let _ = self.conn.unmap_window(self.window);
            let _ = self.conn.flush();
            self.mapped = false;
        }
    }

    fn describe(&self) -> &'static str {
        "X11"
    }
}

impl Drop for X11 {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}
