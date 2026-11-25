//! IRIS's nameplate reader, ported from IRIS (github.com/Niteb1rd/IRIS,
//! GPL-3.0) at commit aec86f08 with the reading fixes of IRIS pull request
//! #1, by way of sc-watch's Python port (iris_reader.py), which matches IRIS's
//! own readings on the real corpus - plus IRIS 2.1.0's line join (BandJoin,
//! [`DetectorSettings::band_join`]), held to IRIS's own output directly. The
//! letter model file is IRIS's, unchanged.
//!
//! Given an image and the box of ONE text row, read it as a player name or
//! as a distance line ([`RowReader`]); find the rows and read whole plates
//! ([`NameplateDetector`], [`ImageOps`]).

pub mod detector;
pub mod features;
pub mod model;
pub mod ops;
pub mod reader;
pub mod rules;

pub use detector::{Candidates, DetectorSettings, NameplateDetector};
pub use features::{CapBand, LineMask, create_mask};
pub use model::{LetterModel, ModelError};
pub use ops::{ImageOps, ReadClass, ReadResult, ScanGeometry, boundary_from_percents};
pub use reader::{NameplateReader, Piece, PieceRead};
pub use rules::DistanceReading;

use crate::{Image, Rect};

/// The reading tunables, IRIS's `NameplateDetectorSettings` defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct ReaderSettings {
    /// Ink a name letter needs, in cap heights squared.
    pub name_min_ink: f64,
    /// The same for a distance line, whose digits are thinner.
    pub distance_min_ink: f64,
    /// A gap wider than this (in cap heights) separates words.
    pub word_gap: f64,
    /// The least score a '[' needs to start a speed group.
    pub bracket_min_score: f64,
    /// Blobs wider than this many cap heights are not split.
    pub max_piece_caps: f64,
    /// Blobs with more candidate letters than this are not split (IRIS PR #1).
    pub max_split_spans: usize,
    /// A distance line's least mean score per letter.
    pub min_distance_score: f64,
    /// How far the distance alphabet may fall below the free choice, per letter.
    pub max_distance_gap: f64,
    /// End pieces scoring below this per letter are trimmed as junk.
    pub distance_end_score: f64,
    pub max_distance_value: u32,
    /// Apply the O/0 rule to names.
    pub o0_rule: bool,
    /// Apply the '_'/'-' rule to names.
    pub underscore_rule: bool,
    /// Pixels added around a row's box before masking.
    pub mask_pad: i32,
}

impl Default for ReaderSettings {
    fn default() -> Self {
        Self {
            name_min_ink: 0.02,
            distance_min_ink: 0.003,
            word_gap: 0.33,
            bracket_min_score: -0.7,
            max_piece_caps: 8.0,
            max_split_spans: 500,
            min_distance_score: -0.35,
            max_distance_gap: 4.0,
            distance_end_score: -1.0,
            max_distance_value: 1000,
            o0_rule: true,
            underscore_rule: true,
            mask_pad: 2,
        }
    }
}

/// A text row read as a player name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NameRead {
    pub text: String,
    pub score: f64,
    /// No gap wider than a word gap.
    pub one_word: bool,
    /// Only A-Z, 0-9, '_' and '-'.
    pub valid_chars: bool,
    pub mask: Option<LineMask>,
    /// The O/0 rule's evidence: (letter index, probability of O).
    pub o0: Vec<(usize, f64)>,
}

impl NameRead {
    pub fn usable(&self) -> bool {
        self.mask.is_some() && !self.text.is_empty() && self.one_word && self.valid_chars
    }
}

/// Reads text rows IRIS's way.
pub struct RowReader {
    pub reader: NameplateReader,
    pub settings: ReaderSettings,
}

impl RowReader {
    pub fn new(reader: NameplateReader, settings: ReaderSettings) -> Self {
        Self { reader, settings }
    }

    /// A row's mask; `keep_colors` also keeps the colour under the ink
    /// (names do, for the plate tint; distance lines need not).
    pub fn mask(&self, bgr: &Image<u8>, rect: Rect, keep_colors: bool) -> Option<LineMask> {
        create_mask(bgr, rect, None, self.settings.mask_pad, keep_colors)
    }

    pub fn read_name(&self, bgr: &Image<u8>, rect: Rect) -> NameRead {
        self.read_name_mask(bgr, self.mask(bgr, rect, true).as_ref())
    }

    pub fn read_name_mask(&self, bgr: &Image<u8>, mask: Option<&LineMask>) -> NameRead {
        let Some(mask) = mask else {
            return NameRead::default();
        };
        let s = &self.settings;
        let read =
            self.reader
                .read_pieces(mask, Some(&self.reader.name_alphabet), s.name_min_ink, s);
        let text: String = read.pieces.iter().map(|p| p.text.as_str()).collect();
        if text.is_empty() {
            return NameRead::default();
        }
        let cap = read.cap as f64;
        let one_word = read
            .pieces
            .windows(2)
            .all(|w| w[1].start as f64 - w[0].end as f64 <= s.word_gap * cap);
        let valid_chars = text
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-');
        let score = read.pieces.iter().map(|p| p.score).sum();
        if !one_word || !valid_chars {
            return NameRead {
                text,
                score,
                one_word,
                valid_chars,
                mask: Some(mask.clone()),
                o0: Vec::new(),
            };
        }
        let mut evidence = Vec::new();
        let mut text = text;
        if s.o0_rule {
            text = rules::o0_correct(bgr, &read, mask, &text, &mut evidence);
        }
        if s.underscore_rule {
            text = rules::underscore_hyphen_correct(&read, mask, &text);
        }
        NameRead {
            text,
            score,
            one_word: true,
            valid_chars: true,
            mask: Some(mask.clone()),
            o0: evidence,
        }
    }

    pub fn read_distance(&self, bgr: &Image<u8>, rect: Rect) -> Option<DistanceReading> {
        self.read_distance_mask(self.mask(bgr, rect, false).as_ref())
    }

    pub fn read_distance_mask(&self, mask: Option<&LineMask>) -> Option<DistanceReading> {
        let mask = mask?;
        let s = &self.settings;
        let read = self.reader.read_pieces(
            mask,
            Some(&self.reader.distance_alphabet),
            s.distance_min_ink,
            s,
        );
        rules::evaluate_distance(&read, s, |p| {
            self.reader
                .is_open_bracket(mask, p.start, p.end, s.bracket_min_score)
        })
    }
}
