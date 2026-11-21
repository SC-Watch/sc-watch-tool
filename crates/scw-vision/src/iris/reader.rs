//! Reading a mask into letter pieces (IRIS's `NameplateReader`): each run of
//! inked columns is one letter, or - when wider than a letter - letters
//! joined together, split at the best-scoring path through column-ink minima.

use super::ReaderSettings;
use super::features::{CapBand, LineMask, find_cap_band, gap_cuts, glyph_vector};
use super::model::{LetterModel, argmax};

/// What a split that cannot be read scores.
pub const SPLIT_FAILURE_SCORE: f64 = -50.0;

/// A player name's characters.
pub const NAME_ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-";
/// Letters the distance font's digits are mistaken for, grouped with the digit.
pub const EFFICIENCY_GROUPS: [&str; 6] = ["0OQD", "1I7", "5S", "8B", "6G", "2Z"];
const EFFICIENCY_LETTERS: &str = "OQDISBGZ";

/// One run of inked columns, read.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// Log-probability of `text` (summed over its letters when split).
    pub score: f64,
    /// The best score had any character been allowed.
    pub free_score: f64,
}

impl Piece {
    /// Characters in the piece (its text is ASCII or the model's charset).
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Score per letter, at least one letter.
    pub fn score_per_letter(&self) -> f64 {
        self.score / self.len().max(1) as f64
    }
}

/// A mask read into pieces.
#[derive(Clone, Debug, PartialEq)]
pub struct PieceRead {
    pub pieces: Vec<Piece>,
    /// Cap height in mask pixels.
    pub cap: i32,
    /// One (start, end) per letter read, when every piece could be read.
    pub spans: Vec<(usize, usize)>,
}

/// The letter model and its two alphabets.
pub struct NameplateReader {
    pub model: LetterModel,
    pub name_alphabet: Vec<bool>,
    pub distance_alphabet: Vec<bool>,
}

impl NameplateReader {
    pub fn new(model: LetterModel) -> Result<Self, String> {
        if model.input_size() != super::features::VECTOR_LENGTH {
            return Err("the model's input does not match the glyph features".into());
        }
        let distance: String = format!("0123456789.km{EFFICIENCY_LETTERS}");
        let name_alphabet = model
            .alphabet(NAME_ALPHABET)
            .ok_or("no name character is in the model's charset")?;
        let distance_alphabet = model
            .alphabet(&distance)
            .ok_or("no distance character is in the model's charset")?;
        Ok(Self {
            model,
            name_alphabet,
            distance_alphabet,
        })
    }

    fn char_at(&self, i: usize) -> char {
        self.model.charset()[i]
    }

    pub fn read_pieces(
        &self,
        m: &LineMask,
        allowed: Option<&[bool]>,
        min_ink: f64,
        settings: &ReaderSettings,
    ) -> PieceRead {
        let band = find_cap_band(m);
        let cap = band.height();
        let capf = cap as f64;
        let mut pieces = Vec::new();
        let mut spans = Vec::new();
        for (a, b) in gap_cuts(m) {
            let ink = m.ink(a, b);
            if (ink as f64) < min_ink * capf * capf {
                continue;
            }
            if (b - a) as f64 <= 1.2 * capf {
                let Some(v) = glyph_vector(m, a, b, band) else {
                    continue;
                };
                let lp = self.model.log_probs(&v);
                let best = argmax(&lp, allowed);
                let free = if allowed.is_none() {
                    best
                } else {
                    argmax(&lp, None)
                };
                spans.push((a, b));
                pieces.push(Piece {
                    start: a,
                    end: b,
                    text: self.char_at(best).to_string(),
                    score: lp[best],
                    free_score: lp[free],
                });
            } else {
                let (text, score) = self.split(m, a, b, band, allowed, settings, &mut spans);
                pieces.push(Piece {
                    start: a,
                    end: b,
                    text,
                    score,
                    free_score: score,
                });
            }
        }
        PieceRead { pieces, cap, spans }
    }

    /// Letters joined into one blob: tries every cut at a column-ink minimum
    /// and keeps the best-scoring path through them.
    #[allow(clippy::too_many_arguments)]
    fn split(
        &self,
        m: &LineMask,
        x0: usize,
        x1: usize,
        band: CapBand,
        allowed: Option<&[bool]>,
        settings: &ReaderSettings,
        spans_out: &mut Vec<(usize, usize)>,
    ) -> (String, f64) {
        let fail = (String::new(), SPLIT_FAILURE_SCORE);
        let capf = band.height() as f64;
        let width = x1 - x0;
        if width as f64 > settings.max_piece_caps * capf {
            return fail;
        }
        let col = m.column_sums(x0, x1);
        let mut cand = vec![0usize];
        for x in 2..width.saturating_sub(2) {
            if col[x] <= col[x - 1] && col[x] <= col[x + 1] && col[x] as f64 <= 0.5 * capf {
                cand.push(x);
            }
        }
        cand.push(width);
        let (hi, lo) = (1.2 * capf, 0.25 * capf);
        let n = cand.len();
        // IRIS PR #1: count the spans first and give up on a blob with too
        // many - each one is a model call.
        let mut count = 0usize;
        for i in 0..n {
            for j in i + 1..n {
                let span = (cand[j] - cand[i]) as f64;
                if span > hi {
                    break;
                }
                if span >= lo {
                    count += 1;
                }
            }
        }
        if count > settings.max_split_spans {
            return fail;
        }
        let mut spans: Vec<(usize, usize, usize, f64)> = Vec::new();
        for i in 0..n {
            for j in i + 1..n {
                let span = (cand[j] - cand[i]) as f64;
                if span > hi {
                    break;
                }
                if span < lo {
                    continue;
                }
                let Some(v) = glyph_vector(m, x0 + cand[i], x0 + cand[j], band) else {
                    continue;
                };
                let lp = self.model.log_probs(&v);
                let best = argmax(&lp, allowed);
                spans.push((i, j, best, lp[best]));
            }
        }
        if spans.is_empty() {
            return fail;
        }
        let mut score = vec![-1e9f64; n];
        let mut back_from = vec![usize::MAX; n];
        let mut back_char = vec![0usize; n];
        score[0] = 0.0;
        for &(i, j, ch, lp) in &spans {
            let s = score[i] + lp;
            if score[i] > -1e8 && s > score[j] {
                score[j] = s;
                back_from[j] = i;
                back_char[j] = ch;
            }
        }
        let end = n - 1;
        if score[end] <= -1e8 {
            return fail;
        }
        let (mut letters, mut letter_spans) = (Vec::new(), Vec::new());
        let mut at = end;
        while at != 0 && back_from[at] != usize::MAX {
            letters.push(self.char_at(back_char[at]));
            letter_spans.push((x0 + cand[back_from[at]], x0 + cand[at]));
            at = back_from[at];
        }
        letters.reverse();
        letter_spans.reverse();
        spans_out.extend(letter_spans);
        (letters.into_iter().collect(), score[end])
    }

    /// Whether the piece between `start..end` reads as '[' (the speed group
    /// after a distance) with at least `min_score`.
    pub fn is_open_bracket(&self, m: &LineMask, start: usize, end: usize, min_score: f64) -> bool {
        let Some(bracket) = self.model.charset().iter().position(|&c| c == '[') else {
            return false;
        };
        let Some(v) = glyph_vector(m, start, end, find_cap_band(m)) else {
            return false;
        };
        let lp = self.model.log_probs(&v);
        let best = argmax(&lp, None);
        best == bracket && lp[best] >= min_score
    }
}
