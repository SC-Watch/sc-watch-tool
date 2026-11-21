//! Saved frames instead of the screen (the Python's ReplaySource), and
//! loading them.

use std::path::{Path, PathBuf};

use scw_vision::Image;

use crate::{CaptureError, Source};

/// A PNG as OpenCV's `imread(IMREAD_COLOR)` gives it: 8-bit BGR (alpha
/// dropped, grey spread to three channels, 16-bit cut to its high byte).
/// None when the file is not a PNG.
pub fn load_png(path: &Path) -> Result<Option<Image<u8>>, CaptureError> {
    let err = |e: &dyn std::fmt::Display| CaptureError::File(format!("{}: {e}", path.display()));
    let file = std::fs::File::open(path).map_err(|e| err(&e))?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let Ok(mut r) = dec.read_info() else {
        return Ok(None);
    };
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).map_err(|e| err(&e))?;
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

/// Saved frames, in order, one per grab.
pub struct ReplaySource {
    paths: Vec<PathBuf>,
    next: usize,
}

impl ReplaySource {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths, next: 0 }
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

impl Source for ReplaySource {
    fn grab(&mut self) -> Result<Option<Image<u8>>, CaptureError> {
        let Some(path) = self.paths.get(self.next) else {
            return Ok(None);
        };
        self.next += 1;
        match load_png(path)? {
            Some(img) => Ok(Some(img)),
            None => Err(CaptureError::File(format!(
                "{}: not a frame this can read (8-bit PNG)",
                path.display()
            ))),
        }
    }

    fn describe(&self) -> String {
        format!("replay of {} frames", self.paths.len())
    }

    fn exhausted(&self) -> bool {
        self.next >= self.paths.len()
    }
}
