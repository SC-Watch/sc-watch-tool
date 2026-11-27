//! Handles out of the in-game chat window (chat.py): a second channel that
//! needs no ping - `[GLOBAL] EXMPLSaint: o7` names whoever is talking, so
//! station chat names people you will never get a HUD label for.
//!
//! The window sits a fixed distance left of screen centre (at 1440 lines,
//! scaled by the height), further left with the helmet off; with the region
//! worked out from the frame the helmet-on place is read first and, only
//! if it finds nobody, the helmet-off one - one place's answer, never both.
//! The OCR reads the crop as it is (colour), and each line's handle is what
//! sits between a known channel tag and the first colon.

use scw_vision::Image;

use crate::legacy::{OcrLine, Recognise};

const REFERENCE_HEIGHT: f64 = 1440.0;
const ANCHOR_LEFT: f64 = 1190.0;
const WIDTH: f64 = 1120.0;
const ANCHOR_LEFT_BARE: f64 = 1290.0;
const WIDTH_BARE: f64 = 1220.0;
const HEIGHT: f64 = 520.0;
const BOTTOM: f64 = 0.590;

/// Words where a handle would be that are not people.
const NOT_HANDLES: [&str; 16] = [
    "GLOBAL", "LOCAL", "PARTY", "ORG", "SYSTEM", "TEAM", "GUILD", "SQUAD", "ADMIN", "SERVER",
    "NOTICE", "INFO", "ERROR", "WARNING", "CHAT", "ALL",
];

/// A region of the screen as fractions of it: x, y, w, h.
pub type Region = (f64, f64, f64, f64);

/// Where [`read_located`]'s answer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    HelmetOn,
    HelmetOff,
    /// "auto" found nobody: both places were looked at.
    Both,
    /// The region set in the settings.
    Set,
}

impl Place {
    /// chat.py's words for it.
    pub fn as_str(self) -> &'static str {
        match self {
            Place::HelmetOn => "helmet on",
            Place::HelmetOff => "helmet off",
            Place::Both => "helmet on+off",
            Place::Set => "your region",
        }
    }
}

/// One handle read off the chat.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatLine {
    /// As its owner writes it, e.g. 'EXMPLSaint'.
    pub handle: String,
    /// The whole OCR line it came from.
    pub text: String,
    pub conf: f64,
}

impl ChatLine {
    /// Upper case, the way the database stores handles.
    pub fn key(&self) -> String {
        self.handle.to_uppercase()
    }
}

/// `default_region`: the chat rectangle for a frame this size, helmet on
/// or off, as fractions.
pub fn default_region(frame_w: usize, frame_h: usize, helmet: bool) -> Region {
    let (anchor, width) = if helmet {
        (ANCHOR_LEFT, WIDTH)
    } else {
        (ANCHOR_LEFT_BARE, WIDTH_BARE)
    };
    let (fw, fh) = (frame_w as f64, frame_h as f64);
    let s = fh / REFERENCE_HEIGHT;
    let x0 = fw / 2.0 - anchor * s;
    let y1 = BOTTOM * fh;
    let y0 = y1 - HEIGHT * s;
    // Python's max(0.0, v) and min(a, b): the first unless the second wins
    let max0 = |v: f64| if v > 0.0 { v } else { 0.0 };
    let min = |a: f64, b: f64| if b < a { b } else { a };
    (
        max0(x0) / fw,
        max0(y0) / fh,
        min(width * s, fw - x0) / fw,
        min(HEIGHT * s, y1) / fh,
    )
}

/// `crop_region`: the region's pixels and where they start.
pub fn crop_region(frame: &Image<u8>, region: Region) -> (Image<u8>, (usize, usize)) {
    let (fw, fh) = (frame.width as f64, frame.height as f64);
    let (x, y, w, h) = region;
    let x0 = (fw * x) as i64;
    let y0 = (fh * y) as i64;
    let x1 = (frame.width as i64).min((fw * (x + w)) as i64);
    let y1 = (frame.height as i64).min((fh * (y + h)) as i64);
    // numpy's slice of starts at or above zero
    let (x0c, y0c) = (
        x0.clamp(0, frame.width as i64),
        y0.clamp(0, frame.height as i64),
    );
    let crop = if x1 <= x0c || y1 <= y0c {
        Image::new(0, 0, frame.channels)
    } else {
        frame.crop(
            x0c as usize,
            y0c as usize,
            (x1 - x0c) as usize,
            (y1 - y0c) as usize,
        )
    };
    (crop, (x0.max(0) as usize, y0.max(0) as usize))
}

/// Python's `str.isspace()` for one character.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// The channel tag at `c[i..]`: GL[O0]B[A4]L, L[O0]C[A4]L, P[A4]RTY,
/// [O0]RG, SYSTEM, TE[A4]M, GUILD, SQU[A4]D, [A4]DMIN or SERVER, spelled to
/// absorb OCR's O/0 and A/4 - its length. No two start alike, so at most
/// one fits.
fn tag_at(c: &[char], i: usize) -> Option<usize> {
    const TAGS: [&str; 10] = [
        "GLOBAL", "LOCAL", "PARTY", "ORG", "SYSTEM", "TEAM", "GUILD", "SQUAD", "ADMIN", "SERVER",
    ];
    // Where the tag takes OCR's look-alikes: O also 0, A also 4.
    const LOOSE: [&str; 10] = [
        "GL.B.L", "L.C.L", "P.RTY", ".RG", "SYSTEM", "TE.M", "GUILD", "SQU.D", ".DMIN", "SERVER",
    ];
    for (tag, loose) in TAGS.iter().zip(LOOSE) {
        let n = tag.len();
        if i + n > c.len() {
            continue;
        }
        let fits = tag
            .chars()
            .zip(loose.chars())
            .enumerate()
            .all(|(k, (t, l))| {
                let ch = c[i + k];
                ch == t || (l == '.' && ((t == 'O' && ch == '0') || (t == 'A' && ch == '4')))
            });
        if fits {
            return Some(n);
        }
    }
    None
}

fn handle_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}

/// `LINE_RE.search(text).group(1)`: the handle in the leftmost
/// `[\[\(]?\s*TAG\s*[\]\)\}IJ|1l]?\s*([A-Za-z0-9_\-]{2,40})\s*:`, with
/// re's backtracking - the optional brackets are tried taken, then not
/// (the closing one is often read as I, J, 1 or l, which a handle may also
/// start with).
fn line_re_handle(text: &str) -> Option<String> {
    let c: Vec<char> = text.chars().collect();
    let n = c.len();
    let spaces = |mut i: usize| {
        while i < n && py_space(c[i]) {
            i += 1;
        }
        i
    };
    for p in 0..=n {
        for open in [true, false] {
            let mut i = p;
            if open {
                if i < n && matches!(c[i], '[' | '(') {
                    i += 1;
                } else {
                    continue;
                }
            }
            i = spaces(i);
            let Some(len) = tag_at(&c, i) else {
                continue;
            };
            i = spaces(i + len);
            for close in [true, false] {
                let mut j = i;
                if close {
                    if j < n && matches!(c[j], ']' | ')' | '}' | 'I' | 'J' | '|' | '1' | 'l') {
                        j += 1;
                    } else {
                        continue;
                    }
                }
                j = spaces(j);
                // Greedy, at most 40: a shorter handle would leave a
                // handle character where the colon must be.
                let mut k = j;
                while k < n && k - j < 40 && handle_char(c[k]) {
                    k += 1;
                }
                if k - j >= 2 {
                    let m = spaces(k);
                    if m < n && c[m] == ':' {
                        return Some(c[j..k].iter().collect());
                    }
                }
            }
        }
    }
    None
}

/// `handles_in`: the handle in one OCR line, or None if it is not a chat
/// line (or names a channel, or is all digits - a timestamp).
pub fn handles_in(text: &str) -> Option<String> {
    let h = line_re_handle(text)?;
    if NOT_HANDLES.contains(&h.to_uppercase().as_str()) {
        return None;
    }
    if h.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(h)
}

/// `_read`: every distinct handle in one region, first spelling kept.
fn read_region(
    frame: &Image<u8>,
    ocr: &mut dyn Recognise,
    region: Region,
) -> Result<Vec<ChatLine>, String> {
    let (crop, _) = crop_region(frame, region);
    if crop.data.is_empty() {
        return Ok(Vec::new());
    }
    let mut out: Vec<ChatLine> = Vec::new();
    for line in ocr.lines(&crop)? {
        let OcrLine { text, conf, .. } = line;
        let Some(h) = handles_in(&text) else {
            continue;
        };
        if out.iter().any(|c| c.key() == h.to_uppercase()) {
            continue;
        }
        out.push(ChatLine {
            handle: h,
            text,
            conf,
        });
    }
    Ok(out)
}

/// `read_located`: the handles the chat window shows, and where the answer
/// came from. `region` None is "auto".
pub fn read_located(
    frame: &Image<u8>,
    ocr: &mut dyn Recognise,
    region: Option<Region>,
) -> Result<(Vec<ChatLine>, Place), String> {
    match region {
        Some(r) => Ok((read_region(frame, ocr, r)?, Place::Set)),
        None => {
            let (fw, fh) = (frame.width, frame.height);
            let lines = read_region(frame, ocr, default_region(fw, fh, true))?;
            if !lines.is_empty() {
                return Ok((lines, Place::HelmetOn));
            }
            let lines = read_region(frame, ocr, default_region(fw, fh, false))?;
            let place = if lines.is_empty() {
                Place::Both
            } else {
                Place::HelmetOff
            };
            Ok((lines, place))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handle_sits_between_tag_and_colon() {
        let h = |t: &str| handles_in(t);
        assert_eq!(h("[GLOBAL] EXMPLSaint: o7").as_deref(), Some("EXMPLSaint"));
        assert_eq!(h("[GLOBAL) EXMPLPsy:thanks").as_deref(), Some("EXMPLPsy"));
        // the closing bracket read as I is the bracket, not the handle's
        assert_eq!(h("[GLOBALIExmplkills:").as_deref(), Some("Exmplkills"));
        assert_eq!(h("(L0CAL exmpl42m : hi").as_deref(), Some("exmpl42m"));
        assert_eq!(h("[GLOBAL] SYSTEM: x"), None);
        assert_eq!(h("[GLOBAL] 1234: x"), None);
        assert_eq!(h("BALI EXMPLHANDLE: x"), None);
        assert_eq!(
            h("[PARTY]\u{3000}EXMPL_ONE: x").as_deref(),
            Some("EXMPL_ONE")
        );
    }
}
