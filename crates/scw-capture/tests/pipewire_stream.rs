//! The PipeWire reader against a real PipeWire: CI runs a daemon, a session
//! manager and GStreamer's videotestsrc into a PipeWire node of one known
//! colour, and names the node in `SCW_PW_TEST_SOURCE`. Skipped without it.
#![cfg(target_os = "linux")]

use std::time::Duration;

use scw_capture::pipewire::{PwCapture, Target};

#[test]
fn reads_a_pipewire_video_node() {
    let Ok(node) = std::env::var("SCW_PW_TEST_SOURCE") else {
        println!("::warning title=pipewire stream::skipped - SCW_PW_TEST_SOURCE not set");
        return;
    };
    let cap = PwCapture::open(None, Target::Object(node), "scw-test").expect("the stream opens");
    let f = cap
        .wait_format(Duration::from_secs(10))
        .expect("a format is agreed");
    assert_eq!((f.width, f.height), (320, 240), "the source's size");
    // The first grab may come before any buffer: try for a few seconds.
    let mut frame = None;
    for _ in 0..20 {
        if let Some(img) = cap.next_frame(Duration::from_millis(250)).unwrap() {
            frame = Some(img);
            break;
        }
    }
    let img = frame.expect("a frame within 5 s");
    assert_eq!((img.width, img.height, img.channels), (320, 240, 3));
    // videotestsrc's foreground-color 0xff2080c0 is red 0x20, green 0x80,
    // blue 0xc0: as BGR, everywhere.
    for (x, y) in [(0, 0), (160, 120), (319, 239)] {
        let i = (y * img.width + x) * 3;
        assert_eq!(&img.data[i..i + 3], &[0xc0, 0x80, 0x20], "pixel {x},{y}");
    }
    // A second grab is a later frame, not the same one again.
    let again = cap.next_frame(Duration::from_secs(2)).unwrap();
    assert!(again.is_some(), "the next frame arrives");
    let (streaming, frames) = cap.streaming();
    assert!(streaming);
    assert_eq!(frames, 2, "only the frames asked for were converted");
    println!(
        "::notice title=pipewire stream::ran - {}x{} format {} agreed, frames read pixel-exact",
        f.width, f.height, f.format
    );
}
