//! The overlay window on X11 (Xvfb on CI, under xvfb-run): an
//! override-redirect window, mapped where the label was put, its input shape
//! empty (clicks go through), holding the label's pixels; unmapped when
//! hidden. Runs only when SCW_OVERLAY_X11_TEST is set and there is a
//! display; otherwise it says it skipped and passes.
#![cfg(target_os = "linux")]

use std::time::Duration;

use scw_overlay::{Drawn, OverlayWindow};
use x11rb::connection::Connection;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, MapState};

fn settle() {
    std::thread::sleep(Duration::from_millis(500));
}

#[test]
fn a_label_window_on_x11() {
    if std::env::var_os("SCW_OVERLAY_X11_TEST").is_none() || std::env::var_os("DISPLAY").is_none() {
        println!("::warning title=x11 overlay::skipped - SCW_OVERLAY_X11_TEST or DISPLAY not set");
        return;
    }
    // SAFETY: set before any thread of this test reads the environment
    unsafe { std::env::set_var("SCW_OVERLAY", "x11") };
    let win = OverlayWindow::open(10, 20).expect("the overlay window");
    assert_eq!(win.kind(), "X11");
    let (w, h) = (160usize, 40usize);
    let bgra = [0x40u8, 0xc0, 0x20, 0xff].repeat(w * h);
    win.show(Some(Drawn {
        x: 100,
        y: 80,
        width: w,
        height: h,
        bgra,
    }));
    settle();

    let (conn, screen) = x11rb::connect(None).expect("the display");
    let root = conn.setup().roots[screen].root;
    let tree = conn.query_tree(root).unwrap().reply().unwrap();
    let mut found = None;
    for &child in &tree.children {
        let a = conn.get_window_attributes(child).unwrap().reply().unwrap();
        let g = conn.get_geometry(child).unwrap().reply().unwrap();
        if a.override_redirect && a.map_state == MapState::VIEWABLE && (g.x, g.y) == (110, 100) {
            found = Some((child, g.width, g.height, g.depth));
        }
    }
    let (window, gw, gh, depth) = found.expect("a mapped override-redirect window at (110, 100)");
    println!("step: window {window:#x}, {gw}x{gh}, depth {depth}");
    assert_eq!((gw as usize, gh as usize), (w, h));
    let input = conn
        .shape_get_rectangles(window, shape::SK::INPUT)
        .unwrap()
        .reply()
        .unwrap();
    assert!(
        input.rectangles.is_empty(),
        "clicks go through: {:?}",
        input.rectangles
    );
    let img = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 5, 5, 1, 1, !0)
        .unwrap()
        .reply()
        .unwrap();
    println!("step: a pixel of it {:?}", &img.data[..4]);
    assert_eq!(&img.data[..3], &[0x40, 0xc0, 0x20], "the label's pixels");

    win.show(None);
    settle();
    let a = conn.get_window_attributes(window).unwrap().reply().unwrap();
    assert_ne!(a.map_state, MapState::VIEWABLE, "hidden");
    println!(
        "::notice title=x11 overlay::ran - an override-redirect window of depth {depth} mapped where the label was put, {gw}x{gh}, its input shape empty, the label's pixels in it, unmapped when hidden"
    );
}
