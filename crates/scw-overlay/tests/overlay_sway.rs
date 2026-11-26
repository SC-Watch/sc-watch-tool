//! The overlay window on a real Wayland desktop: a headless sway on CI
//! (.github/scripts/linux-screencast.sh), seen through `grim`'s screenshots
//! of the output - a label shows where it was put, half-transparent pixels
//! blend with what is under them, it moves, and it goes. Runs only when
//! SCW_OVERLAY_TEST is set (and grim is there); otherwise it says it
//! skipped and passes.
#![cfg(target_os = "linux")]

use std::time::Duration;

use scw_overlay::{Drawn, OverlayWindow};

/// A screenshot of the whole desktop: width, height, RGB.
fn grim() -> Option<(usize, usize, Vec<u8>)> {
    let out = std::process::Command::new("grim")
        .args(["-t", "ppm", "-"])
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!("grim: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    // P6 <w> <h> 255 <rgb>
    let data = out.stdout;
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while data[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while !data[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&data[start..i]).into_owned());
    }
    assert_eq!(fields[0], "P6");
    let (w, h): (usize, usize) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let rgb = data[i + 1..].to_vec();
    assert_eq!(rgb.len(), w * h * 3);
    Some((w, h, rgb))
}

fn pixel(shot: &(usize, usize, Vec<u8>), x: usize, y: usize) -> [u8; 3] {
    let i = (y * shot.0 + x) * 3;
    [shot.2[i], shot.2[i + 1], shot.2[i + 2]]
}

/// A label of one colour, premultiplied BGRA.
fn solid(x: i32, y: i32, w: usize, h: usize, bgra: [u8; 4]) -> Drawn {
    Drawn {
        x,
        y,
        width: w,
        height: h,
        bgra: bgra.repeat(w * h),
    }
}

fn settle() {
    std::thread::sleep(Duration::from_millis(800));
}

#[test]
fn a_label_shows_where_it_is_put_on_a_wayland_desktop() {
    if std::env::var_os("SCW_OVERLAY_TEST").is_none() {
        eprintln!("skipped: SCW_OVERLAY_TEST is not set");
        return;
    }
    let Some(before) = grim() else {
        eprintln!("skipped: no screenshot (grim)");
        return;
    };
    let (w, h) = (before.0, before.1);
    println!("step: the desktop is {w}x{h}");
    let win = OverlayWindow::open(0, 0).expect("the overlay window");
    assert_eq!(win.kind(), "Wayland (layer shell)");
    win.set_screen(w as u32, h as u32);
    let back = pixel(&before, 200, 110);

    // opaque: red 0x20, green 0xc0, blue 0x40
    win.show(Some(solid(100, 80, 200, 60, [0x40, 0xc0, 0x20, 0xff])));
    settle();
    let shot = grim().expect("a screenshot");
    println!(
        "step: shown - {:?} at the label, {:?} beside it",
        pixel(&shot, 200, 110),
        pixel(&shot, 50, 50)
    );
    assert_eq!(
        pixel(&shot, 200, 110),
        [0x20, 0xc0, 0x40],
        "the label where it was put"
    );
    assert_eq!(pixel(&shot, 101, 81), [0x20, 0xc0, 0x40], "its corner");
    assert_eq!(
        pixel(&shot, 99, 79),
        pixel(&before, 99, 79),
        "nothing outside it"
    );
    assert_eq!(pixel(&shot, 50, 50), pixel(&before, 50, 50));

    // half alpha: premultiplied, so it blends with the desktop under it
    win.show(Some(solid(400, 200, 120, 40, [0x00, 0x00, 0x80, 0x80])));
    settle();
    let shot = grim().expect("a screenshot");
    let blended = pixel(&shot, 450, 220);
    let under = pixel(&before, 450, 220);
    println!("step: half-transparent - {blended:?} over {under:?}");
    let want = [0x80 + under[0] / 2, under[1] / 2, under[2] / 2];
    for k in 0..3 {
        assert!(
            (blended[k] as i32 - want[k] as i32).abs() <= 3,
            "blended {blended:?}, want about {want:?}"
        );
    }
    assert_eq!(
        pixel(&shot, 200, 110),
        back,
        "moved: gone from where it was"
    );

    win.show(None);
    settle();
    let shot = grim().expect("a screenshot");
    println!("step: hidden - {:?}", pixel(&shot, 450, 220));
    assert_eq!(pixel(&shot, 450, 220), under, "hidden");
}
