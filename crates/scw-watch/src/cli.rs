//! The watcher's command line - watch.build_parser and parse_args: the
//! saved settings are the defaults, and what is typed overrides them.
//!
//! Parsed as Python's argparse (3.13) parses it, because the UI and people's
//! shortcuts launch the watcher with argparse's rules in mind: `--flag value`
//! and `--flag=value`, a unique abbreviation for any long option, the same
//! number rules as Python's int() and float(), and the same error messages
//! (exit code 2). The help text is this program's own.

use std::path::Path;

use serde_json::{Map, Value, json};

use scw_store::json::{py_float_text, py_int_text, py_repr_str};

/// What an option takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Str,
    Float,
    Int,
    True,
    False,
    /// One or more values.
    Many,
}

struct Opt {
    flag: &'static str,
    dest: &'static str,
    kind: Kind,
    choices: &'static [&'static str],
    /// None: hidden from the help.
    help: Option<&'static str>,
}

const fn opt(
    flag: &'static str,
    dest: &'static str,
    kind: Kind,
    choices: &'static [&'static str],
    help: &'static str,
) -> Opt {
    Opt {
        flag,
        dest,
        kind,
        choices,
        help: Some(help),
    }
}

const fn hidden(flag: &'static str, dest: &'static str, kind: Kind) -> Opt {
    Opt {
        flag,
        dest,
        kind,
        choices: &[],
        help: None,
    }
}

const AREAS: &[&str] = &["whole", "narrow", "iris", "centre"];

/// In watch.build_parser's order (argparse lists ambiguous matches in it).
const OPTS: &[Opt] = &[
    opt(
        "--trigger",
        "trigger",
        Kind::Str,
        &["key", "auto"],
        "'key' reads a burst after the ping key is pressed; 'auto' watches continuously",
    ),
    opt(
        "--key",
        "key",
        Kind::Str,
        &[],
        "the read key or button (a key, joy0.b24, rctrl+joy0.b24)",
    ),
    opt(
        "--ping-delay",
        "ping_delay",
        Kind::Float,
        &[],
        "seconds to wait after the key before reading",
    ),
    opt(
        "--auto-read-delay",
        "auto_read_delay",
        Kind::Float,
        &[],
        "with --trigger auto: seconds to let a new contact's name finish appearing",
    ),
    opt(
        "--burst-spacing",
        "burst_spacing",
        Kind::Float,
        &[],
        "seconds between a burst's frames",
    ),
    opt(
        "--debug",
        "debug",
        Kind::True,
        &[],
        "save every burst frame to debug_bursts/",
    ),
    opt(
        "--focus-key",
        "focus_key",
        Kind::Str,
        &[],
        "a second key; with the IRIS reader it does a full read ('off' disables it)",
    ),
    opt(
        "--chat-key",
        "chat_key",
        Kind::Str,
        &[],
        "read the chat window (needs OCR, not in this version; 'off' disables it)",
    ),
    opt(
        "--chat-region",
        "chat_region",
        Kind::Str,
        &[],
        "the chat window as x,y,w,h fractions of the screen, or 'auto'",
    ),
    opt(
        "--no-debug-log",
        "debug_log",
        Kind::False,
        &[],
        "do not write the session log to debug_bursts/",
    ),
    opt(
        "--support-bundle",
        "support_bundle",
        Kind::True,
        &[],
        "zip the session logs, your settings and your debug screenshots into one file to send \
         for support, then exit. Never includes your databases or API key",
    ),
    opt(
        "--no-frames",
        "bundle_frames",
        Kind::False,
        &[],
        "with --support-bundle: leave the screenshots out",
    ),
    opt(
        "--no-audit",
        "audit",
        Kind::False,
        &[],
        "do not save crops of reads that need checking",
    ),
    opt(
        "--reader",
        "reader",
        Kind::Str,
        &["ocr", "iris+ocr", "iris"],
        "who reads the name plates; this version has 'iris' only",
    ),
    opt(
        "--scan-area",
        "scan_area",
        Kind::Str,
        AREAS,
        "how much of the screen a read scans: whole, narrow, iris or centre",
    ),
    opt(
        "--opencv-threads",
        "opencv_threads",
        Kind::Int,
        &[],
        "(the Python's OpenCV thread cap; this version reads on one thread)",
    ),
    opt(
        "--auto-scan-area",
        "auto_scan_area",
        Kind::Str,
        AREAS,
        "with --trigger auto: the area scanned on every frame",
    ),
    opt(
        "--focus-mask",
        "focus_mask",
        Kind::True,
        &[],
        "(OCR focused reads; no effect here)",
    ),
    opt(
        "--dual-key",
        "dual_key",
        Kind::Str,
        &[],
        "a key for a wide + centre read; with the IRIS reader, a full read",
    ),
    opt(
        "--dual-on-read",
        "dual_on_read",
        Kind::True,
        &[],
        "make the read key do a wide + centre read",
    ),
    opt(
        "--focus-always",
        "focus_always",
        Kind::True,
        &[],
        "(testing) read focused on every trigger",
    ),
    opt(
        "--focus-size",
        "focus_size",
        Kind::Str,
        &[],
        "focused capture region, WxH, or 'auto'",
    ),
    opt(
        "--queue-max",
        "queue_max",
        Kind::Int,
        &[],
        "how many pings may queue while a burst is read",
    ),
    opt(
        "--no-rsi",
        "no_rsi",
        Kind::True,
        &[],
        "skip profile lookups",
    ),
    opt(
        "--where",
        "where",
        Kind::True,
        &[],
        "print where the program and your data live, and exit",
    ),
    opt(
        "--list-inputs",
        "list_inputs",
        Kind::True,
        &[],
        "list the joysticks, and exit",
    ),
    opt(
        "--check-resolution",
        "check_resolution",
        Kind::Many,
        &[],
        "run screenshots through the reader and exit (not in this version yet)",
    ),
    opt(
        "--no-db",
        "no_db",
        Kind::True,
        &[],
        "do not record sightings or check the reputation database",
    ),
    opt(
        "--audio",
        "audio",
        Kind::Str,
        &["off", "tones", "speech", "both"],
        "audible alerts",
    ),
    opt(
        "--gate",
        "gate",
        Kind::True,
        &[],
        "(OCR's km pre-gate; no effect here)",
    ),
    opt(
        "--haze",
        "haze",
        Kind::True,
        &[],
        "(OCR's hazed-daylight locator; no effect here)",
    ),
    opt(
        "--rsi-min-votes",
        "rsi_min_votes",
        Kind::Int,
        &[],
        "votes needed before a handle is looked up",
    ),
    opt(
        "--hz",
        "hz",
        Kind::Float,
        &[],
        "with --trigger auto: frames a second",
    ),
    opt(
        "--burst",
        "burst",
        Kind::Int,
        &[],
        "frames read per trigger",
    ),
    opt(
        "--cooldown",
        "cooldown",
        Kind::Float,
        &[],
        "seconds before the same handle is announced again",
    ),
    opt(
        "--radius",
        "radius",
        Kind::Float,
        &[],
        "pixels within which a contact counts as already tracked",
    ),
    opt(
        "--expire",
        "expire",
        Kind::Float,
        &[],
        "seconds before a vanished contact counts as new again",
    ),
    opt("--monitor", "monitor", Kind::Int, &[], "1-based monitor"),
    opt(
        "--hdr-capture",
        "hdr_capture",
        Kind::Str,
        &["auto", "off"],
        "with Windows HDR on: 'auto' captures the HDR picture and converts it",
    ),
    opt(
        "--seconds",
        "seconds",
        Kind::Float,
        &[],
        "stop after this long (0 = until Ctrl+C)",
    ),
    opt(
        "--replay",
        "replay",
        Kind::Str,
        &[],
        "saved frames instead of the screen: a file, a folder or a pattern",
    ),
    opt("--log", "log", Kind::Str, &[], "sightings.csv's path"),
    opt(
        "--min-votes",
        "min_votes",
        Kind::Int,
        &[],
        "votes needed to announce",
    ),
    opt(
        "--verbose",
        "verbose",
        Kind::True,
        &[],
        "also report unreadable contacts and repeats",
    ),
    hidden("--no-debug", "debug", Kind::False),
    hidden("--no-haze", "haze", Kind::False),
    hidden("--no-gate", "gate", Kind::False),
    hidden("--no-dual-on-read", "dual_on_read", Kind::False),
    hidden("--no-focus-mask", "focus_mask", Kind::False),
    hidden("--rsi", "no_rsi", Kind::False),
];

/// Setting -> argument it is the default of (watch._SETTING_TO_ARG).
pub const SETTING_TO_ARG: &[(&str, &str)] = &[
    ("trigger", "trigger"),
    ("key", "key"),
    ("focus_key", "focus_key"),
    ("focus_size", "focus_size"),
    ("focus_mask", "focus_mask"),
    ("burst", "burst"),
    ("burst_spacing", "burst_spacing"),
    ("ping_delay", "ping_delay"),
    ("monitor", "monitor"),
    ("hz", "hz"),
    ("queue_max", "queue_max"),
    ("hdr_capture", "hdr_capture"),
    ("auto_read_delay", "auto_read_delay"),
    ("min_votes", "min_votes"),
    ("gate", "gate"),
    ("haze", "haze"),
    ("cooldown", "cooldown"),
    ("radius", "radius"),
    ("expire", "expire"),
    ("audio_mode", "audio"),
    ("rsi_min_votes", "rsi_min_votes"),
    ("audit_enabled", "audit"),
    ("debug_frames", "debug"),
    ("debug_log", "debug_log"),
    ("chat_key", "chat_key"),
    ("chat_region", "chat_region"),
    ("dual_key", "dual_key"),
    ("dual_on_read", "dual_on_read"),
    ("reader", "reader"),
    ("scan_area", "scan_area"),
    ("auto_scan_area", "auto_scan_area"),
    ("opencv_threads", "opencv_threads"),
];

/// The run's arguments (argparse's namespace).
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub trigger: String,
    pub key: String,
    pub ping_delay: f64,
    pub auto_read_delay: f64,
    pub burst_spacing: f64,
    pub debug: bool,
    pub focus_key: String,
    pub chat_key: String,
    pub chat_region: String,
    pub debug_log: bool,
    pub support_bundle: bool,
    pub bundle_frames: bool,
    pub audit: bool,
    pub reader: String,
    pub scan_area: String,
    pub opencv_threads: i64,
    pub auto_scan_area: String,
    pub focus_mask: bool,
    pub dual_key: String,
    pub dual_on_read: bool,
    pub focus_always: bool,
    pub focus_size: String,
    pub queue_max: i64,
    pub no_rsi: bool,
    pub where_: bool,
    pub list_inputs: bool,
    pub check_resolution: Option<Vec<String>>,
    pub no_db: bool,
    pub audio: Option<String>,
    pub gate: bool,
    pub haze: bool,
    pub rsi_min_votes: i64,
    pub hz: f64,
    pub burst: i64,
    pub cooldown: f64,
    pub radius: f64,
    pub expire: f64,
    pub monitor: i64,
    pub hdr_capture: String,
    pub seconds: f64,
    pub replay: Option<String>,
    pub log: String,
    pub min_votes: i64,
    pub verbose: bool,
}

/// Why the command line was refused: argparse's message and exit code (0
/// for --help, after the help is printed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliError {
    pub code: i32,
    pub message: String,
}

fn err(message: String) -> CliError {
    CliError { code: 2, message }
}

/// argparse's defaults, before the settings.
fn base_defaults(data_root: &Path) -> Map<String, Value> {
    let log = data_root.join("sightings.csv").display().to_string();
    let pairs: Vec<(&str, Value)> = vec![
        ("trigger", json!("key")),
        ("key", json!("0")),
        ("ping_delay", json!(0.0)),
        ("auto_read_delay", json!(0.5)),
        ("burst_spacing", json!(0.5)),
        ("debug", json!(false)),
        ("focus_key", json!("9")),
        ("chat_key", json!("off")),
        ("chat_region", json!("auto")),
        ("debug_log", json!(true)),
        ("support_bundle", json!(false)),
        ("bundle_frames", json!(true)),
        ("audit", json!(true)),
        ("reader", json!("iris")),
        ("scan_area", json!("whole")),
        ("opencv_threads", json!(2)),
        ("auto_scan_area", json!("narrow")),
        ("focus_mask", json!(false)),
        ("dual_key", json!("off")),
        ("dual_on_read", json!(false)),
        ("focus_always", json!(false)),
        ("focus_size", json!("auto")),
        ("queue_max", json!(3)),
        ("no_rsi", json!(false)),
        ("where", json!(false)),
        ("list_inputs", json!(false)),
        ("check_resolution", Value::Null),
        ("no_db", json!(false)),
        ("audio", Value::Null),
        ("gate", json!(false)),
        ("haze", json!(false)),
        ("rsi_min_votes", json!(2)),
        ("hz", json!(5.0)),
        ("burst", json!(3)),
        ("cooldown", json!(300.0)),
        ("radius", json!(180.0)),
        ("expire", json!(20.0)),
        ("monitor", json!(1)),
        ("hdr_capture", json!("auto")),
        ("seconds", json!(0.0)),
        ("replay", Value::Null),
        ("log", json!(log)),
        ("min_votes", json!(1)),
        ("verbose", json!(false)),
    ];
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// The defaults with the saved settings over them (_defaults_from_settings).
pub fn defaults(data_root: &Path, settings: &Map<String, Value>) -> Map<String, Value> {
    let mut out = base_defaults(data_root);
    for (key, arg) in SETTING_TO_ARG {
        if let Some(v) = settings.get(*key) {
            out.insert(arg.to_string(), v.clone());
        }
    }
    let rsi = settings
        .get("rsi_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    out.insert("no_rsi".into(), json!(!rsi));
    out
}

/// What a command-line word is to argparse: an option (known or not), or
/// a value.
enum Word<'a> {
    Help,
    Known(&'a Opt, Option<String>),
    /// Matches more than one long option.
    Ambiguous(Vec<&'static str>),
    UnknownOption,
    Value,
}

fn negative_number(s: &str) -> bool {
    let b = s.strip_prefix('-').unwrap_or("");
    if b.is_empty() {
        return false;
    }
    if b.bytes().all(|c| c.is_ascii_digit()) {
        return true;
    }
    match b.split_once('.') {
        Some((i, f)) => {
            !f.is_empty()
                && i.bytes().all(|c| c.is_ascii_digit())
                && f.bytes().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

const HELP_FLAGS: [&str; 2] = ["-h", "--help"];

fn classify(word: &str) -> Word<'static> {
    if word.is_empty() || !word.starts_with('-') || word.len() == 1 {
        return Word::Value;
    }
    if HELP_FLAGS.contains(&word) {
        return Word::Help;
    }
    if let Some(o) = OPTS.iter().find(|o| o.flag == word) {
        return Word::Known(o, None);
    }
    if let Some((name, value)) = word.split_once('=') {
        if HELP_FLAGS.contains(&name) {
            return Word::Help;
        }
        if let Some(o) = OPTS.iter().find(|o| o.flag == name) {
            return Word::Known(o, Some(value.to_string()));
        }
    }
    if word.starts_with("--") {
        let (prefix, value) = match word.split_once('=') {
            Some((p, v)) => (p, Some(v.to_string())),
            None => (word, None),
        };
        let mut matches: Vec<&'static str> = Vec::new();
        if "--help".starts_with(prefix) {
            matches.push("--help");
        }
        matches.extend(
            OPTS.iter()
                .filter(|o| o.flag.starts_with(prefix))
                .map(|o| o.flag),
        );
        match matches.len() {
            0 => {}
            1 if matches[0] == "--help" => return Word::Help,
            1 => {
                let o = OPTS.iter().find(|o| o.flag == matches[0]).unwrap();
                return Word::Known(o, value);
            }
            _ => return Word::Ambiguous(matches),
        }
    } else if word.starts_with("-h") {
        // -h with more after it: argparse shows the help first.
        return Word::Help;
    }
    if negative_number(word) || word.contains(' ') {
        return Word::Value;
    }
    Word::UnknownOption
}

fn is_value(word: &str) -> bool {
    matches!(classify(word), Word::Value)
}

fn convert(o: &Opt, raw: &str) -> Result<Value, CliError> {
    let v = match o.kind {
        Kind::Float => {
            let x = py_float_text(raw).ok_or_else(|| {
                err(format!(
                    "argument {}: invalid float value: {}",
                    o.flag,
                    py_repr_str(raw)
                ))
            })?;
            // JSON has no infinity: kept as Python's repr, read back by f().
            if x.is_finite() {
                json!(x)
            } else if x.is_nan() {
                json!("nan")
            } else if x > 0.0 {
                json!("inf")
            } else {
                json!("-inf")
            }
        }
        Kind::Int => json!(py_int_text(raw).ok_or_else(|| err(format!(
            "argument {}: invalid int value: {}",
            o.flag,
            py_repr_str(raw)
        )))?),
        _ => json!(raw),
    };
    if !o.choices.is_empty() && !o.choices.contains(&raw) {
        let list: Vec<String> = o.choices.iter().map(|c| py_repr_str(c)).collect();
        return Err(err(format!(
            "argument {}: invalid choice: {} (choose from {})",
            o.flag,
            py_repr_str(raw),
            list.join(", ")
        )));
    }
    Ok(v)
}

/// Parses `argv` (without the program's name) over `defaults`; every
/// argument's value by its name, as argparse's namespace.
pub fn parse_map(
    argv: &[String],
    defaults: Map<String, Value>,
) -> Result<Map<String, Value>, CliError> {
    let mut out = defaults;
    let mut extras: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let word = &argv[i];
        i += 1;
        if word == "--" {
            extras.extend(argv[i - 1..].iter().cloned());
            break;
        }
        match classify(word) {
            Word::Value | Word::UnknownOption => extras.push(word.clone()),
            Word::Help => {
                return Err(CliError {
                    code: 0,
                    message: String::new(),
                });
            }
            Word::Ambiguous(m) => {
                return Err(err(format!(
                    "ambiguous option: {word} could match {}",
                    m.join(", ")
                )));
            }
            Word::Known(o, explicit) => match o.kind {
                Kind::True | Kind::False => {
                    if let Some(v) = explicit {
                        return Err(err(format!(
                            "argument {}: ignored explicit argument {}",
                            o.flag,
                            py_repr_str(&v)
                        )));
                    }
                    out.insert(o.dest.into(), json!(o.kind == Kind::True));
                }
                Kind::Many => {
                    let mut vals: Vec<Value> = Vec::new();
                    if let Some(v) = explicit {
                        vals.push(json!(v));
                    } else {
                        while i < argv.len() && argv[i] != "--" && is_value(&argv[i]) {
                            vals.push(json!(argv[i]));
                            i += 1;
                        }
                        if vals.is_empty() {
                            return Err(err(format!(
                                "argument {}: expected at least one argument",
                                o.flag
                            )));
                        }
                    }
                    out.insert(o.dest.into(), Value::Array(vals));
                }
                _ => {
                    let raw = match explicit {
                        Some(v) => v,
                        None if i < argv.len() && is_value(&argv[i]) => {
                            i += 1;
                            argv[i - 1].clone()
                        }
                        None => {
                            return Err(err(format!("argument {}: expected one argument", o.flag)));
                        }
                    };
                    out.insert(o.dest.into(), convert(o, &raw)?);
                }
            },
        }
    }
    if !extras.is_empty() {
        return Err(err(format!("unrecognized arguments: {}", extras.join(" "))));
    }
    Ok(out)
}

fn s(m: &Map<String, Value>, k: &str) -> String {
    match m.get(k) {
        Some(Value::String(v)) => v.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => String::new(),
    }
}

fn opt_s(m: &Map<String, Value>, k: &str) -> Option<String> {
    m.get(k).and_then(Value::as_str).map(String::from)
}

fn f(m: &Map<String, Value>, k: &str) -> f64 {
    match m.get(k) {
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(t)) => py_float_text(t).unwrap_or(0.0),
        Some(v) => v.as_f64().unwrap_or(0.0),
        None => 0.0,
    }
}

fn i(m: &Map<String, Value>, k: &str) -> i64 {
    match m.get(k) {
        Some(Value::Bool(b)) => i64::from(*b),
        Some(v) => v
            .as_i64()
            .or_else(|| v.as_f64().map(|x| x as i64))
            .unwrap_or(0),
        None => 0,
    }
}

fn b(m: &Map<String, Value>, k: &str) -> bool {
    match m.get(k) {
        Some(Value::Bool(v)) => *v,
        Some(Value::Null) | None => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(v)) => !v.is_empty(),
        Some(_) => true,
    }
}

impl Args {
    /// The typed arguments from argparse's namespace.
    pub fn from_map(m: &Map<String, Value>) -> Args {
        Args {
            trigger: s(m, "trigger"),
            key: s(m, "key"),
            ping_delay: f(m, "ping_delay"),
            auto_read_delay: f(m, "auto_read_delay"),
            burst_spacing: f(m, "burst_spacing"),
            debug: b(m, "debug"),
            focus_key: s(m, "focus_key"),
            chat_key: s(m, "chat_key"),
            chat_region: s(m, "chat_region"),
            debug_log: b(m, "debug_log"),
            support_bundle: b(m, "support_bundle"),
            bundle_frames: b(m, "bundle_frames"),
            audit: b(m, "audit"),
            reader: s(m, "reader"),
            scan_area: s(m, "scan_area"),
            opencv_threads: i(m, "opencv_threads"),
            auto_scan_area: s(m, "auto_scan_area"),
            focus_mask: b(m, "focus_mask"),
            dual_key: s(m, "dual_key"),
            dual_on_read: b(m, "dual_on_read"),
            focus_always: b(m, "focus_always"),
            focus_size: s(m, "focus_size"),
            queue_max: i(m, "queue_max"),
            no_rsi: b(m, "no_rsi"),
            where_: b(m, "where"),
            list_inputs: b(m, "list_inputs"),
            check_resolution: m
                .get("check_resolution")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .map(|x| x.as_str().unwrap_or_default().to_string())
                        .collect()
                }),
            no_db: b(m, "no_db"),
            audio: opt_s(m, "audio"),
            gate: b(m, "gate"),
            haze: b(m, "haze"),
            rsi_min_votes: i(m, "rsi_min_votes"),
            hz: f(m, "hz"),
            burst: i(m, "burst"),
            cooldown: f(m, "cooldown"),
            radius: f(m, "radius"),
            expire: f(m, "expire"),
            monitor: i(m, "monitor"),
            hdr_capture: s(m, "hdr_capture"),
            seconds: f(m, "seconds"),
            replay: opt_s(m, "replay"),
            log: s(m, "log"),
            min_votes: i(m, "min_votes"),
            verbose: b(m, "verbose"),
        }
    }

    /// Parses the command line over the saved settings.
    pub fn parse(
        argv: &[String],
        data_root: &Path,
        settings: &Map<String, Value>,
    ) -> Result<Args, CliError> {
        let m = parse_map(argv, defaults(data_root, settings))?;
        Ok(Args::from_map(&m))
    }
}

/// The help text.
pub fn help() -> String {
    let mut out = String::from(
        "usage: sc-watch [options]\n\n\
         Reads player handles off the Star Citizen HUD, announces them, and looks\n\
         them up. Saved settings are the defaults; options here override them for\n\
         one run.\n\noptions:\n  -h, --help            show this help and exit\n",
    );
    for o in OPTS {
        let Some(h) = o.help else { continue };
        let left = match o.kind {
            Kind::True | Kind::False => o.flag.to_string(),
            Kind::Many => format!("{} FILE [FILE ...]", o.flag),
            _ if !o.choices.is_empty() => format!("{} {{{}}}", o.flag, o.choices.join(",")),
            _ => format!("{} {}", o.flag, o.dest.to_uppercase()),
        };
        if left.len() < 22 {
            out.push_str(&format!("  {left:<22}{h}\n"));
        } else {
            out.push_str(&format!("  {left}\n  {:<22}{h}\n", ""));
        }
    }
    out
}
