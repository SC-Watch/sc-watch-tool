//! Draws a few mining labels with the system font into one PNG, over a
//! grey-blue backdrop like the game's sky, to look at the drawing.
//!
//!     cargo run --release -p scw-overlay --example draw_label -- out.png
use scw_overlay::{Label, Painter};

fn main() {
    let out = std::env::args().nth(1).expect("an output .png");
    let painter = Painter::system().expect("a system font");
    println!("font: {}", painter.font_path.display());
    let (w, h) = (900usize, 300usize);
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            rgb[i] = 110 + (y / 6) as u8;
            rgb[i + 1] = 140 + (y / 8) as u8;
            rgb[i + 2] = 170;
        }
    }
    // text, signal x and y, digit height, text and back colours (BGR)
    type Sample<'a> = (&'a str, f64, f64, f64, [u8; 3], [u8; 3]);
    let labels: [Sample; 4] = [
        (
            "Hephaestanite 4x",
            60.0,
            30.0,
            13.0,
            [255, 230, 230],
            [150, 90, 120],
        ),
        (
            "FPS Mineables 2x / Salvage 3x",
            420.0,
            30.0,
            13.0,
            [255, 255, 255],
            [160, 100, 140],
        ),
        ("Multiple", 60.0, 160.0, 12.0, [250, 240, 255], [40, 40, 50]),
        (
            "Quantainium 2x",
            420.0,
            160.0,
            18.0,
            [80, 220, 255],
            [20, 25, 30],
        ),
    ];
    for (text, sx, sy, dh, tc, bc) in labels {
        let label = Label {
            text: text.into(),
            signal: [sx, sy, 110.0, dh + 4.0],
            digit_height: dh,
            text_bgr: tc,
            back_bgr: bc,
        };
        let d = painter.draw(&label, 1.5).unwrap();
        for y in 0..d.height {
            for x in 0..d.width {
                let (px, py) = (d.x as usize + x, d.y as usize + y);
                if px >= w || py >= h {
                    continue;
                }
                let s = (y * d.width + x) * 4;
                let a = d.bgra[s + 3] as f32 / 255.0;
                let i = (py * w + px) * 3;
                // premultiplied BGRA over RGB
                for (k, c) in [(0, 2), (1, 1), (2, 0)] {
                    rgb[i + k] =
                        (d.bgra[s + c] as f32 + rgb[i + k] as f32 * (1.0 - a)).round() as u8;
                }
            }
        }
    }
    let file = std::fs::File::create(&out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    println!("wrote {out}");
}
