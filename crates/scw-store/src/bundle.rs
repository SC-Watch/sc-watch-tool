//! The support bundle - debuglog.build_bundle: one zip of everything a
//! maintainer needs and nothing they do not, small enough to send (free
//! chat uploads and most mail stop at 20-25 MB).
//!
//! What goes in: the newest session logs, the settings with secrets
//! replaced, the report flags, and - unless it is text only - the newest
//! screenshots from debug_bursts/ and audit/ within the budget, crops first
//! (they are tiny, and they are the evidence for a misread). Crops and
//! JPEGs go in untouched; full PNG frames are re-encoded as JPEG at their
//! own size, six times smaller with no geometry lost. A MANIFEST.txt says
//! in plain words what is inside, including that a full frame is the whole
//! screen.
//!
//! What never goes in, by construction: the databases (reputation reports
//! about named players, imported lists, cached profiles), sightings.csv and
//! the API key. Nothing reads them here.
//!
//! The re-encoded frames are jpeg-encoder's JPEG, not OpenCV's, so their
//! bytes differ from the Python's; everything else is the same
//! (tests/bundle_ops.rs).

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use chrono::{Datelike, NaiveDateTime, Timelike};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::flags::Flags;
use crate::json::dumps_indent;

/// Where frames and session logs live.
pub const DEBUG_DIR: &str = "debug_bursts";
/// How many session logs a bundle (and the log rotation) keeps.
pub const KEEP_SESSIONS: usize = 12;
/// The size a bundle is kept under, in MB.
pub const BUNDLE_MB: f64 = 24.0;
/// The JPEG quality full frames are re-encoded at.
pub const BUNDLE_JPEG_QUALITY: u8 = 88;
/// Names that never enter a bundle, wherever they are found.
pub const NEVER_BUNDLE: [&str; 7] = [
    "sc-watch.db",
    "sc-watch.db-wal",
    "sc-watch.db-shm",
    "watchlist.db",
    "rsi_cache.db",
    "sightings.csv",
    "sc_api_key.txt",
];

/// Anything shaped like a secret: `api_key=...`, `token: ...`.
pub fn secret_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)(api[_-]?key|token|secret|password)\s*[:=]\s*\S+").unwrap())
}

/// The settings as a log or bundle shows them: anything secret replaced.
/// The read key is called `key`, which the rule cannot tell from an API
/// key, so it is replaced too (as debuglog.safe_settings does).
pub fn safe_settings(settings: &Map<String, Value>) -> Map<String, Value> {
    settings
        .iter()
        .map(|(k, v)| {
            let lk = k.to_lowercase();
            // re.match: a match that starts at the first character.
            let secret = secret_re()
                .find(&format!("{k}=x"))
                .is_some_and(|m| m.start() == 0)
                || (lk.contains("key") && !lk.contains("_key"));
            (
                k.clone(),
                if secret {
                    Value::String("<redacted>".into())
                } else {
                    v.clone()
                },
            )
        })
        .collect()
}

/// How a bundle is made.
#[derive(Clone, Debug)]
pub struct Options {
    /// Screenshots in, or text only.
    pub with_frames: bool,
    /// The size budget for the screenshots, in MB.
    pub budget_mb: f64,
    /// Where to write it; else debug_bursts/sc-watch-support-<time>.zip.
    pub dest: Option<PathBuf>,
    /// The command that makes a text-only bundle, as the manifest names it.
    pub text_only_command: String,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            with_frames: true,
            budget_mb: BUNDLE_MB,
            dest: None,
            text_only_command: if cfg!(windows) {
                r".\sc-watch.exe --support-bundle --no-frames"
            } else {
                "./sc-watch --support-bundle --no-frames"
            }
            .into(),
        }
    }
}

/// A lossless OCR or audit crop rather than a full screen grab.
fn is_crop(name: &str) -> bool {
    name.contains("_crop_")
}

/// `*.<ext>`, case-blind on Windows as pathlib's glob is.
fn has_ext(name: &str, ext: &str) -> bool {
    let tail = format!(".{ext}");
    if cfg!(windows) {
        name.to_lowercase().ends_with(&tail)
    } else {
        name.ends_with(&tail)
    }
}

fn is_image(name: &str) -> bool {
    ["png", "jpg", "jpeg"].iter().any(|e| has_ext(name, e))
}

/// `<head>*<tail>`, case-blind on Windows.
fn glob(name: &str, head: &str, tail: &str) -> bool {
    let n = if cfg!(windows) {
        name.to_lowercase()
    } else {
        name.to_string()
    };
    n.len() >= head.len() + tail.len() && n.starts_with(head) && n.ends_with(tail)
}

/// `session_*.log`.
fn is_session_log(name: &str) -> bool {
    glob(name, "session_", ".log")
}

/// `sc-watch-support-*.zip`.
fn is_old_bundle(name: &str) -> bool {
    glob(name, "sc-watch-support-", ".zip")
}

/// The files in `dir` whose names pass `keep`, with their times; nothing
/// when the folder cannot be read.
fn listed(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<(SystemTime, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| keep(&e.file_name().to_string_lossy()))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, e.path()))
        })
        .collect()
}

fn newest_first(mut v: Vec<(SystemTime, PathBuf)>) -> Vec<PathBuf> {
    v.sort_by_key(|e| std::cmp::Reverse(e.0));
    v.into_iter().map(|(_, p)| p).collect()
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A PNG, as 8-bit RGB with its size (alpha dropped, grey made colour, as
/// OpenCV's IMREAD_COLOR reads it); None when it is not one.
fn read_png(bytes: &[u8]) -> Option<(Vec<u8>, u16, u16)> {
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let px = &buf[..info.buffer_size()];
    let n = (info.width as usize) * (info.height as usize);
    let rgb: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => px.to_vec(),
        png::ColorType::Rgba => px
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g]).collect(),
        png::ColorType::GrayscaleAlpha => px
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0]])
            .collect(),
        png::ColorType::Indexed => return None,
    };
    if rgb.len() != n * 3 {
        return None;
    }
    Some((
        rgb,
        u16::try_from(info.width).ok()?,
        u16::try_from(info.height).ok()?,
    ))
}

/// The bytes to put in the zip for an image, and its name there: crops and
/// JPEGs untouched; a full PNG frame as JPEG at its own size; anything that
/// will not decode, as it is.
fn encoded(path: &Path) -> std::io::Result<(Vec<u8>, String)> {
    let bytes = std::fs::read(path)?;
    let name = name_of(path);
    if is_crop(&name) || has_ext(&name, "jpg") || has_ext(&name, "jpeg") {
        return Ok((bytes, name));
    }
    let Some((rgb, w, h)) = read_png(&bytes) else {
        return Ok((bytes, name));
    };
    let mut out = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut out, BUNDLE_JPEG_QUALITY);
    if enc
        .encode(&rgb, w, h, jpeg_encoder::ColorType::Rgb)
        .is_err()
    {
        return Ok((bytes, name));
    }
    let stem = Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(name);
    Ok((out, format!("{stem}.jpg")))
}

/// (source, name in the zip, bytes) for every screenshot that fits the
/// budget: crops first, then newest first.
fn frame_sets(root: &Path, opts: &Options) -> Vec<(PathBuf, String, Vec<u8>)> {
    let mut out = Vec::new();
    if !opts.with_frames {
        return out;
    }
    let mut picks: Vec<(SystemTime, PathBuf, &str)> = Vec::new();
    for sub in [DEBUG_DIR, "audit"] {
        for (t, p) in listed(&root.join(sub), is_image) {
            picks.push((t, p, sub));
        }
    }
    picks.sort_by(|a, b| {
        let crop = |p: &Path| !is_crop(&name_of(p));
        crop(&a.1).cmp(&crop(&b.1)).then(b.0.cmp(&a.0))
    });
    let mut budget = opts.budget_mb * 1e6;
    for (_, path, sub) in picks {
        let Ok((data, name)) = encoded(&path) else {
            continue;
        };
        if data.len() as f64 > budget {
            continue;
        }
        budget -= data.len() as f64;
        out.push((path, format!("{sub}/{name}"), data));
    }
    out
}

fn manifest(
    now: NaiveDateTime,
    logs: usize,
    images: usize,
    available: usize,
    full: usize,
    crops: usize,
    opts: &Options,
) -> String {
    let mut m = vec![
        "sc-watch support bundle".to_string(),
        format!("created  {}", now.format("%Y-%m-%d %H:%M:%S")),
        String::new(),
        "WHAT IS IN HERE".into(),
        format!("  session logs     {logs} - your settings, and what the watcher"),
        "                     read from each frame it looked at".into(),
        "  settings.json    your in-tool settings, secrets replaced".into(),
        "  flags.json       your report reasons, colours and polarity".into(),
    ];
    if opts.with_frames {
        m.extend([
            format!(
                "  screenshots      {images} of {available} available ({full} full frames, {crops} OCR crops)"
            ),
            String::new(),
            "READ THIS BEFORE YOU SEND IT".into(),
            "  A full frame is your WHOLE screen at the moment of a read. If".into(),
            "  you had a QR code, an account page, a second monitor or a chat".into(),
            "  overlay visible, it is in these images. Open a few and look.".into(),
            "  For a text-only bundle: press 'text only, no screenshots'".into(),
            format!("  in Settings, or run  {}", opts.text_only_command),
        ]);
    } else {
        m.push("  screenshots      NONE - built with --no-frames".into());
    }
    m.extend(
        [
            "",
            "WHAT IS DELIBERATELY NOT IN HERE",
            "  sc-watch.db      your reputation reports about named real players",
            "  watchlist.db     third-party lists you imported",
            "  rsi_cache.db     cached profiles",
            "  sightings.csv    your encounter log",
            "  sc_api_key.txt   your API key. The logs record only whether a key",
            "                   is set and how many characters it is.",
            "",
            "  Home directory paths and your Windows username are rewritten in",
            "  the logs before they are written to disk.",
        ]
        .map(String::from),
    );
    m.join("\n")
}

/// `json.dumps(v, indent=2, sort_keys=True)`: every object's keys sorted.
fn sorted(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), sorted(&m[k])))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        _ => v.clone(),
    }
}

/// Python's round(x, 1) for the size in MB (halves to even, on the float).
fn round1(x: f64) -> f64 {
    let y = x * 10.0;
    let r = y.round();
    let r = if (y - y.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
        r - y.signum()
    } else {
        r
    };
    r / 10.0
}

/// Builds the bundle for the data folder `root` at `now`, from the
/// `settings` and `flags` as loaded. Returns what went in, in the Python's
/// keys and order - also what the page shows and the manifest inside says.
pub fn build(
    root: &Path,
    settings: &Map<String, Value>,
    flags: &Flags,
    now: NaiveDateTime,
    opts: &Options,
) -> Map<String, Value> {
    let d = root.join(DEBUG_DIR);
    let _ = std::fs::create_dir_all(&d);
    let dest = opts.dest.clone().unwrap_or_else(|| {
        d.join(format!(
            "sc-watch-support-{}.zip",
            now.format("%Y%m%d_%H%M%S")
        ))
    });

    // Old bundles are ~20 MB each and housekeeping only sweeps images, so
    // they would sit in the debug folder forever. Keep the two most recent.
    for stale in newest_first(listed(&d, is_old_bundle)).into_iter().skip(2) {
        let _ = std::fs::remove_file(stale);
    }

    let logs: Vec<PathBuf> = newest_first(listed(&d, is_session_log))
        .into_iter()
        .take(KEEP_SESSIONS)
        .collect();
    let images = frame_sets(root, opts);
    let available = if opts.with_frames {
        [DEBUG_DIR, "audit"]
            .iter()
            .map(|sub| listed(&root.join(sub), is_image).len())
            .sum()
    } else {
        0
    };
    let full = images
        .iter()
        .filter(|(p, _, _)| !is_crop(&name_of(p)))
        .count();
    let crops = images.len() - full;
    let text = manifest(now, logs.len(), images.len(), available, full, crops, opts);

    let mut info = Map::new();
    info.insert("file".into(), json!(dest.display().to_string()));
    info.insert("logs".into(), json!(logs.len()));
    info.insert("images".into(), json!(images.len()));
    info.insert("images_available".into(), json!(available));
    info.insert("full_frames".into(), json!(full));
    info.insert("crops".into(), json!(crops));
    info.insert("with_frames".into(), json!(opts.with_frames));

    let written = (|| -> Result<(), String> {
        let when = zip::DateTime::from_date_and_time(
            now.year() as u16,
            now.month() as u8,
            now.day() as u8,
            now.hour() as u8,
            now.minute() as u8,
            now.second() as u8,
        )
        .unwrap_or_default();
        let zopts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(when);
        let file = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
        let mut z = zip::ZipWriter::new(file);
        let mut put = |name: &str, data: &[u8]| -> Result<(), String> {
            z.start_file(name, zopts).map_err(|e| e.to_string())?;
            z.write_all(data).map_err(|e| e.to_string())
        };
        put("MANIFEST.txt", format!("{text}\n").as_bytes())?;
        for lg in &logs {
            let data = std::fs::read(lg).map_err(|e| e.to_string())?;
            put(&format!("logs/{}", name_of(lg)), &data)?;
        }
        let safe = Value::Object(safe_settings(settings));
        put("settings.json", dumps_indent(&sorted(&safe), 2)?.as_bytes())?;
        // Report reasons are settings too, and a custom polarity is exactly
        // the kind of thing that explains "why is this person not alerting".
        let all = Value::Array(flags.all.iter().map(|f| f.to_json()).collect());
        put("flags.json", dumps_indent(&all, 2)?.as_bytes())?;
        for (src, name, data) in &images {
            if NEVER_BUNDLE.contains(&name_of(src).as_str()) {
                continue;
            }
            put(name, data)?;
        }
        z.finish().map_err(|e| e.to_string())?;
        Ok(())
    })();
    match written.and_then(|()| std::fs::metadata(&dest).map_err(|e| e.to_string())) {
        Ok(m) => {
            info.insert("bytes".into(), json!(m.len()));
            info.insert("mb".into(), json!(round1(m.len() as f64 / 1e6)));
            info.insert("ok".into(), json!(true));
        }
        Err(e) => {
            info.insert("ok".into(), json!(false));
            info.insert("error".into(), json!(e));
        }
    }
    info.insert("manifest".into(), json!(text));
    info
}

/// What `--support-bundle` prints (watch.py's lines after the manifest).
pub fn report(info: &Map<String, Value>, budget_mb: f64) -> (String, bool) {
    let text = |k: &str| {
        info.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let num = |k: &str| info.get(k).and_then(Value::as_u64).unwrap_or(0);
    let mut out = format!("{}\n\n", text("manifest"));
    if info.get("ok") != Some(&Value::Bool(true)) {
        out.push_str(&format!("  FAILED: {}", text("error")));
        return (out, false);
    }
    let mb = info.get("mb").cloned().unwrap_or(json!(0));
    out.push_str(&format!("  written: {}\n", text("file")));
    out.push_str(&format!("  size:    {} MB", crate::json::py_str(&mb)));
    if num("images") < num("images_available") {
        out.push_str(&format!(
            "\n  note:    {} of {} screenshots included (newest first, {:.0} MB budget)",
            num("images"),
            num("images_available"),
            budget_mb
        ));
    }
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sorted_into_kinds() {
        assert!(is_crop("20261124_EXMPLHANDLE_crop_name.png"));
        assert!(!is_crop("burst_210000_0.png"));
        assert!(is_session_log("session_20261124_120000.log"));
        assert!(is_session_log("session_.log"));
        assert!(!is_session_log("session.log"));
        assert!(!is_session_log("sentry_requests.log"));
        assert!(is_old_bundle("sc-watch-support-20261124_210509.zip"));
        assert!(!is_old_bundle("sc-watch-share-x.zip"));
        assert!(is_image("a.png") && is_image("b.jpg") && is_image("c.jpeg"));
        assert!(!is_image("notes.txt") && !is_image(".pngx"));
    }

    #[test]
    fn sizes_round_as_python_does() {
        assert_eq!(round1(1.25), 1.2);
        assert_eq!(round1(1.35), 1.4);
        assert_eq!(round1(0.0), 0.0);
        assert_eq!(round1(23.97), 24.0);
    }
}
