//! Where a frame scan's time goes: the image functions the finder runs over
//! the whole capture area, timed on one local reference frame
//! (SCW_PARITY_DATA/iris-detect). Prints timings only.
use scw_vision::{Image, cv};
use std::path::PathBuf;
use std::time::Instant;

fn read_bgr(path: &std::path::Path) -> Image<u8> {
    let mut dec = png::Decoder::new(std::fs::File::open(path).unwrap());
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let n = info.color_type.samples();
    let mut data = Vec::new();
    for px in buf[..info.buffer_size()].chunks_exact(n) {
        if n < 3 {
            data.extend([px[0]; 3])
        } else {
            data.extend([px[2], px[1], px[0]])
        }
    }
    Image::from_vec(info.width as usize, info.height as usize, 3, data)
}

fn time<T>(label: &str, f: impl Fn() -> T) -> T {
    let t = Instant::now();
    let mut out = f();
    for _ in 0..4 {
        out = f();
    }
    println!(
        "  {label:<28} {:>7.2} ms",
        t.elapsed().as_secs_f64() * 1000.0 / 5.0
    );
    out
}

fn main() {
    let dir = PathBuf::from(std::env::var_os("SCW_PARITY_DATA").expect("SCW_PARITY_DATA"))
        .join("iris-detect");
    let first = std::fs::read_to_string(dir.join("frames.txt"))
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_string();
    let img = read_bgr(&PathBuf::from(first));
    let (w, h) = (img.width * 7 / 10, img.height * 6 / 10);
    let region = img.crop(img.width * 15 / 100, img.height / 10, w, h);
    println!("region {w}x{h}");
    let value = time("channels + max", || {
        let (b, g, r) = (region.channel(0), region.channel(1), region.channel(2));
        Image::from_vec(
            w,
            h,
            1,
            (0..w * h)
                .map(|i| b.data[i].max(g.data[i]).max(r.data[i]))
                .collect(),
        )
    });
    let small = time("area resize /4", || {
        cv::resize_area_to(&value, w.div_ceil(4), h.div_ceil(4))
    });
    let blur = time("blur 15", || cv::blur(&small, 15));
    time("linear resize x4", || cv::resize_linear_to(&blur, w, h));
    let thr = time("threshold", || cv::threshold_binary(&value, 150.0, 255.0));
    time("subtract", || cv::subtract(&value, &thr));
    time("erode 11x11", || cv::erode(&value, 11, 11));
    time("tophat 11x11", || cv::tophat(&value, 11, 11));
    let mask = cv::threshold_binary(&cv::tophat(&value, 11, 11), 28.0, 255.0);
    println!(
        "  (mask ink {:.1}%)",
        100.0 * cv::count_nonzero(&mask) as f64 / (w * h) as f64
    );
    time("components 8 (mask)", || cv::connected_components(&mask, 8));
    let joined = time("dilate 7x1", || cv::dilate(&mask, 7, 1));
    time("components 8 (joined)", || {
        cv::connected_components(&joined, 8)
    });
    time("bounding rect", || cv::bounding_rect(&mask));

    use scw_vision::iris::{
        DetectorSettings, ImageOps, LetterModel, NameplateReader, ReaderSettings, RowReader,
        ScanGeometry, boundary_from_percents,
    };
    let model =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models/iris-letters-model.bin");
    let reader = || {
        RowReader::new(
            NameplateReader::new(LetterModel::from_bytes(&std::fs::read(&model).unwrap()).unwrap())
                .unwrap(),
            ReaderSettings::default(),
        )
    };
    let geo = ScanGeometry::create(
        img.width as i32,
        img.height as i32,
        &DetectorSettings::default(),
        Some(boundary_from_percents(15, 15, 8, 8)),
    )
    .unwrap();
    let ops = ImageOps::new(reader(), geo);
    // The same measure as the Python bench: ops.scan, best of 3 a frame,
    // the first 40 reference frames.
    let frames: Vec<Image<u8>> = std::fs::read_to_string(dir.join("frames.txt"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(40)
        .map(|l| read_bgr(&PathBuf::from(l.trim())))
        .collect();
    let mut total = 0.0;
    for frame in &frames {
        let mut best = f64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            std::hint::black_box(ops.scan(frame));
            best = best.min(t.elapsed().as_secs_f64());
        }
        total += best;
    }
    println!(
        "scan {:.1} ms a frame ({} frames at {}x{}, best of 3)",
        1000.0 * total / frames.len() as f64,
        frames.len(),
        img.width,
        img.height
    );
}
