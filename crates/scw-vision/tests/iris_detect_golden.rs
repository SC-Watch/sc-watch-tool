//! The IRIS finder against IRIS's OWN output on real game frames - the
//! reference the Python port was held to (tools/iris_parity_detect.py), for
//! two scan areas, stage by stage:
//!
//!   capture   the scan geometry's capture area
//!   boxes     every text-line box, position and order
//!   pairs     the strict, crowd and loose pairs
//!   distance  which boxes read as a distance line (and the km)
//!   scan      the distance lines the scan keeps
//!   read      each line's read: class, reason, name, name rows
//!
//! Real frames show real players (and the user's screen), so this data never
//! enters the repository. SCW_PARITY_DATA/iris-detect holds `frames.txt` (one
//! frame per line; relative paths are taken from that folder) and `out/` (the
//! bench harness's det_{whole,app}_{dump,read}.json). Without it the test
//! reports that it skipped and passes.
//!
//! That set is IRIS 2.0 (with the PR #1 fixes), the Python's reference, so
//! it runs with IRIS 2.1's line join off. SCW_PARITY_DATA/iris-detect/
//! bandjoin/ holds the same files for the bench with 2.1's BandJoin patched
//! in and on, which the default settings are held to.

mod reading;

use scw_vision::iris::{DetectorSettings, ImageOps, ScanGeometry, boundary_from_percents};
use scw_vision::{Image, Rect};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn detect_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("SCW_PARITY_DATA")?).join("iris-detect");
    dir.join("frames.txt").is_file().then_some(dir)
}

fn read_bgr(path: &Path) -> Image<u8> {
    let mut dec = png::Decoder::new(std::fs::File::open(path).unwrap());
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let n = info.color_type.samples();
    let mut data = Vec::with_capacity(info.width as usize * info.height as usize * 3);
    for px in buf[..info.buffer_size()].chunks_exact(n) {
        if n < 3 {
            data.extend([px[0]; 3]);
        } else {
            data.extend([px[2], px[1], px[0]]);
        }
    }
    Image::from_vec(info.width as usize, info.height as usize, 3, data)
}

fn rect(v: &Value) -> Rect {
    let a: Vec<i32> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_i64().unwrap() as i32)
        .collect();
    Rect::new(a[0], a[1], a[2], a[3])
}

fn idx_pairs(v: &Value) -> Vec<(usize, usize)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p[0].as_u64().unwrap() as usize,
                p[1].as_u64().unwrap() as usize,
            )
        })
        .collect()
}

#[derive(Default)]
struct Tally {
    ok: BTreeMap<&'static str, usize>,
    bad: BTreeMap<&'static str, Vec<String>>,
}

impl Tally {
    fn judge(&mut self, stage: &'static str, r: Result<(), String>) -> bool {
        match r {
            Ok(()) => {
                *self.ok.entry(stage).or_default() += 1;
                true
            }
            Err(e) => {
                self.bad.entry(stage).or_default().push(e);
                false
            }
        }
    }
}

/// IRIS 2.0 and the PR #1 fixes - the Python's reference: no line join.
#[test]
fn iris_reference_detection() {
    let Some(dir) = detect_dir() else {
        eprintln!("skipped: SCW_PARITY_DATA is not set or holds no iris-detect/frames.txt");
        return;
    };
    let settings = DetectorSettings {
        band_join: false,
        ..DetectorSettings::default()
    };
    assert!(
        check_set(&dir, &settings),
        "the finder differs from IRIS's output"
    );
}

/// IRIS 2.1's line join (BandJoin), on as it is by default, against IRIS's
/// own output with it on.
#[test]
fn iris_reference_line_join() {
    let Some(dir) = detect_dir()
        .map(|d| d.join("bandjoin"))
        .filter(|d| d.join("frames.txt").is_file())
    else {
        eprintln!("skipped: SCW_PARITY_DATA holds no iris-detect/bandjoin/frames.txt");
        return;
    };
    assert!(
        check_set(&dir, &DetectorSettings::default()),
        "the line join differs from IRIS 2.1's output"
    );
}

/// One reference set: `dir`/frames.txt and `dir`/out/, both scan areas.
fn check_set(dir: &Path, settings: &DetectorSettings) -> bool {
    let frames: Vec<PathBuf> = std::fs::read_to_string(dir.join("frames.txt"))
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let p = PathBuf::from(l);
            if p.is_absolute() { p } else { dir.join(p) }
        })
        .collect();
    let runs = [("whole", (0, 0, 0, 0)), ("app", (15, 15, 8, 8))];
    let mut all_ok = true;
    for (run, (top, bottom, left, right)) in runs {
        let load = |kind: &str| -> Value {
            serde_json::from_slice(
                &std::fs::read(dir.join("out").join(format!("det_{run}_{kind}.json"))).unwrap(),
            )
            .unwrap()
        };
        let dump: HashMap<String, Value> = load("dump")
            .as_array()
            .unwrap()
            .iter()
            .map(|d| (d["frame"].as_str().unwrap().to_string(), d.clone()))
            .collect();
        let read: HashMap<String, Value> = load("read")["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["frame"].as_str().unwrap().to_string(), r.clone()))
            .collect();
        let mut t = Tally::default();
        let mut ops_cache: HashMap<(usize, usize), ImageOps> = HashMap::new();
        let (mut t_scan, mut t_read, mut n_lines) = (0.0, 0.0, 0usize);
        for path in &frames {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let img = read_bgr(path);
            let ops = ops_cache.entry((img.width, img.height)).or_insert_with(|| {
                let geo = ScanGeometry::create(
                    img.width as i32,
                    img.height as i32,
                    settings,
                    Some(boundary_from_percents(top, bottom, left, right)),
                )
                .unwrap();
                ImageOps::new(reading::row_reader(), geo)
            });
            let c = ops.geometry.capture;
            let g = &dump[&name];
            if !t.judge(
                "capture",
                (rect(&g["capture"]) == c)
                    .then_some(())
                    .ok_or(format!("{name}: {} vs {c:?}", g["capture"])),
            ) {
                continue;
            }
            let scan_img = ops.capture_of(&img);
            let cands = ops.scan_detector.find_candidates(&scan_img);
            let mine: Vec<Rect> = cands
                .boxes
                .iter()
                .map(|b| Rect::new(b.x + c.x, b.y + c.y, b.w, b.h))
                .collect();
            let gold: Vec<Rect> = g["boxes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| rect(&b["box"]))
                .collect();
            let boxes_ok = t.judge(
                "boxes",
                (mine == gold).then_some(()).ok_or_else(|| {
                    format!(
                        "{name}: {} vs {} boxes, {} of IRIS's missing",
                        gold.len(),
                        mine.len(),
                        gold.iter().filter(|b| !mine.contains(b)).count()
                    )
                }),
            );
            if boxes_ok {
                let crowd: Vec<(usize, usize)> = cands.crowd.iter().map(|c| (c.0, c.1)).collect();
                let same = cands.pairs == idx_pairs(&g["pairs"])
                    && crowd == idx_pairs(&g["crowd"])
                    && cands.loose == idx_pairs(&g["loose"]);
                t.judge(
                    "pairs",
                    same.then_some(()).ok_or_else(|| {
                        format!(
                            "{name}: strict {}/{} crowd {}/{} loose {}/{}",
                            g["pairs"].as_array().unwrap().len(),
                            cands.pairs.len(),
                            g["crowd"].as_array().unwrap().len(),
                            cands.crowd.len(),
                            g["loose"].as_array().unwrap().len(),
                            cands.loose.len()
                        )
                    }),
                );
                let bad = g["boxes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .zip(&cands.boxes)
                    .filter(|&(gb, &b)| {
                        let d = ops.rows.read_distance(&scan_img, b);
                        d.is_some() != gb["distance"].as_bool().unwrap()
                            || d.is_some_and(|d| (d.km() - gb["km"].as_f64().unwrap()).abs() > 1e-9)
                    })
                    .count();
                t.judge(
                    "distance",
                    (bad == 0)
                        .then_some(())
                        .ok_or(format!("{name}: {bad} boxes differ")),
                );
            }

            let started = Instant::now();
            let lines = ops.scan(&img);
            t_scan += started.elapsed().as_secs_f64();
            let gr = &read[&name];
            let greads = gr["reads"].as_array().unwrap();
            let glines: Vec<Rect> = greads.iter().map(|x| rect(&x["line"])).collect();
            if !t.judge(
                "scan",
                (lines == glines).then_some(()).ok_or(format!(
                    "{name}: {} vs {} lines",
                    glines.len(),
                    lines.len()
                )),
            ) {
                continue;
            }
            for (gx, &line) in greads.iter().zip(&lines) {
                let started = Instant::now();
                let r = ops.read_line(&img, line);
                t_read += started.elapsed().as_secs_f64();
                n_lines += 1;
                let want_rows: Option<Vec<Rect>> =
                    gx["rows"].as_array().map(|a| a.iter().map(rect).collect());
                let mine_rows = r.rows.clone().filter(|v| !v.is_empty());
                let gname = gx["name"].as_str().filter(|s| !s.is_empty());
                let verdict = if (r.class.as_str(), r.reason)
                    != (gx["cls"].as_str().unwrap(), gx["reason"].as_str().unwrap())
                {
                    Err(format!(
                        "{name} line {line:?}: {}/{} vs {}/{}",
                        gx["cls"],
                        gx["reason"],
                        r.class.as_str(),
                        r.reason
                    ))
                } else if gname != r.name.as_deref().filter(|s| !s.is_empty()) {
                    // Lengths only: a name is a real player's.
                    Err(format!(
                        "{name} line {line:?}: name lengths {} vs {}",
                        gname.map_or(0, str::len),
                        r.name.as_deref().map_or(0, str::len)
                    ))
                } else if want_rows != mine_rows {
                    Err(format!(
                        "{name} line {line:?}: rows {want_rows:?} vs {mine_rows:?}"
                    ))
                } else {
                    Ok(())
                };
                if t.judge("read", verdict) && r.name.is_some() {
                    *t.ok.entry("read (names)").or_default() += 1;
                }
            }
        }
        println!(
            "== scan area '{run}' ({top}, {bottom}, {left}, {right}): {} frames; scan {:.0} ms a frame, read {:.1} ms a line",
            frames.len(),
            1000.0 * t_scan / frames.len().max(1) as f64,
            1000.0 * t_read / n_lines.max(1) as f64
        );
        for (stage, n) in &t.ok {
            println!("   {stage:<14} ok {n}");
        }
        for (stage, list) in &t.bad {
            all_ok = false;
            println!("   {stage:<14} DIFFERS {}", list.len());
            for e in list.iter().take(4) {
                println!("      {e}");
            }
        }
    }
    all_ok
}
