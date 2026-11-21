//! Lists the monitors as the capture numbers them, with Windows' HDR state,
//! then grabs from one for two seconds: what Windows delivered, timings.
//!
//!     cargo run --release -p scw-capture --example capture_probe -- [monitor] [auto|off] [raw-out] [sdr-white]
//!
//! With `raw-out`, the last frame is written there as raw BGR bytes, for
//! comparing with the Python's capture. It is a screenshot: delete it.
//! `sdr-white` converts float frames at that level instead of Windows' own.
#[cfg(windows)]
fn main() {
    use scw_capture::windows::{DesktopSource, all_displays, monitors};
    use scw_capture::{HdrCapture, Source};
    use std::time::{Duration, Instant};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let monitor: usize = args
        .first()
        .map_or(1, |a| a.parse().expect("monitor number"));
    let hdr = HdrCapture::parse(args.get(1).map_or("auto", String::as_str)).expect("auto or off");
    for d in all_displays() {
        println!(
            "display {}: HDR supported {}, on {}, {} bits, SDR white {:.1} nits",
            d.device, d.supported, d.on, d.bits, d.sdr_white_nits
        );
    }
    for m in monitors().expect("monitors") {
        let rotated = if m.rotated { " (rotated)" } else { "" };
        println!(
            "monitor {}: {} {}x{}{rotated}",
            m.monitor, m.device, m.width, m.height
        );
    }
    let t = Instant::now();
    let mut src = DesktopSource::open(monitor, hdr).expect("open");
    if let Some(w) = args.get(3) {
        src.set_sdr_white(w.parse().expect("nits"));
    }
    println!("opened in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    let (mut got, mut none, mut grab_time) = (0usize, 0usize, Duration::ZERO);
    let mut order = String::new();
    let mut last: Option<scw_vision::Image<u8>> = None;
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        let t = Instant::now();
        match src.grab().expect("grab") {
            Some(f) => {
                grab_time += t.elapsed();
                got += 1;
                order.push(if src.converting() { 'F' } else { '8' });
                last = Some(f);
            }
            None => none += 1,
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("{}", src.describe());
    println!("frame formats in order (F float, 8 8-bit): {order}");
    println!(
        "2 s: {got} frames, {none} grabs with nothing new; {:.1} ms a frame ({:.1} ms converting)",
        grab_time.as_secs_f64() * 1000.0 / got.max(1) as f64,
        src.convert.as_secs_f64() * 1000.0 / src.frames.max(1) as f64
    );
    if let (Some(f), Some(path)) = (last, args.get(2)) {
        std::fs::write(path, &f.data).expect("write");
        println!("last frame {}x{} written", f.width, f.height);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the desktop capture is Windows-only so far");
}
