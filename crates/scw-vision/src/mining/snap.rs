//! `TableSnap`: a read with exactly one doubtful digit becomes the one
//! table value that digit's near-best alternatives make - or stays as it
//! was. And `PerValueCooldown`, which keeps one crop per snapped value.

use std::collections::HashMap;

use super::table::MiningTable;
use super::{MiningSettings, SignalRead, SignalState, is_number, net_min, parse_signal};

/// The snapped read, or None when the read stays as it was.
pub fn apply(
    read: &SignalRead,
    table: &MiningTable,
    settings: &MiningSettings,
) -> Option<SignalRead> {
    if !read.found() || !is_number(&read.text) {
        return None;
    }
    let scores = read.digit_scores.as_ref().filter(|s| !s.is_empty())?;
    if read.text.bytes().filter(u8::is_ascii_digit).count() != scores.len() {
        return None;
    }
    let mut best = vec![0usize; scores.len()];
    let mut uncertain: Option<usize> = None;
    for (place, s) in scores.iter().enumerate() {
        let (mut b, mut second): (usize, Option<usize>) = (0, None);
        for d in 1..10 {
            if s[d] > s[b] {
                second = Some(b);
                b = d;
            } else if second.is_none_or(|x| s[d] > s[x]) {
                second = Some(d);
            }
        }
        best[place] = b;
        if s[b] < settings.min_digit_score {
            return None;
        }
        let second = second.expect("ten scores");
        if s[b] - s[second] < settings.confident_margin {
            if uncertain.is_some() {
                return None;
            }
            uncertain = Some(place);
        }
    }
    let uncertain = uncertain?;
    let choices = &scores[uncertain];
    let mut found: Option<(i32, String, usize)> = None;
    for d in 0..10 {
        if choices[d] < choices[best[uncertain]] - settings.snap_choice_margin
            || choices[d] < settings.min_digit_score
        {
            continue;
        }
        let text = with_digit(&read.text, uncertain, d);
        let Some(value) = parse_signal(&text).filter(|&v| table.contains(v)) else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        found = Some((value, text, d));
    }
    let (snapped, text, digit) = found?;
    if read.state == SignalState::Signal && read.value == Some(snapped) {
        return None;
    }
    let mut min_score = choices[digit];
    for (place, s) in scores.iter().enumerate() {
        if place != uncertain {
            min_score = net_min(min_score, s[best[place]]);
        }
    }
    Some(SignalRead {
        state: SignalState::Signal,
        text,
        value: Some(snapped),
        min_score,
        ..read.clone()
    })
}

/// The text with its `place`-th digit (commas not counted) made `digit`.
fn with_digit(text: &str, place: usize, digit: usize) -> String {
    let mut seen = 0;
    text.chars()
        .map(|c| {
            if !c.is_ascii_digit() {
                return c;
            }
            seen += 1;
            if seen - 1 == place {
                (b'0' + digit as u8) as char
            } else {
                c
            }
        })
        .collect()
}

/// `PerValueCooldown`: at most one take per value per cooldown.
#[derive(Clone, Debug)]
pub struct PerValueCooldown {
    cooldown_ms: f64,
    last_ms: HashMap<i32, f64>,
}

impl PerValueCooldown {
    pub fn new(cooldown_ms: f64) -> Self {
        Self {
            cooldown_ms,
            last_ms: HashMap::new(),
        }
    }

    pub fn try_take(&mut self, value: i32, frame_ms: f64) -> bool {
        if let Some(&last) = self.last_ms.get(&value)
            && frame_ms - last < self.cooldown_ms
            && frame_ms >= last
        {
            return false;
        }
        self.last_ms.insert(value, frame_ms);
        true
    }
}
