//! The OpenCV functions sc-watch's readers use, reimplemented to give the
//! same bytes as OpenCV 5.0.0 (tests/opencv_parity.rs). 8-bit images unless
//! a name says otherwise; morphology, blur and labelling take one channel.

mod basic;
mod blur;
mod components;
mod morph;
mod resize;

pub use basic::{
    bgr_to_gray, bounding_rect, count_nonzero, gray_to_bgr, lut, otsu_threshold, subtract,
    threshold_binary,
};
pub use blur::blur;
pub use components::{Stats, connected_components};
pub use morph::{dilate, erode, open, tophat};
pub use resize::{
    resize_area_to, resize_cubic_by, resize_cubic_f32_by, resize_cubic_to, resize_linear_to,
    scaled_size,
};
