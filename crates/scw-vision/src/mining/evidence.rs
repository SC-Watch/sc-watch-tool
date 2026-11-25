//! How sure a read is (`SignalConfidence`), several reads' digit scores
//! averaged into one (`DigitEvidence`), and the label's colours following
//! the HUD slowly while staying readable (`LabelColours`). From IRIS's
//! MiningFusion.cs; its frame average belongs with the reader on images.

use super::{
    Bgr, MiningSettings, SignalRead, SignalState, is_number, net_min, parse_signal, round_even,
};

/// `SignalConfidence.MinMargin`: the smallest lead any digit's best score
/// has over its second.
pub fn min_margin(read: &SignalRead) -> f64 {
    let Some(scores) = read.digit_scores.as_ref().filter(|s| !s.is_empty()) else {
        return 0.0;
    };
    let mut min = f64::INFINITY;
    for digit in scores {
        let (mut best, mut second) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &s in digit {
            if s > best {
                second = best;
                best = s;
            } else if s > second {
                second = s;
            }
        }
        min = net_min(min, best - second);
    }
    min
}

/// `SignalConfidence.IsConfident`.
pub fn is_confident(read: &SignalRead, margin: f64) -> bool {
    read.state == SignalState::Signal && min_margin(read) >= margin
}

/// `DigitEvidence`: the digit scores of the last reads with the same
/// layout (digits and commas in the same places).
#[derive(Clone, Debug)]
pub struct DigitEvidence {
    max_reads: usize,
    reads: Vec<Vec<[f64; 10]>>,
    layout: Option<String>,
}

impl DigitEvidence {
    pub fn new(max_reads: usize) -> Self {
        Self {
            max_reads: max_reads.max(1),
            reads: Vec::new(),
            layout: None,
        }
    }

    pub fn len(&self) -> usize {
        self.reads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reads.is_empty()
    }

    pub fn reset(&mut self) {
        self.reads.clear();
        self.layout = None;
    }

    pub fn add(&mut self, read: &SignalRead) {
        if !read.found() {
            return;
        }
        let Some(scores) = read.digit_scores.as_ref().filter(|s| !s.is_empty()) else {
            return;
        };
        let layout = layout(&read.text);
        if layout.chars().filter(|&c| c == '#').count() != scores.len() {
            return;
        }
        if self.layout.as_deref() != Some(layout.as_str()) {
            self.reads.clear();
            self.layout = Some(layout);
        }
        self.reads.push(scores.clone());
        if self.reads.len() > self.max_reads {
            self.reads.remove(0);
        }
    }

    /// `Fused`: the reads' scores averaged digit by digit - None when fewer
    /// than two, or the result is no number or a digit too weak.
    pub fn fused(&self, current: &SignalRead, min_digit_score: f64) -> Option<SignalRead> {
        let layout = self.layout.as_ref()?;
        if self.reads.len() < 2 {
            return None;
        }
        let count = self.reads.len() as f64;
        let mut averaged = Vec::with_capacity(self.reads[0].len());
        let mut text = String::new();
        let mut place = 0;
        let mut min_score = f64::INFINITY;
        for c in layout.chars() {
            if c != '#' {
                text.push(c);
                continue;
            }
            let mut sum = [0f64; 10];
            for read in &self.reads {
                for (d, s) in sum.iter_mut().enumerate() {
                    *s += read[place][d] / count;
                }
            }
            let mut best = 0;
            for d in 1..10 {
                if sum[d] > sum[best] {
                    best = d;
                }
            }
            min_score = net_min(min_score, sum[best]);
            averaged.push(sum);
            text.push((b'0' + best as u8) as char);
            place += 1;
        }
        if !is_number(&text) || min_score < min_digit_score {
            return None;
        }
        let value = parse_signal(&text)?;
        Some(SignalRead {
            state: SignalState::Signal,
            text,
            value: Some(value),
            min_score,
            digit_scores: Some(averaged),
            ..current.clone()
        })
    }
}

fn layout(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

/// `LabelColours`: the label's text and background colours, blended
/// toward each confident read and published at most every
/// `colour_update_ms`, when one moved by `colour_step` or more.
#[derive(Clone, Debug)]
pub struct LabelColours {
    blend: f64,
    update_ms: f64,
    step: i32,
    min_contrast: f64,
    text: [f64; 3],
    back: [f64; 3],
    published_ms: f64,
    pub text_colour: Bgr,
    pub back_colour: Bgr,
}

impl LabelColours {
    pub fn new(settings: &MiningSettings, text: Bgr, back: Bgr) -> Self {
        Self {
            blend: settings.colour_blend,
            update_ms: settings.colour_update_ms,
            step: settings.colour_step,
            min_contrast: settings.min_contrast,
            text: [text.b as f64, text.g as f64, text.r as f64],
            back: [back.b as f64, back.g as f64, back.r as f64],
            published_ms: f64::NEG_INFINITY,
            text_colour: text,
            back_colour: back,
        }
    }

    /// True when the published colours changed.
    pub fn add(&mut self, text: Bgr, back: Bgr, now_ms: f64) -> bool {
        let a = self.blend;
        let blend = |v: &mut [f64; 3], c: Bgr| {
            v[0] += a * (c.b as f64 - v[0]);
            v[1] += a * (c.g as f64 - v[1]);
            v[2] += a * (c.r as f64 - v[2]);
        };
        blend(&mut self.text, text);
        blend(&mut self.back, back);
        if now_ms - self.published_ms < self.update_ms {
            return false;
        }
        let new_text = to_bgr(self.text);
        let new_back = readable(new_text, to_bgr(self.back), self.min_contrast);
        if distance(new_text, self.text_colour) < self.step
            && distance(new_back, self.back_colour) < self.step
        {
            return false;
        }
        self.text_colour = new_text;
        self.back_colour = new_back;
        self.published_ms = now_ms;
        true
    }
}

fn to_bgr(v: [f64; 3]) -> Bgr {
    let byte = |x: f64| round_even(x).clamp(0.0, 255.0) as u8;
    Bgr::new(byte(v[0]), byte(v[1]), byte(v[2]))
}

/// `Readable`: the background darkened by a fifth at a time (at most 20
/// times) until the text stands out by `min_ratio` (WCAG's contrast ratio).
pub fn readable(text: Bgr, mut back: Bgr, min_ratio: f64) -> Bgr {
    let lt = luminance(text);
    for _ in 0..20 {
        let lb = luminance(back);
        if (lt.max(lb) + 0.05) / (lt.min(lb) + 0.05) >= min_ratio {
            return back;
        }
        let darker = |v: u8| (v as f64 * 0.8) as u8;
        back = Bgr::new(darker(back.b), darker(back.g), darker(back.r));
    }
    back
}

fn luminance(c: Bgr) -> f64 {
    let lin = |v: u8| {
        let s = v as f64 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

fn distance(a: Bgr, b: Bgr) -> i32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).abs();
    d(a.b, b.b).max(d(a.g, b.g)).max(d(a.r, b.r))
}
