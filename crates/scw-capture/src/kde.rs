//! KDE Plasma's own account of its screens (`kscreen-doctor -o`): which one
//! has HDR on, and at what SDR brightness. Only reported - nothing here
//! changes how frames are read: with HDR on, KDE converts the screen to SDR
//! itself before sharing it (since Plasma 6.2.4, SDR content as it was
//! drawn, the SDR brightness as white), which is the conversion sc-watch
//! makes for itself on Windows. The start lines and the session log say
//! what KDE reported, so a report from an HDR desktop can be read.
//!
//! The parsing is plain text and runs anywhere; only asking KDE is Linux's.

/// One screen, as kscreen-doctor describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KdeOutput {
    /// Its connector's name (DP-1, HDMI-A-1).
    pub name: String,
    pub enabled: bool,
    /// Where it sits on the desktop, and its size there.
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    /// "enabled", "disabled" or "incapable", as KDE says.
    pub hdr: Option<String>,
    /// The SDR brightness setting, in nits.
    pub sdr_brightness: Option<f64>,
}

impl KdeOutput {
    pub fn hdr_on(&self) -> bool {
        self.hdr.as_deref() == Some("enabled")
    }
}

/// `text` without its terminal colour codes (ESC [ ... letter).
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The words after `label` in `block`, up to the end of its line.
fn after<'a>(block: &'a str, label: &str) -> Option<&'a str> {
    let at = block.find(label)? + label.len();
    let rest = &block[at..];
    Some(rest[..rest.find('\n').unwrap_or(rest.len())].trim())
}

/// "x,y wxh" -> ((x, y), (w, h)).
fn geometry(text: &str) -> Option<((i32, i32), (i32, i32))> {
    let mut parts = text.split_whitespace();
    let (x, y) = parts.next()?.split_once(',')?;
    let (w, h) = parts.next()?.split_once('x')?;
    Some((
        (x.trim().parse().ok()?, y.trim().parse().ok()?),
        (w.trim().parse().ok()?, h.trim().parse().ok()?),
    ))
}

/// The screens in kscreen-doctor's `-o` report, in its order.
pub fn parse(text: &str) -> Vec<KdeOutput> {
    let text = strip_ansi(text);
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("Output:") {
        rest = &rest[at + "Output:".len()..];
        let block = &rest[..rest.find("Output:").unwrap_or(rest.len())];
        let mut head = block.split_whitespace();
        let _id = head.next();
        let Some(name) = head.next() else { continue };
        let enabled = block
            .lines()
            .next()
            .is_some_and(|l| l.split_whitespace().any(|w| w == "enabled"));
        let (position, size) = match after(block, "Geometry:").and_then(geometry) {
            Some((p, s)) => (Some(p), Some(s)),
            None => (None, None),
        };
        let hdr = after(block, "HDR:")
            .and_then(|v| v.split_whitespace().next())
            .map(|v| v.trim_end_matches(',').to_lowercase());
        let sdr_brightness = after(block, "SDR brightness:")
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<f64>().ok());
        out.push(KdeOutput {
            name: name.to_string(),
            enabled,
            position,
            size,
            hdr,
            sdr_brightness,
        });
    }
    out
}

/// The screen that was shared: the one at the portal's position (and of its
/// size, when both are known); else the only one of its size; else the only
/// screen switched on.
pub fn matching(
    outputs: &[KdeOutput],
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
) -> Option<&KdeOutput> {
    let on: Vec<&KdeOutput> = outputs.iter().filter(|o| o.enabled).collect();
    fn one(found: Vec<&KdeOutput>) -> Option<&KdeOutput> {
        match found.as_slice() {
            [o] => Some(*o),
            _ => None,
        }
    }
    if let Some(p) = position
        && let Some(o) = one(on
            .iter()
            .copied()
            .filter(|o| o.position == Some(p) && (size.is_none() || o.size == size))
            .collect())
    {
        return Some(o);
    }
    if let Some(s) = size
        && let Some(o) = one(on.iter().copied().filter(|o| o.size == Some(s)).collect())
    {
        return Some(o);
    }
    one(on)
}

/// Asks KDE (`kscreen-doctor -o`, waited for up to three seconds): its
/// screens, or why they are not known.
#[cfg(target_os = "linux")]
pub fn query() -> Result<Vec<KdeOutput>, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new("kscreen-doctor")
        .arg("-o")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("kscreen-doctor could not run ({e})"))?;
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("kscreen-doctor did not answer".into());
            }
        }
    }
    let mut text = String::new();
    child
        .stdout
        .take()
        .ok_or("kscreen-doctor gave no output")?
        .read_to_string(&mut text)
        .map_err(|e| format!("kscreen-doctor's answer could not be read ({e})"))?;
    let outputs = parse(&text);
    if outputs.is_empty() {
        return Err("kscreen-doctor named no screen".into());
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    // As Plasma 6's kscreen-doctor prints it to a terminal: colours, a
    // field a line (the connector names and sizes made up).
    const TWO: &str = "\u{1b}[01;32mOutput: \u{1b}[0;0m1 DP-1 \u{1b}[01;32menabled\u{1b}[0;0m connected priority 1 DisplayPort
\tModes: 1:5120x1440@240.00*! 2:5120x1440@120.00
\tGeometry: 0,0 5120x1440
\tScale: 1
\tRotation: 1
\tOverscan: 0
\tVrr: Automatic
\tRgbRange: unknown
\tHDR: enabled
\t\tSDR brightness: 203 nits
\t\tSDR gamut wideness: 0%
\t\tPeak brightness: 1015 nits
\tWide Color Gamut: enabled
\tICC profile: none
\u{1b}[01;32mOutput: \u{1b}[0;0m2 HDMI-A-1 \u{1b}[01;31mdisabled\u{1b}[0;0m connected priority 0 HDMI
\tModes: 1:1920x1080@60.00*!
\tGeometry: 5120,0 1920x1080
\tHDR: incapable
\tWide Color Gamut: incapable
";

    #[test]
    fn screens_are_read_from_the_report() {
        let o = parse(TWO);
        assert_eq!(o.len(), 2);
        assert_eq!(
            o[0],
            KdeOutput {
                name: "DP-1".into(),
                enabled: true,
                position: Some((0, 0)),
                size: Some((5120, 1440)),
                hdr: Some("enabled".into()),
                sdr_brightness: Some(203.0),
            }
        );
        assert!(o[0].hdr_on());
        assert_eq!(o[1].name, "HDMI-A-1");
        assert!(!o[1].enabled, "disabled");
        assert_eq!(o[1].position, Some((5120, 0)));
        assert_eq!(o[1].hdr.as_deref(), Some("incapable"));
        assert_eq!(o[1].sdr_brightness, None);
        assert!(!o[1].hdr_on());
        // piped, on one line a screen, without colours, as older versions
        let flat = parse(
            "Output: 3 eDP-1 enabled connected priority 1 Panel Modes: 1:2560x1600@165! \
             Geometry: 0,0 1280x800 Scale: 2 HDR: disabled SDR brightness: 250.5 nits",
        );
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].name, "eDP-1");
        assert_eq!(
            flat[0].size,
            Some((1280, 800)),
            "the desktop's size, scaled"
        );
        assert_eq!(flat[0].hdr.as_deref(), Some("disabled"));
        assert_eq!(flat[0].sdr_brightness, Some(250.5));
        assert!(parse("kscreen-doctor: no outputs").is_empty());
    }

    #[test]
    fn the_shared_screen_is_found_by_place_then_size() {
        let mut o = parse(TWO);
        o[1].enabled = true;
        let name = |p, s| matching(&o, p, s).map(|o| o.name.clone());
        assert_eq!(name(Some((5120, 0)), None), Some("HDMI-A-1".into()));
        assert_eq!(name(Some((0, 0)), Some((5120, 1440))), Some("DP-1".into()));
        assert_eq!(name(None, Some((1920, 1080))), Some("HDMI-A-1".into()));
        // two screens on and nothing to tell them apart
        assert_eq!(name(None, None), None);
        assert_eq!(name(Some((9, 9)), Some((640, 480))), None);
        // one screen on: that one
        o[1].enabled = false;
        assert_eq!(
            matching(&o, None, None).map(|o| o.name.as_str()),
            Some("DP-1")
        );
    }

    #[test]
    fn colours_are_taken_out() {
        assert_eq!(strip_ansi("\u{1b}[01;32mOutput: \u{1b}[0;0m1"), "Output: 1");
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
