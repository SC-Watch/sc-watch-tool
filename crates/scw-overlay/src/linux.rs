//! The overlay's window on Linux, on a thread of its own fed drawn labels
//! through a channel, as on Windows:
//!
//! - Wayland: a wlr-layer-shell surface on the overlay layer (KDE Plasma,
//!   Sway, Hyprland - not GNOME, which has no layer shell; not under
//!   gamescope), on the monitor whose size is the captured screen's,
//!   anchored at its top-left corner and placed by margins. Clicks go
//!   through (an empty input region) and it never takes the keyboard. A
//!   scaled monitor's label is drawn in its real pixels and shown at its
//!   logical size (wp_viewporter).
//! - X11: an override-redirect window with a 32-bit visual (see-through
//!   under a compositor) and an empty input shape.
//!
//! `SCW_OVERLAY` (wayland or x11) chooses outright.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::Drawn;

mod wayland;
mod x11;

pub(crate) enum Command {
    Screen(u32, u32),
    Show(Drawn),
    Hide,
    Quit,
}

/// A window system the overlay can draw on.
pub(crate) trait Backend {
    /// The captured screen's size: which monitor to draw on.
    fn screen(&mut self, _w: u32, _h: u32) {}
    fn show(&mut self, d: &Drawn) -> Result<(), String>;
    fn hide(&mut self);
    /// Handles what the server sent meanwhile.
    fn pump(&mut self) {}
    fn describe(&self) -> &'static str;
}

/// The overlay window, on its own thread; dropped, it closes.
pub struct OverlayWindow {
    tx: Sender<Command>,
    thread: Option<JoinHandle<()>>,
    /// "Wayland (layer shell)" or "X11".
    pub kind: &'static str,
}

fn open_backend(left: i32, top: i32) -> Result<Box<dyn Backend>, String> {
    let forced = std::env::var("SCW_OVERLAY").ok();
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x = std::env::var_os("DISPLAY").is_some();
    match forced.as_deref() {
        Some("wayland") => {
            return wayland::Wayland::open().map(|b| Box::new(b) as Box<dyn Backend>);
        }
        Some("x11") => return x11::X11::open(left, top).map(|b| Box::new(b) as Box<dyn Backend>),
        _ => {}
    }
    let mut why = Vec::new();
    if wayland {
        match wayland::Wayland::open() {
            Ok(b) => return Ok(Box::new(b)),
            Err(e) => why.push(format!("Wayland: {e}")),
        }
    }
    if x {
        match x11::X11::open(left, top) {
            Ok(b) => return Ok(Box::new(b)),
            Err(e) => why.push(format!("X11: {e}")),
        }
    }
    if why.is_empty() {
        why.push("no Wayland or X11 display".into());
    }
    Err(why.join("; "))
}

impl OverlayWindow {
    /// The window for a monitor whose top-left corner is at (left, top) on
    /// the desktop (used on X11; on Wayland the monitor is the one the
    /// screen's size matches).
    pub fn open(left: i32, top: i32) -> Result<OverlayWindow, String> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let thread = std::thread::Builder::new()
            .name("mining overlay".into())
            .spawn(move || match open_backend(left, top) {
                Ok(b) => {
                    let _ = ready_tx.send(Ok(b.describe()));
                    run(rx, b);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(kind)) => Ok(OverlayWindow {
                tx,
                thread: Some(thread),
                kind,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the overlay window did not open".into()),
        }
    }

    /// Shows a label, or hides the window.
    pub fn show(&self, drawn: Option<Drawn>) {
        let _ = self.tx.send(match drawn {
            Some(d) => Command::Show(d),
            None => Command::Hide,
        });
    }

    /// The captured screen's size: on Wayland, the monitor to draw on.
    pub fn set_screen(&self, w: u32, h: u32) {
        let _ = self.tx.send(Command::Screen(w, h));
    }

    /// Which window system it draws on.
    pub fn kind(&self) -> &'static str {
        self.kind
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(rx: Receiver<Command>, mut b: Box<dyn Backend>) {
    let mut said = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Command::Screen(w, h)) => b.screen(w, h),
            Ok(Command::Show(d)) => {
                if let Err(e) = b.show(&d)
                    && !said
                {
                    eprintln!("  ! mining overlay: {e}");
                    said = true;
                }
            }
            Ok(Command::Hide) => b.hide(),
            Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        b.pump();
    }
    b.hide();
}
