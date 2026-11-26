//! The support bundle against debuglog.build_bundle itself
//! (tests/fixtures/make_bundle_ops.py): the same data folder - logs,
//! frames, crops, audits, old bundles, written here with the same bytes and
//! times - bundled the same ways gives the same answer, the same zip
//! entries in the same order with the same contents, and leaves the same
//! files behind.
//!
//! Re-encoded frames are the one difference: OpenCV's JPEG there,
//! jpeg-encoder's here. They are compared by being a JPEG of the source's
//! size.

use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;
use chrono::NaiveDateTime;
use filetime::FileTime;
use scw_store::bundle::{self, Options};
use scw_store::flags::Flags;
use serde_json::{Value, json};

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn fixture() -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundle_ops.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("scw-bundle-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[allow(deprecated)]
fn make(root: &Path, tree: &Value) {
    for f in tree.as_array().unwrap() {
        let p = root.join(f["path"].as_str().unwrap());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let data = base64::engine::general_purpose::STANDARD
            .decode(f["b64"].as_str().unwrap())
            .unwrap();
        std::fs::write(&p, data).unwrap();
        let t = f["mtime"].as_f64().unwrap();
        let (s, frac) = (t.floor(), t - t.floor());
        filetime::set_file_mtime(
            &p,
            FileTime::from_unix_time(s as i64, (frac * 1e9).round() as u32),
        )
        .unwrap();
    }
}

/// A JPEG's width and height, from its frame header.
fn jpeg_size(b: &[u8]) -> Option<(u16, u16)> {
    if b.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2;
    while at + 4 <= b.len() {
        if b[at] != 0xFF {
            return None;
        }
        let marker = b[at + 1];
        let len = u16::from_be_bytes([b[at + 2], b[at + 3]]) as usize;
        if matches!(marker, 0xC0..=0xC3) {
            let h = u16::from_be_bytes([b[at + 5], b[at + 6]]);
            let w = u16::from_be_bytes([b[at + 7], b[at + 8]]);
            return Some((w, h));
        }
        at += 2 + len;
    }
    None
}

#[test]
fn bundles_as_debuglog_does() {
    let fx = fixture();
    let now =
        NaiveDateTime::parse_from_str(fx["now"].as_str().unwrap(), "%Y-%m-%dT%H:%M:%S").unwrap();
    let mut checked = 0;
    for (i, sc) in fx["scenarios"].as_array().unwrap().iter().enumerate() {
        let name = sc["name"].as_str().unwrap();
        let root = temp(&format!("s{i}"));
        if sc["with_config"].as_bool().unwrap() {
            std::fs::write(
                root.join("settings.json"),
                serde_json::to_string_pretty(&fx["settings"]).unwrap(),
            )
            .unwrap();
            std::fs::write(
                root.join("flags.json"),
                serde_json::to_string_pretty(&fx["flags"]).unwrap(),
            )
            .unwrap();
        }
        make(&root, &sc["tree"]);
        let settings = scw_store::settings::load(&root.join("settings.json"));
        let flags = Flags::load(&root.join("flags.json"));
        let opts = Options {
            with_frames: sc["with_frames"].as_bool().unwrap(),
            budget_mb: sc["budget_mb"].as_f64().unwrap(),
            dest: None,
            // the Python's words, so the manifest can be compared whole
            text_only_command: "watch.py --support-bundle --no-frames".into(),
        };
        let info = bundle::build(&root, &settings, &flags, now, &opts);
        assert_eq!(info["ok"], json!(true), "{name}: {info:?}");

        let file = PathBuf::from(info["file"].as_str().unwrap());
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        assert_eq!(rel, sc["file"].as_str().unwrap(), "{name}: where");
        let mut got = info.clone();
        for k in ["file", "bytes", "mb"] {
            got.remove(k);
        }
        assert!(
            same(&Value::Object(got.clone()), &sc["info"]),
            "{name}: the answer\n  rust   {got:?}\n  python {}",
            sc["info"]
        );
        assert!(info["mb"].as_f64().is_some() && info["bytes"].as_u64().unwrap() > 0);

        let mut z = zip::ZipArchive::new(std::fs::File::open(&file).unwrap()).unwrap();
        let want = sc["entries"].as_array().unwrap();
        assert_eq!(z.len(), want.len(), "{name}: how many entries");
        for (j, e) in want.iter().enumerate() {
            let mut f = z.by_index(j).unwrap();
            let ename = f.name().unwrap().to_string();
            let mut data = Vec::new();
            f.read_to_end(&mut data).unwrap();
            assert_eq!(ename, e["name"].as_str().unwrap(), "{name}: entry {j}");
            if let Some(t) = e.get("text") {
                let text = String::from_utf8(data).unwrap();
                if ename == "settings.json" {
                    // without the settings only the port has (the mining
                    // overlay), which settings.py does not know
                    let mut ours: Value = serde_json::from_str(&text).unwrap();
                    for k in scw_store::settings::RUST_ONLY {
                        ours.as_object_mut().unwrap().shift_remove(*k);
                    }
                    let theirs: Value = serde_json::from_str(t.as_str().unwrap()).unwrap();
                    assert_eq!(ours, theirs, "{name}: {ename}");
                } else {
                    assert_eq!(text, t.as_str().unwrap(), "{name}: {ename}");
                }
            } else if let Some(s) = e.get("sha1") {
                assert_eq!(
                    sha1_smol::Sha1::from(&data).digest().to_string(),
                    s.as_str().unwrap(),
                    "{name}: {ename} untouched"
                );
            } else {
                let wh = e["jpeg"].as_array().unwrap();
                let size = (
                    wh[0].as_u64().unwrap() as u16,
                    wh[1].as_u64().unwrap() as u16,
                );
                assert_eq!(
                    jpeg_size(&data),
                    Some(size),
                    "{name}: {ename} a JPEG of the frame's size"
                );
            }
            checked += 1;
        }

        let mut left: Vec<String> = std::fs::read_dir(root.join("debug_bursts"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        left.sort();
        let want: Vec<String> = sc["left"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(left, want, "{name}: the files left");
        let _ = std::fs::remove_dir_all(&root);
    }
    assert_eq!(checked, 50, "every entry of every scenario compared");
}

#[test]
fn report_says_what_watch_py_prints() {
    let mut info = serde_json::Map::new();
    for (k, v) in [
        ("file", json!("/data/debug_bursts/sc-watch-support-x.zip")),
        ("images", json!(3)),
        ("images_available", json!(6)),
        ("ok", json!(true)),
        ("mb", json!(12.3)),
        ("manifest", json!("MANIFEST")),
    ] {
        info.insert(k.into(), v);
    }
    let (text, ok) = bundle::report(&info, 24.0);
    assert!(ok);
    assert_eq!(
        text,
        "MANIFEST\n\n  written: /data/debug_bursts/sc-watch-support-x.zip\n  size:    12.3 MB\n  \
         note:    3 of 6 screenshots included (newest first, 24 MB budget)"
    );
    info.insert("ok".into(), json!(false));
    info.insert("error".into(), json!("disk full"));
    let (text, ok) = bundle::report(&info, 24.0);
    assert!(!ok);
    assert_eq!(text, "MANIFEST\n\n  FAILED: disk full");
}
