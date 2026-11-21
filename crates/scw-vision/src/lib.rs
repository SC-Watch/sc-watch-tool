//! sc-watch's image work, in Rust.
//!
//! [`cv`] reimplements the OpenCV functions sc-watch's readers call, checked
//! byte for byte against OpenCV 5.0.0 (tests/opencv_parity.rs, on fixtures
//! made by tests/fixtures/make_fixtures.py). Writing them rather than binding
//! OpenCV keeps the build free of a system OpenCV and libclang on Windows and
//! Linux alike.
//!
//! The IRIS nameplate finder and letter reader build on it, ported from
//! sc-watch's Python port of IRIS (github.com/Niteb1rd/IRIS, GPL-3.0).

pub mod cv;
pub mod image;
pub mod iris;
pub mod names;
pub mod pipeline;

pub use image::{Image, Rect};
