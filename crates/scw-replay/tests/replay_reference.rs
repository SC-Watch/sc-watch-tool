//! scw-replay against the Python pipeline end to end (reference.py) on real
//! frames: every frame's lines, contacts (name, kind, km, boxes) and
//! refusals, and every burst's tally and voted contacts - exactly.
//!
//! Real frames name real players, so the data stays off the repository:
//! every SCW_PARITY_DATA/replay/*.json is reference.py's output for the
//! frames in SCW_PARITY_DATA/iris-detect/frames.txt, with the settings it
//! records (scan area, burst, min votes). Without them the test reports that
//! it skipped and passes. scw-replay runs `--as-python`: the port's
//! deliberate improvements are off here.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn data() -> Option<(Vec<PathBuf>, PathBuf)> {
    let root = PathBuf::from(std::env::var_os("SCW_PARITY_DATA")?);
    let frames = root.join("iris-detect").join("frames.txt");
    let mut refs: Vec<PathBuf> = std::fs::read_dir(root.join("replay"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    refs.sort();
    (!refs.is_empty() && frames.is_file()).then_some((refs, frames))
}

/// Where two JSON values first differ, as a path - never the values, which
/// can be real names.
fn first_difference(path: &str, a: &Value, b: &Value) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                (Some(p), Some(q)) => first_difference(&format!("{path}.{k}"), p, q),
                _ => Some(format!("{path}.{k}: present on one side only")),
            })
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: {} items vs {}", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (p, q))| first_difference(&format!("{path}[{i}]"), p, q))
        }
        (Value::Number(p), Value::Number(q)) => {
            let same = match (p.as_f64(), q.as_f64()) {
                (Some(f), Some(g)) => f.to_bits() == g.to_bits() || f == g,
                _ => p == q,
            };
            (!same).then(|| format!("{path}: numbers differ"))
        }
        _ => (a != b).then(|| format!("{path}: values differ")),
    }
}

#[test]
fn replay_matches_the_python_pipeline() {
    let Some((refs, frames)) = data() else {
        eprintln!("skipped: SCW_PARITY_DATA holds no replay/*.json and iris-detect/frames.txt");
        return;
    };
    let mut all_ok = true;
    for expected_path in &refs {
        all_ok &= check_one(expected_path, &frames);
    }
    assert!(all_ok, "scw-replay differs from the Python pipeline");
}

fn check_one(expected_path: &Path, frames: &Path) -> bool {
    let expected: Value = serde_json::from_slice(&std::fs::read(expected_path).unwrap()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_scw-replay"))
        .arg("--frames-list")
        .arg(frames)
        .args(["--scan-area", expected["scan_area"].as_str().unwrap()])
        .args(["--burst", &expected["burst"].to_string()])
        .args(["--min-votes", &expected["min_votes"].to_string()])
        // the Python has neither IRIS 2.1's line join nor the damaged-serial
        // rule; each has its own test (iris_detect_golden, names)
        .arg("--as-python")
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "scw-replay failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: Value = serde_json::from_slice(&out.stdout).unwrap();
    let frames_n = expected["frames"].as_array().unwrap().len();
    let bursts_n = expected["bursts"].as_array().unwrap().len();
    let mut differ = Vec::new();
    for i in 0..frames_n {
        if let Some(d) = first_difference(
            &format!("frames[{i}]"),
            &expected["frames"][i],
            &got["frames"][i],
        ) {
            differ.push(d);
        }
    }
    for i in 0..bursts_n {
        if let Some(d) = first_difference(
            &format!("bursts[{i}]"),
            &expected["bursts"][i],
            &got["bursts"][i],
        ) {
            differ.push(d);
        }
    }
    let contacts: usize = expected["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["contacts"].as_array().unwrap().len())
        .sum();
    let resolved: usize = expected["bursts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["resolved"].as_array().unwrap().len())
        .sum();
    println!(
        "{}: scan area {}, burst {}, min votes {}: {frames_n} frames ({contacts} contacts), {bursts_n} bursts ({resolved} voted contacts): {} differ",
        expected_path.file_name().unwrap().to_string_lossy(),
        expected["scan_area"],
        expected["burst"],
        expected["min_votes"],
        differ.len()
    );
    for d in differ.iter().take(8) {
        println!("   {d}");
    }
    differ.is_empty()
        && got["frames"].as_array().map(Vec::len) == Some(frames_n)
        && got["bursts"].as_array().map(Vec::len) == Some(bursts_n)
}
