//! The whole Wayland path against real programs: CI starts a headless
//! Wayland desktop (sway) with one solid-colour output, the screen-cast
//! portal with its wlroots backend (told to share that output without a
//! dialog), and PipeWire; `SCW_PORTAL_TEST` says they are up. The source
//! asks the portal for the screen, gets its PipeWire stream and reads it.
//! Skipped without it.
#![cfg(target_os = "linux")]

use scw_capture::Source;
use scw_capture::screencast::ScreenCastSource;

#[test]
fn shares_and_reads_the_screen_through_the_portal() {
    let Ok(colour) = std::env::var("SCW_PORTAL_TEST") else {
        println!("::warning title=screen-cast portal::skipped - SCW_PORTAL_TEST not set");
        return;
    };
    // the output's colour as RRGGBB, so BGR is its bytes backwards
    let rgb = u32::from_str_radix(&colour, 16).unwrap();
    let bgr = [rgb as u8, (rgb >> 8) as u8, (rgb >> 16) as u8];
    let token = std::env::temp_dir().join(format!("scw-portal-token-{}", std::process::id()));
    let said = std::sync::Mutex::new(Vec::new());
    println!("step: asking the portal");
    let mut src = ScreenCastSource::open(Some(token.clone()), &|s: &str| {
        println!("step: said {s:?}");
        said.lock().unwrap().push(s.to_string())
    })
    .expect("the portal shares the screen");
    let what = src.describe();
    println!("step: opened - {what}");
    assert!(what.starts_with("the shared screen, "), "{what}");
    assert_eq!(said.lock().unwrap().len(), 1, "told before the dialog");

    let mut frame = None;
    for i in 0..40 {
        println!("step: grab {i}");
        if let Some(img) = src.grab().unwrap() {
            frame = Some(img);
            break;
        }
    }
    let img = frame.expect("a frame of the screen within 10 s");
    println!("step: a frame {}x{}", img.width, img.height);
    let centre = ((img.height / 2) * img.width + img.width / 2) * 3;
    assert_eq!(&img.data[centre..centre + 3], &bgr, "the output's colour");
    // The size is known without a grab, and a part is read alone.
    assert_eq!(
        src.size(),
        Some((img.width, img.height)),
        "the stream's size"
    );
    // A still screen sends no new picture, and a grab takes only a new one:
    // sway paints its output a second colour, and the part shows it.
    let second = [bgr[2], bgr[1], bgr[0]];
    let painted = std::process::Command::new("swaymsg")
        .args(["output", "*", "bg"])
        .arg(format!(
            "#{:02x}{:02x}{:02x}",
            second[2], second[1], second[0]
        ))
        .arg("solid_color")
        .status()
        .is_ok_and(|s| s.success());
    assert!(painted, "swaymsg repaints the output (SWAYSOCK set?)");
    let mut part = None;
    for _ in 0..40 {
        if let Some(p) = src
            .grab_region(img.width / 2, img.height / 2, 16, 8)
            .unwrap()
            && p.data[..3] == second
        {
            part = Some(p);
            break;
        }
    }
    let part = part.expect("the repainted part of the screen within 10 s");
    assert_eq!((part.width, part.height), (16, 8));
    assert!(
        part.data.chunks(3).all(|p| p == second),
        "the part's colour"
    );
    let kept = token.exists();
    let _ = std::fs::remove_file(&token);
    println!(
        "::notice title=screen-cast portal::ran - {what}; read pixel-exact; restore token {}",
        if kept {
            "kept"
        } else {
            "not offered by this backend"
        }
    );
}
