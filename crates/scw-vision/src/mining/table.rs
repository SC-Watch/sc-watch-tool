//! `MiningTable`: which signal is which rock. One line per resource, `base
//! signal/name/most multiples`; a signal of base x N (N up to the most)
//! reads "name Nx", and a value several resources share reads all of them,
//! joined by " / ". The table IRIS ships is models/iris-mining.txt.

use std::collections::HashMap;

use super::parse_digits;

/// The table built in (IRIS's App/Assets/mining.txt, verbatim).
pub const EMBEDDED_TEXT: &str = include_str!("../../../../models/iris-mining.txt");

/// The comment line that names the table's version.
pub const VERSION_PREFIX: &str = "# IRIS mining data ";

/// Shown for a signal above the table that is a whole thousand.
pub const MULTIPLE_LABEL: &str = "Multiple";

/// Shown for any other signal above the table.
pub const ORE_MULTIPLE_LABEL: &str = "Ore/Multiple";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MiningResource {
    pub base: i32,
    pub name: String,
    pub max_multiple: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MiningTable {
    pub resources: Vec<MiningResource>,
    /// The lines left out, and why.
    pub errors: Vec<String>,
    pub version: Option<String>,
    labels: HashMap<i32, String>,
    pub max_signal: i32,
}

impl MiningTable {
    fn new(resources: Vec<MiningResource>, errors: Vec<String>, version: Option<String>) -> Self {
        let mut lists: HashMap<i32, Vec<String>> = HashMap::new();
        for r in &resources {
            for n in 1..=r.max_multiple {
                let value = r.base as i64 * n as i64;
                if value > i32::MAX as i64 {
                    break;
                }
                lists
                    .entry(value as i32)
                    .or_default()
                    .push(format!("{} {n}x", r.name));
            }
        }
        let labels: HashMap<i32, String> =
            lists.into_iter().map(|(k, v)| (k, v.join(" / "))).collect();
        let max_signal = labels.keys().copied().max().unwrap_or(0);
        Self {
            resources,
            errors,
            version,
            labels,
            max_signal,
        }
    }

    /// `MiningTable.Empty`.
    pub fn empty() -> Self {
        Self::new(Vec::new(), Vec::new(), None)
    }

    /// `MiningTable.Parse`: never fails; a line it cannot use is left out
    /// and named in `errors`.
    pub fn parse(text: &str) -> Self {
        let mut resources = Vec::new();
        let mut errors = Vec::new();
        let mut version: Option<String> = None;
        let text = text.replace("\r\n", "\n");
        for (i, raw) in text.split('\n').enumerate() {
            let line = raw.trim();
            if let Some(v) = line.strip_prefix(VERSION_PREFIX) {
                version.get_or_insert_with(|| v.trim().to_string());
            }
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split('/').collect();
            let n = i + 1;
            if parts.len() != 3 {
                errors.push(format!(
                    "line {n}: expected base signal/name/most multiples"
                ));
                continue;
            }
            let number = parts[0].trim().replace(',', "");
            let name = parts[1].trim();
            let Some(base) = parse_digits(&number).filter(|&b| b > 0) else {
                errors.push(format!(
                    "line {n}: \"{}\" is not a signal value",
                    parts[0].trim()
                ));
                continue;
            };
            if name.is_empty() {
                errors.push(format!("line {n}: the name is empty"));
                continue;
            }
            let Some(max) = parse_digits(parts[2].trim().trim_end_matches(['x', 'X']))
                .filter(|m| (1..=99).contains(m))
            else {
                errors.push(format!(
                    "line {n}: \"{}\" is not a number of multiples (1-99)",
                    parts[2].trim()
                ));
                continue;
            };
            resources.push(MiningResource {
                base,
                name: name.to_string(),
                max_multiple: max,
            });
        }
        Self::new(resources, errors, version)
    }

    /// The table built in.
    pub fn embedded() -> Self {
        Self::parse(EMBEDDED_TEXT)
    }

    /// `IsOutdated`: an edited copy whose version differs from the
    /// built-in table's.
    pub fn is_outdated(file_text: Option<&str>) -> bool {
        match (
            file_text.and_then(|t| Self::parse(t).version),
            Self::embedded().version,
        ) {
            (Some(v), Some(shipped)) => v != shipped,
            _ => false,
        }
    }

    pub fn lookup(&self, value: i32) -> Option<&str> {
        self.labels.get(&value).map(String::as_str)
    }

    pub fn contains(&self, value: i32) -> bool {
        self.labels.contains_key(&value)
    }

    /// `AboveLimitLabel`: what a signal above the whole table is called.
    pub fn above_limit_label(&self, value: i32) -> Option<&'static str> {
        if self.max_signal <= 0 || value <= self.max_signal {
            None
        } else if value % 1000 == 0 {
            Some(MULTIPLE_LABEL)
        } else {
            Some(ORE_MULTIPLE_LABEL)
        }
    }
}
