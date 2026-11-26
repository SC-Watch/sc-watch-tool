//! The overlay's window on Windows: a layered, click-through, always-on-top
//! tool window (as IRIS's LayeredWindow), on a thread of its own that owns
//! it, fed drawn labels through a channel. It is kept out of screen
//! captures, so neither reader ever reads the label it drew.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HGDIOBJ,
    ReleaseDC, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HWND_TOPMOST, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage, ULW_ALPHA,
    UpdateLayeredWindow, WDA_EXCLUDEFROMCAPTURE, WINDOW_EX_STYLE, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

use crate::Drawn;

enum Command {
    Show(Drawn),
    Hide,
    Quit,
}

/// The overlay window, on its own thread; dropped, it closes.
pub struct OverlayWindow {
    tx: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl OverlayWindow {
    /// The window for a monitor whose top-left corner is at (left, top) on
    /// the desktop (labels are drawn in that monitor's pixels).
    pub fn open(left: i32, top: i32) -> Result<OverlayWindow, String> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        let thread = std::thread::Builder::new()
            .name("mining overlay".into())
            .spawn(move || run(rx, ready_tx, left, top))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(OverlayWindow {
                tx,
                thread: Some(thread),
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
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn run(rx: Receiver<Command>, ready: Sender<Result<(), String>>, left: i32, top: i32) {
    let hwnd = match unsafe { create() } {
        Ok(h) => {
            let _ = ready.send(Ok(()));
            h
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut shown = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Command::Show(d)) => {
                unsafe { update(hwnd, &d, left, top) };
                if !shown {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    }
                    shown = true;
                }
                // the game may have put itself on top meanwhile
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
            Ok(Command::Hide) => {
                if shown {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                    shown = false;
                }
            }
            Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        pump();
    }
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

fn pump() {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe fn create() -> Result<HWND, String> {
    unsafe {
        // the label is drawn in the capture's real pixels
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("scw-mining-overlay");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        // a second window of the class in one process: already registered
        let _ = RegisterClassW(&wc);
        let ex: WINDOW_EX_STYLE =
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let hwnd = CreateWindowExW(
            ex,
            class,
            w!("sc-watch mining overlay"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| format!("the overlay window: {e}"))?;
        // out of every capture - its own readers' included
        let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        Ok(hwnd)
    }
}

/// Puts the drawn label in the window: a 32-bit premultiplied bitmap,
/// blended by the window manager per pixel.
unsafe fn update(hwnd: HWND, d: &Drawn, left: i32, top: i32) {
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: d.width as i32,
                // negative: rows top to bottom
                biHeight: -(d.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let Ok(bitmap) = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        else {
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            return;
        };
        std::ptr::copy_nonoverlapping(d.bgra.as_ptr(), bits as *mut u8, d.bgra.len());
        let old = SelectObject(mem, HGDIOBJ(bitmap.0));
        let pos = POINT {
            x: left + d.x,
            y: top + d.y,
        };
        let size = SIZE {
            cx: d.width as i32,
            cy: d.height as i32,
        };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            Some(screen),
            Some(&pos),
            Some(&size),
            Some(mem),
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}
