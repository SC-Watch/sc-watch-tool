//! Bindings: keyboard keys, joystick buttons, and combinations of both
//! (sc-watch's inputs.py, exactly - the same spellings, labels and error
//! messages). A binding is a set of inputs that must all be held at once,
//! so `rctrl+joy0.b24` means what it looks like.
//!
//! ```text
//! A, F9, RCTRL, NUMPAD3      a keyboard key
//! joy0.b24                   joystick by driver index, button from 1
//! vid231d:0126.b7            joystick by VID/PID, survives replugging
//! ```

use std::fmt;

/// Key names and their Windows virtual-key codes (inputs.VK_CODES order).
pub fn vk_codes() -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = [
        ("TAB", 0x09),
        ("SPACE", 0x20),
        ("ENTER", 0x0D),
        ("ESC", 0x1B),
        ("SHIFT", 0x10),
        ("CTRL", 0x11),
        ("ALT", 0x12),
        ("LSHIFT", 0xA0),
        ("RSHIFT", 0xA1),
        ("LCTRL", 0xA2),
        ("RCTRL", 0xA3),
        ("LALT", 0xA4),
        ("RALT", 0xA5),
        ("CAPSLOCK", 0x14),
        ("BACKSPACE", 0x08),
        ("INSERT", 0x2D),
        ("DELETE", 0x2E),
        ("HOME", 0x24),
        ("END", 0x23),
        ("PAGEUP", 0x21),
        ("PAGEDOWN", 0x22),
        ("UP", 0x26),
        ("DOWN", 0x28),
        ("LEFT", 0x25),
        ("RIGHT", 0x27),
        ("NUMLOCK", 0x90),
        ("SCROLLLOCK", 0x91),
        ("PAUSE", 0x13),
        ("MINUS", 0xBD),
        ("EQUALS", 0xBB),
        ("LBRACKET", 0xDB),
        ("RBRACKET", 0xDD),
        ("SEMICOLON", 0xBA),
        ("QUOTE", 0xDE),
        ("COMMA", 0xBC),
        ("PERIOD", 0xBE),
        ("SLASH", 0xBF),
        ("BACKSLASH", 0xDC),
        ("GRAVE", 0xC0),
    ]
    .iter()
    .map(|&(n, c)| (n.to_string(), c))
    .collect();
    v.extend((0..10).map(|i| (format!("NUMPAD{i}"), 0x60 + i)));
    v.extend((1..25).map(|i| (format!("F{i}"), 0x6F + i)));
    v.extend(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
            .chars()
            .map(|c| (c.to_string(), c as u32)),
    );
    v
}

/// The code for a key name (any case).
pub fn vk_code(name: &str) -> Option<u32> {
    let up = name.to_uppercase();
    vk_codes()
        .into_iter()
        .find(|(n, _)| *n == up)
        .map(|(_, c)| c)
}

/// The name for a code: the first name given to it.
pub fn vk_name(vk: u32) -> Option<String> {
    vk_codes()
        .into_iter()
        .find(|(_, c)| *c == vk)
        .map(|(n, _)| n)
}

/// A joystick the driver can see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub index: u32,
    pub vid: u16,
    pub pid: u16,
    pub name: String,
    pub buttons: u32,
    pub axes: u32,
    /// More buttons than the legacy API can read (32).
    pub truncated: bool,
}

impl Device {
    pub fn ident(&self) -> String {
        format!("vid{:04x}:{:04x}", self.vid, self.pid)
    }

    pub fn describe(&self) -> String {
        let extra = if self.truncated {
            "  (more than 32 buttons - only the first 32 are readable)"
        } else {
            ""
        };
        format!(
            "joy{}  {}  {}  {} buttons, {} axes{extra}",
            self.index,
            self.ident(),
            self.name,
            self.buttons,
            self.axes
        )
    }
}

/// One input that must be held.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    Key(u32),
    /// A joystick button (1-based), by driver index or by "vidXXXX:YYYY".
    Joy {
        index: Option<u32>,
        ident: String,
        button: u32,
    },
}

impl Token {
    /// Which joystick index this means now, if any.
    pub fn resolve(&self, devs: &[Device]) -> Option<u32> {
        match self {
            Token::Key(_) => None,
            Token::Joy { ident, .. } if !ident.is_empty() => {
                devs.iter().find(|d| d.ident() == *ident).map(|d| d.index)
            }
            Token::Joy { index, .. } => *index,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Token::Key(vk) => vk_name(*vk)
                .unwrap_or_else(|| format!("vk{vk:02x}"))
                .to_lowercase(),
            Token::Joy {
                index,
                ident,
                button,
            } => {
                let who = if ident.is_empty() {
                    format!("joy{}", index.unwrap_or(0))
                } else {
                    ident.clone()
                };
                format!("{who}.b{button}")
            }
        }
    }
}

/// A binding that makes no sense; the message is the Python's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindError(pub String);

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BindError {}

/// Python's `repr()` of a string, for the error messages.
fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isdigit()` for ASCII-only use: non-empty, all 0-9.
fn digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Python's `int(s, 16)` succeeds: optional sign and 0x, hex digits with
/// single underscores between them, whitespace around.
fn py_hex(s: &str) -> bool {
    let s = s.trim();
    let s = s.strip_prefix(['+', '-']).unwrap_or(s);
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .map_or(s, |r| r.strip_prefix('_').unwrap_or(r));
    !s.is_empty()
        && !s.starts_with('_')
        && !s.ends_with('_')
        && !s.contains("__")
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '_')
}

/// A run of digits as Python's int() prints it: no leading zeros.
fn py_int(digits: &str) -> &str {
    let t = digits.trim_start_matches('0');
    if t.is_empty() { "0" } else { t }
}

/// Turns 'rctrl+joy0.b24' into tokens. Empty or "off" is no binding.
pub fn parse(spec: &str) -> Result<Vec<Token>, BindError> {
    let spec = spec.trim();
    if spec.is_empty() || spec.to_lowercase() == "off" {
        return Ok(Vec::new());
    }
    let err = |m: String| Err(BindError(m));
    let mut tokens = Vec::new();
    for part in spec.split('+') {
        let p = part.trim();
        if p.is_empty() {
            return err(format!("empty part in {}", repr(spec)));
        }
        let low = p.to_lowercase();
        if low.contains('.') && (low.starts_with("joy") || low.starts_with("vid")) {
            let (who, btn) = low.split_once('.').unwrap();
            let Some(num) = btn.strip_prefix('b').filter(|n| digits(n)) else {
                return err(format!("expected .b<number> in {}", repr(p)));
            };
            let n = py_int(num);
            let button: u32 = match n.parse() {
                Ok(b) if (1..=32).contains(&b) => b,
                _ => return err(format!("button {n} out of range 1-32 in {}", repr(p))),
            };
            if low.starts_with("joy") {
                if !digits(&who[3..]) {
                    return err(format!("expected joy<number> in {}", repr(p)));
                }
                tokens.push(Token::Joy {
                    index: Some(py_int(&who[3..]).parse().unwrap_or(u32::MAX)),
                    ident: String::new(),
                    button,
                });
            } else {
                let body = &who[3..];
                let (vid, pid) = body.split_once(':').unwrap_or((body, ""));
                if vid.chars().count() != 4 || pid.chars().count() != 4 {
                    return err(format!("expected vidXXXX:YYYY in {}", repr(p)));
                }
                if !py_hex(vid) || !py_hex(pid) {
                    return err(format!("vid/pid must be hex in {}", repr(p)));
                }
                tokens.push(Token::Joy {
                    index: None,
                    ident: who.to_string(),
                    button,
                });
            }
        } else {
            match vk_code(p) {
                Some(vk) => tokens.push(Token::Key(vk)),
                None => return err(format!("unknown key {}", repr(p))),
            }
        }
    }
    if tokens.is_empty() {
        return err("nothing to bind".into());
    }
    Ok(tokens)
}
