//! Finding and loading saved frames.

use std::path::{Path, PathBuf};

use scw_vision::Image;

/// A PNG as OpenCV's `imread(IMREAD_COLOR)` gives it (scw-capture's loader);
/// None when the file is not a PNG.
pub fn load(path: &Path) -> Result<Option<Image<u8>>, String> {
    scw_capture::replay::load_png(path).map_err(|e| e.to_string())
}

pub use scw_capture::replay::expand;

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
