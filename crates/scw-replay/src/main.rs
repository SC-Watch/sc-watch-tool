//! scw-replay: saved frames through sc-watch's Rust reading pipeline - IRIS's
//! finder and reader, the name classifier and the burst vote - as the
//! watcher reads them with `--reader iris`. For checking the port against
//! the Python on real frames, and for trying settings without the game.
//!
//! Bursts are consecutive frames, `--burst` at a time. The watcher's own
//! trigger (when to start a burst) is not modelled: the replay starts one
//! every `--burst` frames.

mod frames;

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use scw_vision::iris::{DetectorSettings, LetterModel, NameplateReader, ReaderSettings, RowReader};
use scw_vision::names::vote::Resolved;
use scw_vision::pipeline::{Burst, FrameRead, FrameReader, ScanArea};
use serde_json::{Value, json};

/// IRIS's letter model (models/iris-letters-model.bin, GPL-3.0), built in.
const MODEL: &[u8] = include_bytes!("../../../models/iris-letters-model.bin");

const USAGE: &str = "\
scw-replay - saved frames through sc-watch's Rust reading pipeline

usage: scw-replay [options] <frames>...

  <frames>          PNG files, folders (their .png files), or patterns with
                    * and ? in the file name; each sorted, in argument order
  --frames-list F   frame paths from F, one per line (relative ones from F's
                    folder), in that order
  --scan-area A     whole | narrow | iris | centre        (default whole)
  --burst N         frames per burst; 0 = all one burst     (default 3)
  --min-votes N     votes a contact needs to be reported    (default 1)
  --listed F        handles, one per line, that are people even when they
                    read as a world label (your own lists)
  --model F         the IRIS letter model                   (default: built in)
  --as-python       read exactly as sc-watch's Python does: without the
                    port's deliberate improvements (IRIS 2.1's line join,
                    the damaged-serial rule) - for checking the port
  --json            one JSON document on stdout instead of text
  -h, --help        this text
";

struct Options {
    frames: Vec<PathBuf>,
    area: ScanArea,
    burst: usize,
    min_votes: usize,
    listed: HashSet<String>,
    model: Option<PathBuf>,
    json: bool,
    as_python: bool,
}

fn parse_args() -> Result<Options, String> {
    let mut o = Options {
        frames: Vec::new(),
        area: ScanArea::Whole,
        burst: 3,
        min_votes: 1,
        listed: HashSet::new(),
        model: None,
        json: false,
        as_python: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{a} needs {what}"));
        match a.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--json" => o.json = true,
            "--as-python" => o.as_python = true,
            "--scan-area" => {
                let v = value("an area")?;
                o.area = ScanArea::parse(&v).ok_or(format!("unknown scan area {v:?}"))?;
            }
            "--burst" => {
                o.burst = value("a number")?
                    .parse()
                    .map_err(|_| "--burst needs a number")?
            }
            "--min-votes" => {
                o.min_votes = value("a number")?
                    .parse()
                    .map_err(|_| "--min-votes needs a number")?
            }
            "--listed" => {
                let f = value("a file")?;
                let text = std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?;
                o.listed.extend(
                    text.lines()
                        .map(|l| l.trim().to_uppercase())
                        .filter(|l| !l.is_empty()),
                );
            }
            "--model" => o.model = Some(PathBuf::from(value("a file")?)),
            "--frames-list" => {
                let f = PathBuf::from(value("a file")?);
                o.frames.extend(frames::from_list(&f)?);
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => o.frames.extend(frames::expand(s)?),
        }
    }
    if o.frames.is_empty() {
        return Err("no frames given".into());
    }
    Ok(o)
}

fn row_reader(model: Option<&PathBuf>) -> Result<RowReader, String> {
    let bytes = match model {
        Some(p) => std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => MODEL.to_vec(),
    };
    let model = LetterModel::from_bytes(&bytes).map_err(|e| e.to_string())?;
    Ok(RowReader::new(
        NameplateReader::new(model)?,
        ReaderSettings::default(),
    ))
}

fn resolved_json(r: &[Resolved]) -> Value {
    r.iter()
        .map(|c| {
            json!({"name": c.name, "km": c.range_km, "votes": c.votes, "total": c.total,
                   "variants": c.variants.iter().map(|(n, k)| json!([n, k])).collect::<Vec<_>>()})
        })
        .collect()
}

fn frame_json(file: &str, f: &FrameRead) -> Value {
    let rect = |r: &scw_vision::Rect| json!([r.x, r.y, r.w, r.h]);
    json!({
        "file": file,
        "lines": f.lines,
        "contacts": f.contacts.iter().map(|c| json!({
            "name": c.name, "kind": c.kind.as_str(), "km": c.range_km,
            "name_box": rect(&c.name_box), "range_box": rect(&c.range_box),
        })).collect::<Vec<_>>(),
        "refused": f.refused.iter().map(|(k, n)| json!([k, n])).collect::<Vec<_>>(),
    })
}

fn run(o: Options) -> Result<(), String> {
    let mut reader = FrameReader::new(
        row_reader(o.model.as_ref())?,
        o.area,
        DetectorSettings::default(),
    );
    if o.as_python {
        reader = reader.as_python();
    }
    let listed = |n: &str| o.listed.contains(&n.to_uppercase());
    let per_burst = if o.burst == 0 {
        o.frames.len()
    } else {
        o.burst
    };
    let (mut frames_out, mut bursts_out) = (Vec::new(), Vec::new());
    let (mut total_ms, mut read_frames) = (0.0, 0usize);
    for (b, chunk) in o.frames.chunks(per_burst).enumerate() {
        let mut burst = Burst::default();
        let first = b * per_burst;
        if !o.json {
            println!(
                "burst {} - frames {}-{}",
                b + 1,
                first + 1,
                first + chunk.len()
            );
        }
        for (k, path) in chunk.iter().enumerate() {
            let file = path
                .file_name()
                .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
            let Some(img) = frames::load(path)? else {
                return Err(format!(
                    "{}: not a frame this tool can read (8-bit PNG)",
                    path.display()
                ));
            };
            let t = Instant::now();
            let read = reader.read(&img);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            total_ms += ms;
            read_frames += 1;
            burst.tally(&read.contacts, &listed);
            if o.json {
                frames_out.push(frame_json(&file, &read));
            } else {
                println!(
                    "  frame {:>3}  {file}  {} lines, {ms:.0} ms",
                    first + k + 1,
                    read.lines
                );
                for c in &read.contacts {
                    let km = c
                        .range_km
                        .map_or("   - km".into(), |v| format!("{v:>5.1} km"));
                    println!(
                        "      {:<24} {km}  {}",
                        c.name.as_deref().unwrap_or("?"),
                        c.kind.as_str()
                    );
                }
                if !read.refused.is_empty() {
                    let r: Vec<String> = read
                        .refused
                        .iter()
                        .map(|(k, n)| format!("{k} x{n}"))
                        .collect();
                    println!("      refused: {}", r.join(", "));
                }
            }
        }
        let resolved = burst.resolve(o.min_votes);
        if o.json {
            bursts_out.push(json!({
                "frames": (first..first + chunk.len()).collect::<Vec<_>>(),
                "unreadable": burst.unreadable,
                "discarded": burst.discarded.iter().map(|(k, n)| json!([k.as_str(), n])).collect::<Vec<_>>(),
                "per_frame": burst.per_frame,
                "resolved": resolved_json(&resolved),
            }));
        } else {
            if resolved.is_empty() {
                println!("  => no players");
            }
            for r in &resolved {
                let also: Vec<String> = r
                    .variants
                    .iter()
                    .map(|(n, c)| format!("{n} x{c}"))
                    .collect();
                let also = if also.is_empty() {
                    String::new()
                } else {
                    format!("  (also read: {})", also.join(", "))
                };
                println!(
                    "  => {:<24} {:>5.1} km  {}/{} votes{also}",
                    r.name, r.range_km, r.votes, r.total
                );
            }
            let skipped: Vec<String> = burst
                .discarded
                .iter()
                .map(|(k, n)| format!("{} x{n}", k.as_str()))
                .collect();
            if !skipped.is_empty() || burst.unreadable > 0 {
                println!(
                    "     not people: {}; unreadable: {}",
                    if skipped.is_empty() {
                        "none".into()
                    } else {
                        skipped.join(", ")
                    },
                    burst.unreadable
                );
            }
        }
    }
    if o.json {
        let doc = json!({"scan_area": o.area.name(), "burst": o.burst, "min_votes": o.min_votes,
                         "frames": frames_out, "bursts": bursts_out});
        println!("{doc}");
    } else {
        println!(
            "{read_frames} frames, {:.0} ms a frame (scan area '{}')",
            total_ms / read_frames.max(1) as f64,
            o.area.name()
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    match parse_args() {
        Err(e) if e.is_empty() => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("scw-replay: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Ok(o) => match run(o) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("scw-replay: {e}");
                ExitCode::FAILURE
            }
        },
    }
}
