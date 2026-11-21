//! Finding and loading saved frames.

use std::path::{Path, PathBuf};

use scw_vision::Image;

/// A PNG as OpenCV's `imread(IMREAD_COLOR)` gives it: 8-bit BGR (alpha
/// dropped, grey spread to three channels, 16-bit cut to its high byte).
/// None when the file is not a PNG.
pub fn load(path: &Path) -> Result<Option<Image<u8>>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let Ok(mut r) = dec.read_info() else {
        return Ok(None);
    };
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r
        .next_frame(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let n = info.color_type.samples();
    let mut data = Vec::with_capacity(info.width as usize * info.height as usize * 3);
    for px in buf[..info.buffer_size()].chunks_exact(n) {
        if n < 3 {
            data.extend([px[0]; 3]);
        } else {
            data.extend([px[2], px[1], px[0]]);
        }
    }
    Ok(Some(Image::from_vec(
        info.width as usize,
        info.height as usize,
        3,
        data,
    )))
}

/// Python's `sorted(Path)`: by path components, case-blind on Windows.
fn sort_key(p: &Path) -> Vec<String> {
    p.components()
        .map(|c| {
            let s = c.as_os_str().to_string_lossy().into_owned();
            if cfg!(windows) { s.to_lowercase() } else { s }
        })
        .collect()
}

fn sorted(mut v: Vec<PathBuf>) -> Vec<PathBuf> {
    v.sort_by_cached_key(|p| sort_key(p));
    v
}

/// `*` and `?` matching (case-blind on Windows, as Python's glob there).
fn wildcard(pattern: &[char], name: &[char]) -> bool {
    match (pattern.first(), name.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            wildcard(&pattern[1..], name) || (!name.is_empty() && wildcard(pattern, &name[1..]))
        }
        (Some('?'), Some(_)) => wildcard(&pattern[1..], &name[1..]),
        (Some(p), Some(n)) if p == n => wildcard(&pattern[1..], &name[1..]),
        _ => false,
    }
}

fn fold(s: &str) -> Vec<char> {
    if cfg!(windows) {
        s.to_lowercase().chars().collect()
    } else {
        s.chars().collect()
    }
}

fn is_png(p: &Path) -> bool {
    p.extension()
        .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("png"))
}

/// One command-line argument as frames: a file, a folder's PNG files, or a
/// pattern with `*`/`?` in its file name.
pub fn expand(arg: &str) -> Result<Vec<PathBuf>, String> {
    let path = PathBuf::from(arg);
    if path.is_dir() {
        let mut v = Vec::new();
        for e in std::fs::read_dir(&path).map_err(|e| format!("{arg}: {e}"))? {
            let p = e.map_err(|e| format!("{arg}: {e}"))?.path();
            if p.is_file() && is_png(&p) {
                v.push(p);
            }
        }
        return Ok(sorted(v));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.contains(['*', '?']) {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let pat = fold(&name);
        let mut v = Vec::new();
        for e in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let p = e.map_err(|e| format!("{arg}: {e}"))?.path();
            let n = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if p.is_file() && wildcard(&pat, &fold(&n)) {
                v.push(
                    if dir == Path::new(".")
                        && path.parent().is_none_or(|d| d.as_os_str().is_empty())
                    {
                        PathBuf::from(n)
                    } else {
                        p
                    },
                );
            }
        }
        if v.is_empty() {
            return Err(format!("no frames match {arg}"));
        }
        return Ok(sorted(v));
    }
    if !path.is_file() {
        return Err(format!("{arg}: no such file or folder"));
    }
    Ok(vec![path])
}

/// Frame paths from a list file, one per line, in its order; relative paths
/// are taken from the list's folder.
pub fn from_list(list: &Path) -> Result<Vec<PathBuf>, String> {
    let text = std::fs::read_to_string(list).map_err(|e| format!("{}: {e}", list.display()))?;
    let base = list.parent().unwrap_or(Path::new("."));
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let p = PathBuf::from(l);
            if p.is_absolute() { p } else { base.join(p) }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, n: &str) -> bool {
        wildcard(&fold(p), &fold(n))
    }

    #[test]
    fn wildcards() {
        assert!(m("*.png", "burst_1.png"));
        assert!(m("burst_??.png", "burst_01.png"));
        assert!(!m("burst_??.png", "burst_1.png"));
        assert!(m("*", ""));
        assert!(!m("*.png", "burst.jpg"));
        assert!(m("a*b*c", "aXXbYc"));
    }

    #[test]
    fn sorts_by_components() {
        let v = sorted(vec![PathBuf::from("a b/x"), PathBuf::from("a/x")]);
        assert_eq!(
            v[0],
            PathBuf::from("a/x"),
            "component order, not string order"
        );
    }
}
