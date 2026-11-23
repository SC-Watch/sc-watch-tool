//! The X11 capture against a real X server (Xvfb in CI): a window named as
//! Wine names the game's is found, and its pixels read back exactly; with
//! no such window the monitor is read. Skipped without a display.
#![cfg(target_os = "linux")]

use std::time::Duration;

use scw_capture::Source;
use scw_capture::x11::{GAME_WINDOW, X11Source};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt, CreateGCAux, CreateWindowAux, PropMode, Rectangle, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;

const RED: u32 = 0xFF0000;
const BLUE: u32 = 0x0000FF;

#[test]
fn reads_the_game_window_and_the_monitor() {
    if std::env::var("DISPLAY").is_err() {
        eprintln!("skipped: no DISPLAY");
        return;
    }
    let (conn, screen) = x11rb::connect(None).unwrap();
    let scr = &conn.setup().roots[screen];
    let (w, h) = (200u16, 120u16);

    // With no game window: the monitor.
    let mut src = X11Source::open(1, &GAME_WINDOW).unwrap();
    let frame = src.grab().unwrap().expect("a frame of the monitor");
    assert_eq!(
        (frame.width, frame.height),
        (
            usize::from(scr.width_in_pixels),
            usize::from(scr.height_in_pixels)
        )
    );
    assert_eq!(src.from, "monitor 1");

    // A window as Wine makes the game's: red, with a blue right half.
    let win = conn.generate_id().unwrap();
    conn.create_window(
        scr.root_depth,
        win,
        scr.root,
        10,
        20,
        w,
        h,
        0,
        WindowClass::INPUT_OUTPUT,
        scr.root_visual,
        &CreateWindowAux::new().background_pixel(RED),
    )
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"starcitizen.exe\0starcitizen.exe\0",
    )
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"Star Citizen",
    )
    .unwrap();
    conn.map_window(win).unwrap();
    conn.sync().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, win, &CreateGCAux::new().foreground(BLUE))
        .unwrap();
    conn.poly_fill_rectangle(
        win,
        gc,
        &[Rectangle {
            x: (w / 2) as i16,
            y: 0,
            width: w / 2,
            height: h,
        }],
    )
    .unwrap();
    conn.sync().unwrap();
    std::thread::sleep(Duration::from_millis(200));

    let mut src = X11Source::open(1, &GAME_WINDOW).unwrap();
    assert_eq!(src.find_window(), Some(win), "found by its class and title");
    let frame = src.grab().unwrap().expect("a frame of the window");
    assert_eq!(
        (frame.width, frame.height),
        (usize::from(w), usize::from(h))
    );
    assert_eq!(src.from, "the Star Citizen window");
    let px = |x: usize, y: usize| {
        let i = (y * frame.width + x) * 3;
        [frame.data[i], frame.data[i + 1], frame.data[i + 2]]
    };
    assert_eq!(px(10, 10), [0, 0, 255], "red, as BGR");
    assert_eq!(px(150, 60), [255, 0, 0], "blue, as BGR");

    // Gone: back to the monitor (after the next look).
    conn.destroy_window(win).unwrap();
    conn.sync().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let frame = src.grab().unwrap().expect("the monitor again");
    assert_eq!(frame.width, usize::from(scr.width_in_pixels));
}
