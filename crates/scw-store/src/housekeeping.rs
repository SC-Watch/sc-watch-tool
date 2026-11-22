//! Retention for captured frames, and the audit pruning - housekeeping.py.
//!
//! Frames are swept only in the managed directories, never the reference
//! data (frames/, corpus/, evidence/, glyphs*/), and never below the newest
//! `keep_newest` of each. Audits are pruned by DATABASE ROW, never by
//! scanning audit/: the row owns its images.
//!
//! The settings come in as an argument - the loaded settings.json values -
//! rather than being read here, so there is one owner of settings.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::json::{Expect, coerce, py_strip, read, truthy, write};
use crate::reputation::Database;

/// The directories the tool writes frames into. Anything else is left alone.
pub const MANAGED: [&str; 4] = ["live_frames", "debug_bursts", "probe_frames", "detections"];
/// Crops land as .png, full frames as .jpg.
pub const PATTERNS: [&str; 2] = ["png", "jpg"];

#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
pub struct Policy {
    pub enabled: bool,
    pub max_age_hours: f64,
    /// A hard floor so a busy session cannot fill the disk; 0 = off.
    pub max_files_per_dir: i64,
    /// Never delete below this many, however old.
    pub keep_newest: i64,
    pub directories: Vec<String>,
    pub audit_max_age_days: f64,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            enabled: true,
            max_age_hours: 24.0,
            max_files_per_dir: 0,
            keep_newest: 20,
            directories: MANAGED.iter().map(|s| s.to_string()).collect(),
            audit_max_age_days: 7.0,
        }
    }
}

/// Python's `float(v)`: numbers, bools, numeric text.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(*b as u8 as f64),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = py_strip(s).replace('_', "");
            match t.to_ascii_lowercase().as_str() {
                "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
                "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
                "nan" | "+nan" | "-nan" => Some(f64::NAN),
                _ => t.parse().ok(),
            }
        }
        _ => None,
    }
}

/// Python's `int(v)`: integers, bools, floats (truncated), integer text.
fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite())
                .map(|f| f.trunc() as i64)
        }),
        Value::String(s) => py_strip(s).replace('_', "").parse().ok(),
        _ => None,
    }
}

impl Policy {
    /// The retention settings: the legacy housekeeping.json first (an install
    /// from before the settings tab), then `settings` - settings.json's
    /// values, None if it could not be read - over it, field by field, until
    /// one will not convert. The managed directories are always swept.
    pub fn load(legacy: &Path, settings: Option<&Map<String, Value>>) -> Policy {
        let mut pol = Policy::default();
        if let Some(Value::Object(old)) = read(legacy, Expect::Object) {
            let mut cur = serde_json::to_value(&pol).unwrap();
            for (k, v) in cur.as_object_mut().unwrap() {
                if let Some(given) = old.get(k) {
                    *v = coerce(v, given);
                }
            }
            if let Ok(p) = serde_json::from_value::<PolicyLoose>(cur) {
                pol = p.into_policy();
            }
        }
        if let Some(s) = settings {
            let _ = (|| -> Option<()> {
                pol.enabled = truthy(Some(s.get("housekeeping_enabled")?));
                pol.max_age_hours = py_float(s.get("max_age_hours")?)?;
                pol.keep_newest = py_int(s.get("keep_newest")?)?;
                pol.max_files_per_dir = py_int(s.get("max_files_per_dir")?)?;
                pol.audit_max_age_days = py_float(s.get("audit_max_age_days")?)?;
                Some(())
            })();
        }
        for m in MANAGED {
            if !pol.directories.iter().any(|d| d == m) {
                pol.directories.push(m.into());
            }
        }
        let mut seen = std::collections::HashSet::new();
        pol.directories.retain(|d| seen.insert(d.clone()));
        pol
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        write(path, &serde_json::to_value(self).unwrap())
    }
}

/// The policy as coerced from a legacy file: the directory list may hold
/// anything, and only its text entries are kept.
#[derive(serde::Deserialize)]
struct PolicyLoose {
    enabled: bool,
    max_age_hours: f64,
    max_files_per_dir: i64,
    keep_newest: i64,
    directories: Vec<Value>,
    audit_max_age_days: f64,
}

impl PolicyLoose {
    fn into_policy(self) -> Policy {
        Policy {
            enabled: self.enabled,
            max_age_hours: self.max_age_hours,
            max_files_per_dir: self.max_files_per_dir,
            keep_newest: self.keep_newest,
            directories: self
                .directories
                .into_iter()
                .filter_map(|d| d.as_str().map(String::from))
                .collect(),
            audit_max_age_days: self.audit_max_age_days,
        }
    }
}

/// One directory's sweep.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Sweep {
    pub directory: String,
    pub examined: usize,
    pub deleted: usize,
    pub bytes_freed: u64,
    pub kept: usize,
}

/// Does a name match `*.<ext>` - case-blind on Windows, as pathlib's glob is.
fn matches(name: &str, ext: &str) -> bool {
    let tail = format!(".{ext}");
    if cfg!(windows) {
        name.to_lowercase().ends_with(&tail)
    } else {
        name.ends_with(&tail)
    }
}

fn mtime(m: &std::fs::Metadata) -> Option<f64> {
    let t = m.modified().ok()?;
    Some(match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    })
}

/// Sweeps the policy's directories under `root` at `now`; with `apply`,
/// deletes. A file that vanishes or will not delete is never an error -
/// the watcher may be writing into the same directory.
pub fn sweep(policy: &Policy, root: &Path, now: f64, apply: bool) -> Vec<Sweep> {
    let mut results = Vec::new();
    if !policy.enabled {
        return results;
    }
    let cutoff = now - policy.max_age_hours * 3600.0;
    for name in &policy.directories {
        let d = root.join(name);
        if !d.is_dir() {
            continue;
        }
        let listed: Vec<PathBuf> = std::fs::read_dir(&d)
            .map(|it| it.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        // stat() once per file, up front, and tolerate it being gone.
        let mut stats: Vec<(PathBuf, f64, u64)> = Vec::new();
        for ext in PATTERNS {
            for f in &listed {
                let n = f
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !matches(&n, ext) {
                    continue;
                }
                if let Ok(m) = std::fs::metadata(f)
                    && let Some(t) = mtime(&m)
                {
                    stats.push((f.clone(), t, m.len()));
                }
            }
        }
        // Newest first; equal times keep their listing order.
        stats.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut res = Sweep {
            directory: name.clone(),
            examined: stats.len(),
            ..Default::default()
        };
        // stats[:keep_newest], as a Python slice: a negative count is
        // "all but the last n".
        let k = policy.keep_newest;
        let protect = if k >= 0 {
            (k as usize).min(stats.len())
        } else {
            stats.len().saturating_sub(k.unsigned_abs() as usize)
        };
        for (i, (f, t, size)) in stats.iter().enumerate() {
            if i < protect {
                res.kept += 1;
                continue;
            }
            let too_old = *t < cutoff;
            let too_many = policy.max_files_per_dir != 0 && i as i64 >= policy.max_files_per_dir;
            if !(too_old || too_many) {
                res.kept += 1;
                continue;
            }
            if apply && std::fs::remove_file(f).is_err() {
                res.kept += 1;
                continue;
            }
            res.deleted += 1;
            res.bytes_freed += size;
        }
        results.push(res);
    }
    results
}

/// One line for the console and the page.
pub fn summarise(results: &[Sweep], applied: bool) -> String {
    if results.is_empty() {
        return "housekeeping: nothing to do".into();
    }
    let total: u64 = results.iter().map(|r| r.bytes_freed).sum();
    let n: usize = results.iter().map(|r| r.deleted).sum();
    let verb = if applied { "freed" } else { "would free" };
    let parts: Vec<String> = results
        .iter()
        .filter(|r| r.examined > 0)
        .map(|r| {
            format!(
                "{}: {}/{} ({} MB)",
                r.directory,
                r.deleted,
                r.examined,
                mb(r.bytes_freed)
            )
        })
        .collect();
    format!(
        "housekeeping {verb} {} MB across {n} file(s) | {}",
        mb(total),
        parts.join(" | ")
    )
}

/// `f"{bytes/1e6:.0f}"`.
fn mb(bytes: u64) -> String {
    format!("{:.0}", bytes as f64 / 1e6)
}

/// Deletes stale audits and any image no row points at: (audits removed,
/// orphan files removed). A dry run counts.
pub fn prune_audit_rows(
    db: &Database,
    policy: &Policy,
    apply: bool,
) -> rusqlite::Result<(usize, usize)> {
    let orphans = db.prune_orphan_audit_files(apply)?;
    // Audits whose player row is gone: no tile can offer a button for them.
    let dangling = db.orphan_audits(apply)?;
    let removed = if apply {
        db.prune_audits(policy.audit_max_age_days)?.removed
    } else {
        let cutoff = db.now() - policy.audit_max_age_days * 86400.0;
        db.open_audits()?
            .iter()
            .filter(|a| {
                a.get("ts")
                    .and_then(Value::as_f64)
                    .is_some_and(|t| t < cutoff)
            })
            .count()
    };
    Ok((removed + dangling.len(), orphans.len()))
}

/// One sweep of everything, as a single line - what the watcher runs at
/// startup, so a failure here never stops a session. `db` is None when
/// sc-watch.db could not be opened.
pub fn run_now(
    policy: &Policy,
    root: &Path,
    db: Option<&Database>,
    now: f64,
    apply: bool,
) -> String {
    let mut line = summarise(&sweep(policy, root, now, apply), apply);
    match db.map(|db| prune_audit_rows(db, policy, apply)) {
        Some(Ok((audits, orphans))) => {
            if audits > 0 || orphans > 0 {
                line.push_str(&format!(
                    " | audits: {audits} pruned, {orphans} orphan file(s)"
                ));
            }
        }
        Some(Err(_)) => line.push_str(" | audits skipped (DatabaseError)"),
        None => line.push_str(" | audits skipped (no database)"),
    }
    line
}
