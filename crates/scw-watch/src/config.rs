//! What the watcher checks before it opens anything - watch._parse_bindings,
//! _parse_chat_region, _parse_focus_size - and the 'waiting for' label.

use scw_input::binding::vk_codes;
use scw_input::{Binding, InputState, Token};
use scw_store::json::{py_float_text, py_int_text, py_repr_str, py_strip};

/// A flag or setting the watcher cannot start with; shown as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// The four bindings - read, focused read, wide + centre, chat - as
/// (mode, binding), leaving out the ones that are off. Each may be a key, a
/// joystick button or a combination. In key mode the read key must be bound,
/// and no two may be bound to the same inputs.
pub fn parse_bindings(
    key: &str,
    focus_key: &str,
    dual_key: &str,
    chat_key: &str,
    key_mode: bool,
    state: &dyn InputState,
) -> Result<Vec<(String, Binding)>, ConfigError> {
    let mut binds: Vec<(String, Binding)> = Vec::new();
    for (mode, spec, flag) in [
        ("full", key, "--key"),
        ("focus", focus_key, "--focus-key"),
        ("dual", dual_key, "--dual-key"),
        ("chat", chat_key, "--chat-key"),
    ] {
        let b = Binding::new(spec, state).map_err(|e| {
            let keys: Vec<String> = {
                let mut names: Vec<String> = vk_codes().into_iter().map(|(n, _)| n).collect();
                names.sort();
                names.into_iter().take(18).collect()
            };
            ConfigError(format!(
                "{flag} {}: {e}\n  keys:      {}, ...\n  joysticks: joy0.b24, vid231d:0126.b7, rctrl+joy0.b24\n  run `python inputs.py --listen` to find yours",
                py_repr_str(spec),
                keys.join(", ")
            ))
        })?;
        if !b.is_empty() {
            binds.push((mode.to_string(), b));
        }
    }
    if key_mode && !binds.iter().any(|(m, _)| m == "full") {
        return Err(ConfigError(
            "--key cannot be 'off' - it is the main trigger".into(),
        ));
    }
    // Two bindings on the same inputs would make one unreachable.
    for (i, (mode, b)) in binds.iter().enumerate() {
        if let Some((other, _)) = binds[..i]
            .iter()
            .find(|(_, o)| same_tokens(&o.tokens, &b.tokens))
        {
            return Err(ConfigError(format!(
                "{mode} and {other} are both bound to {}; they have to differ",
                b.label()
            )));
        }
    }
    Ok(binds)
}

fn same_tokens(a: &[Token], b: &[Token]) -> bool {
    a.iter().all(|t| b.contains(t)) && b.iter().all(|t| a.contains(t))
}

/// None means "work it out from the frame"; otherwise four fractions of the
/// screen, x,y,w,h.
pub fn parse_chat_region(spec: &str) -> Result<Option<[f64; 4]>, ConfigError> {
    let s = py_strip(spec);
    if s.is_empty() || s.to_lowercase() == "auto" {
        return Ok(None);
    }
    let err = || {
        ConfigError(format!(
            "--chat-region {} should be 'auto' or four fractions between 0 and 1, like 0.15,0.30,0.35,0.45",
            py_repr_str(spec)
        ))
    };
    let parts: Vec<f64> = spec
        .split(',')
        .map(py_float_text)
        .collect::<Option<_>>()
        .ok_or_else(err)?;
    if parts.len() != 4 || !parts.iter().all(|v| (0.0..=1.0).contains(v)) {
        return Err(err());
    }
    Ok(Some([parts[0], parts[1], parts[2], parts[3]]))
}

/// (None, None) means "size it from the frame"; otherwise WxH pixels.
pub fn parse_focus_size(spec: &str) -> Result<(Option<i64>, Option<i64>), ConfigError> {
    let s = py_strip(spec).to_lowercase();
    if s.is_empty() || s == "auto" {
        return Ok((None, None));
    }
    let err = || {
        ConfigError(format!(
            "--focus-size {} should be 'auto' or look like 1200x600",
            py_repr_str(spec)
        ))
    };
    let lower = spec.to_lowercase();
    let parts: Vec<i64> = lower
        .split('x')
        .map(py_int_text)
        .collect::<Option<_>>()
        .ok_or_else(err)?;
    match parts[..] {
        [w, h] => Ok((Some(w), Some(h))),
        _ => Err(err()),
    }
}

/// 'waiting for ...' with a shared modifier chain said once:
/// `rctrl+(full:joy0.b24 focus:joy0.b25)`.
pub fn waiting_label(binds: &[(String, Binding)]) -> String {
    if binds.is_empty() {
        return "nothing".into();
    }
    let common: Vec<&Token> = if binds.len() > 1 {
        let mut c: Vec<&Token> = Vec::new();
        for t in &binds[0].1.tokens {
            if !c.contains(&t) && binds.iter().all(|(_, b)| b.tokens.contains(t)) {
                c.push(t);
            }
        }
        c
    } else {
        Vec::new()
    };
    let parts: Vec<String> = binds
        .iter()
        .map(|(mode, b)| {
            let rest: Vec<String> = b
                .tokens
                .iter()
                .filter(|t| !common.contains(t))
                .map(Token::label)
                .collect();
            let rest = if rest.is_empty() {
                b.label()
            } else {
                rest.join("+")
            };
            format!("{mode}:{rest}")
        })
        .collect();
    let mut prefix: Vec<String> = common.iter().map(|t| t.label()).collect();
    prefix.sort();
    if prefix.is_empty() {
        parts.join(" ")
    } else {
        format!("{}+({})", prefix.join("+"), parts.join(" "))
    }
}
