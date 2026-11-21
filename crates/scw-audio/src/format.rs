//! Python's `string.Formatter` as audio.py uses it: parsing a phrase into
//! literal text and fields (CPython's MarkupIterator, the same rules and
//! error messages), and filling the fields from a table of words.

/// One piece of a template: literal text, then optionally a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub literal: String,
    /// The field's name, when this piece has a field.
    pub field: Option<String>,
    pub format_spec: String,
    pub conversion: Option<char>,
}

/// `string.Formatter().parse(template)`, or its ValueError's message.
pub fn parse(template: &str) -> Result<Vec<Piece>, String> {
    let s: Vec<char> = template.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        // Literal text up to a '{' or '}'.
        let start = i;
        let mut c = '\0';
        let mut markup = false;
        while i < s.len() {
            c = s[i];
            i += 1;
            if c == '{' || c == '}' {
                markup = true;
                break;
            }
        }
        let at_end = i >= s.len();
        let mut len = i - start;
        if c == '}' && markup && (at_end || s[i] != '}') {
            return Err("Single '}' encountered in format string".into());
        }
        if at_end && c == '{' && markup {
            return Err("Single '{' encountered in format string".into());
        }
        if markup && !at_end {
            if s[i] == c {
                // '{{' or '}}': one literal brace, no field.
                i += 1;
                markup = false;
            } else {
                len -= 1;
            }
        }
        let literal: String = s[start..start + len].iter().collect();
        if !markup {
            out.push(Piece {
                literal,
                field: None,
                format_spec: String::new(),
                conversion: None,
            });
            continue;
        }
        let (name, format_spec, conversion) = parse_field(&s, &mut i)?;
        out.push(Piece {
            literal,
            field: Some(name),
            format_spec,
            conversion,
        });
    }
    Ok(out)
}

/// CPython's parse_field: the name up to '}', ':' or '!' ('{' refused,
/// '[...]' skipped), then an optional conversion and a format spec whose
/// braces nest. `i` is just past the opening brace; on return, just past
/// the closing one.
fn parse_field(s: &[char], i: &mut usize) -> Result<(String, String, Option<char>), String> {
    let name_start = *i;
    let mut c = '\0';
    while *i < s.len() {
        c = s[*i];
        *i += 1;
        match c {
            '{' => return Err("unexpected '{' in field name".into()),
            '[' => {
                while *i < s.len() && s[*i] != ']' {
                    *i += 1;
                }
                continue;
            }
            '}' | ':' | '!' => break,
            _ => continue,
        }
    }
    // The loop ended at the end of the string, not on a terminator.
    if !matches!(c, '}' | ':' | '!') {
        return Err("expected '}' before end of string".into());
    }
    let name: String = s[name_start..*i - 1].iter().collect();
    if c == '}' {
        return Ok((name, String::new(), None));
    }
    let mut conversion = None;
    if c == '!' {
        if *i >= s.len() {
            return Err("end of string while looking for conversion specifier".into());
        }
        conversion = Some(s[*i]);
        *i += 1;
        if *i < s.len() {
            let n = s[*i];
            *i += 1;
            if n == '}' {
                return Ok((name, String::new(), conversion));
            }
            if n != ':' {
                return Err("expected ':' after conversion specifier".into());
            }
        }
    }
    let spec_start = *i;
    let mut count = 1;
    while *i < s.len() {
        let ch = s[*i];
        *i += 1;
        if ch == '{' {
            count += 1;
        } else if ch == '}' {
            count -= 1;
            if count == 0 {
                let spec: String = s[spec_start..*i - 1].iter().collect();
                return Ok((name, spec, conversion));
            }
        }
    }
    Err("unmatched '{' in format spec".into())
}

/// `Formatter().vformat(template, (), blank_defaulting(words))` for what
/// audio.py's phrases can hold: named fields, filled from `words`, a missing
/// one blank. None where Python fails - a positional or numbered field
/// (IndexError), bad braces (ValueError) - and also where it would format
/// specially: a conversion, a format spec, or an attribute or index on the
/// name. The settings page never saves those (check_phrase refuses them),
/// and in Python most of them end in silence too.
pub fn fill(template: &str, words: &dyn Fn(&str) -> String) -> Option<String> {
    let pieces = parse(template).ok()?;
    let mut out = String::new();
    for p in pieces {
        out.push_str(&p.literal);
        if let Some(name) = p.field {
            if name.is_empty()
                || name.chars().all(|c| c.is_ascii_digit())
                || name.contains(['.', '['])
                || p.conversion.is_some()
                || !p.format_spec.is_empty()
            {
                return None;
            }
            out.push_str(&words(&name));
        }
    }
    Some(out)
}
