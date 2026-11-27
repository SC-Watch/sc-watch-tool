//! Measures changes to the legacy reader's OCR step (PORT.md, OCR step 5):
//! each variant reads every frame's plates (`legacy::detect`, a full read)
//! and is scored three ways, printing counts and masked labels only - the
//! reads name real players.
//!
//!   key       test2 (in-game HDR, 30 frames): reads of each player the
//!             answer key names (H1, H2, ...), and player reads it does not
//!   vs base   frames whose players read exactly as the first variant's
//!   names     player reads of handles the first variant reads in 3 or
//!             more frames (likely real), and reads of handles this variant
//!             reads only once (likely misreads)
//!
//! and timed. Variants: `base` (RapidOCR's: detector at 736), `native`
//! (detector at the crop's own size, at most 960), `min480`, `min320`, each
//! with `+abc` (only characters a name or range holds), and `tuned` - what
//! the watcher runs, `legacy::Tuned`: plate crops at their own size, the
//! OCR's own lines at 736.
//!
//! With SCW_TUNE_FOCUS set it measures the focused read instead
//! (`legacy::detect_focused`, the centre crop to the OCR's own detector),
//! where `native` is the crop's own size: no resizing at all. With
//! SCW_TUNE_CHAT set it measures chat reads (`chat::read_located`, the
//! handles, upper-cased), `native` again meaning no resizing.
//!
//!   cargo run --release -p scw-ocr --example legacy_tune -- \
//!       <frames.txt> <test_consensus.py> [variant ...]

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use scw_ocr::chat;
use scw_ocr::legacy::{self, Options, Recognise};
use scw_ocr::{Limit, Ocr};
use scw_vision::Image;
use scw_vision::names::NameKind;

/// A handle's characters, a range's, and the space.
const NAME_CHARS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-. ";

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

/// The players test_consensus.py's answer key names (its docstring's
/// "NAME  xN" lines, the first - a ship serial - and UNKNOWN left out).
fn answer_key(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap();
    let doc = text.split("\"\"\"").nth(1).unwrap_or("");
    let mut names = Vec::new();
    for line in doc.lines() {
        if !line.starts_with(char::is_whitespace) {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(name), Some(count)) = (parts.next(), parts.next()) else {
            continue;
        };
        let ok_name = name.len() >= 4
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-');
        let ok_count = count.len() > 1
            && count.starts_with('x')
            && count[1..].chars().all(|c| c.is_ascii_digit());
        if ok_name && ok_count {
            names.push(name.to_string());
        }
    }
    names
        .into_iter()
        .skip(1)
        .filter(|n| n != "UNKNOWN")
        .collect()
}

fn engine(variant: &str, threads: usize, focus: bool) -> Ocr {
    let (size, abc) = match variant.strip_suffix("+abc") {
        Some(s) => (s, true),
        None => (variant, false),
    };
    let mut ocr = Ocr::embedded().unwrap().with_threads(threads);
    ocr.det_settings.limit = match size {
        "base" | "tuned" => Limit::Min(736),
        "native" if focus => Limit::Max(4096),
        "native" => Limit::Max(960),
        "min480" => Limit::Min(480),
        "min320" => Limit::Min(320),
        "min520" => Limit::Min(520),
        other => panic!("no variant {other}"),
    };
    if abc { ocr.only_chars(NAME_CHARS) } else { ocr }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let frames: Vec<String> = std::fs::read_to_string(&a[1])
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let key = answer_key(Path::new(&a[2]));
    let variants: Vec<String> = if a.len() > 3 {
        a[3..].to_vec()
    } else {
        ["base", "native", "min480", "base+abc", "native+abc"]
            .map(String::from)
            .to_vec()
    };
    let threads = std::thread::available_parallelism()
        .map_or(2, |n| n.get() / 2)
        .max(2);
    let chat_mode = std::env::var_os("SCW_TUNE_CHAT").is_some();
    let focus = chat_mode || std::env::var_os("SCW_TUNE_FOCUS").is_some();
    let engines: Vec<Ocr> = variants.iter().map(|v| engine(v, threads, focus)).collect();
    let opts = Options::default();
    // reads[v][f]: the player names variant v read in frame f
    let mut reads: Vec<Vec<HashSet<String>>> = vec![Vec::new(); variants.len()];
    let mut ms: Vec<Vec<f64>> = vec![Vec::new(); variants.len()];
    let mut test2 = Vec::new();
    let t0 = Instant::now();
    for (fi, path) in frames.iter().enumerate() {
        let frame = read_bgr(Path::new(path));
        test2.push(
            Path::new(path)
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("test2_")),
        );
        for (vi, e) in engines.iter().enumerate() {
            let mut plain: &Ocr = e;
            let mut tuned = legacy::Tuned::new(e, Limit::Min(736));
            let r: &mut dyn Recognise = if variants[vi] == "tuned" {
                &mut tuned
            } else {
                &mut plain
            };
            let t = Instant::now();
            if chat_mode {
                let (lines, _) = chat::read_located(&frame, r, None).unwrap();
                ms[vi].push(t.elapsed().as_secs_f64() * 1e3);
                reads[vi].push(lines.iter().map(|c| c.key()).collect());
                continue;
            }
            let found = if focus {
                legacy::detect_focused(&frame, r, None, true, &opts)
            } else {
                legacy::detect(&frame, r, frame.height, legacy::MAX_OCR_PAIRS, &opts)
            }
            .unwrap();
            ms[vi].push(t.elapsed().as_secs_f64() * 1e3);
            reads[vi].push(
                found
                    .into_iter()
                    .filter(|c| c.kind == NameKind::Player && c.range_km.is_some())
                    .filter_map(|c| c.name)
                    .collect(),
            );
        }
        if (fi + 1) % 25 == 0 {
            eprintln!(
                "{}/{} frames, {:.0} s",
                fi + 1,
                frames.len(),
                t0.elapsed().as_secs_f64()
            );
        }
    }

    // How many frames the first variant reads each handle in.
    let mut base_freq: HashMap<&str, usize> = HashMap::new();
    for f in &reads[0] {
        for n in f {
            *base_freq.entry(n).or_default() += 1;
        }
    }
    let n_test2 = test2.iter().filter(|t| **t).count();
    println!(
        "{} frames ({n_test2} of test2, {} players in its key); {threads} OCR threads; {} read",
        frames.len(),
        key.len(),
        if chat_mode {
            "chat"
        } else if focus {
            "focused"
        } else {
            "full"
        }
    );
    for (vi, v) in variants.iter().enumerate() {
        let mut freq: HashMap<&str, usize> = HashMap::new();
        for f in &reads[vi] {
            for n in f {
                *freq.entry(n).or_default() += 1;
            }
        }
        let total: usize = reads[vi].iter().map(HashSet::len).sum();
        let same = reads[vi]
            .iter()
            .zip(&reads[0])
            .filter(|(a, b)| a == b)
            .count();
        let frequent: usize = reads[vi]
            .iter()
            .flatten()
            .filter(|n| base_freq.get(n.as_str()).copied().unwrap_or(0) >= 3)
            .count();
        let once = freq.values().filter(|c| **c == 1).count();
        let (mut hits, mut other) = (vec![0usize; key.len()], 0usize);
        for (f, is_t2) in reads[vi].iter().zip(&test2) {
            if !*is_t2 {
                continue;
            }
            for n in f {
                match key.iter().position(|k| k == n) {
                    Some(i) => hits[i] += 1,
                    None => other += 1,
                }
            }
        }
        let mut sorted = ms[vi].clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = sorted[sorted.len() / 2];
        let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
        let key_text: Vec<String> = hits
            .iter()
            .enumerate()
            .map(|(i, h)| format!("H{} {h:>2}", i + 1))
            .collect();
        println!(
            "  {v:<11} key {}  other {other:>2} | same as {} {same:>3}/{} | reads {total:>4}, of frequent handles {frequent:>4}, handles read once {once:>3} | {median:>5.0} ms median, {mean:>5.0} mean",
            key_text.join(" "),
            variants[0],
            frames.len()
        );
    }
}
