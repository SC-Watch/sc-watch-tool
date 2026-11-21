//! A plain image: rows of pixels, channels interleaved (BGR for colour, as
//! the screen capture delivers and OpenCV uses).

/// An image of `T` samples, row-major, `channels` per pixel, no row padding.
#[derive(Clone, Debug, PartialEq)]
pub struct Image<T> {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: Vec<T>,
}

impl<T: Copy + Default> Image<T> {
    /// A new image filled with `T::default()` (zero).
    pub fn new(width: usize, height: usize, channels: usize) -> Self {
        Self::filled(width, height, channels, T::default())
    }

    pub fn filled(width: usize, height: usize, channels: usize, value: T) -> Self {
        Self {
            width,
            height,
            channels,
            data: vec![value; width * height * channels],
        }
    }

    /// Wraps existing samples; panics if the length does not fit the shape.
    pub fn from_vec(width: usize, height: usize, channels: usize, data: Vec<T>) -> Self {
        assert_eq!(
            data.len(),
            width * height * channels,
            "{width}x{height}x{channels} image needs {} samples, got {}",
            width * height * channels,
            data.len()
        );
        Self {
            width,
            height,
            channels,
            data,
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize, c: usize) -> T {
        self.data[(y * self.width + x) * self.channels + c]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, c: usize, v: T) {
        self.data[(y * self.width + x) * self.channels + c] = v;
    }

    /// One row's samples (all channels).
    #[inline]
    pub fn row(&self, y: usize) -> &[T] {
        let n = self.width * self.channels;
        &self.data[y * n..(y + 1) * n]
    }

    #[inline]
    pub fn row_mut(&mut self, y: usize) -> &mut [T] {
        let n = self.width * self.channels;
        &mut self.data[y * n..(y + 1) * n]
    }

    /// One channel as a single-channel image.
    pub fn channel(&self, c: usize) -> Image<T> {
        assert!(c < self.channels);
        let data = self
            .data
            .iter()
            .skip(c)
            .step_by(self.channels)
            .copied()
            .collect();
        Image::from_vec(self.width, self.height, 1, data)
    }

    /// A copy of the rectangle (x, y, w, h), which must lie inside the image.
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Image<T> {
        assert!(
            x + w <= self.width && y + h <= self.height,
            "crop outside the image"
        );
        let mut out = Image::new(w, h, self.channels);
        for r in 0..h {
            let src = &self.row(y + r)[x * self.channels..(x + w) * self.channels];
            out.row_mut(r).copy_from_slice(src);
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}
