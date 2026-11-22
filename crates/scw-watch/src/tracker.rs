//! What is new on screen, and what has already been said - watch.Tracker
//! and watch.Announcer.

use std::collections::HashMap;

use scw_vision::names::vote::same_contact;

/// A contact already read, held by screen position.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub cx: f64,
    pub cy: f64,
    pub last_seen: f64,
}

/// Decides what is new: a position within `radius` of a live track is the
/// same contact; a track not seen for `expire` seconds is forgotten.
#[derive(Clone, Debug)]
pub struct Tracker {
    pub radius: f64,
    pub expire: f64,
    pub tracks: Vec<Track>,
}

impl Tracker {
    pub fn new(radius: f64, expire: f64) -> Self {
        Tracker {
            radius,
            expire,
            tracks: Vec::new(),
        }
    }

    /// Takes this frame's positions; returns how many are new.
    pub fn update(&mut self, points: &[(f64, f64)], now: f64) -> usize {
        let expire = self.expire;
        self.tracks.retain(|t| now - t.last_seen < expire);
        let mut new = 0;
        for &(cx, cy) in points {
            let mut hit = None;
            let mut best = self.radius;
            for (i, t) in self.tracks.iter().enumerate() {
                let d = ((t.cx - cx).powi(2) + (t.cy - cy).powi(2)).sqrt();
                if d < best {
                    best = d;
                    hit = Some(i);
                }
            }
            match hit {
                None => {
                    self.tracks.push(Track {
                        cx,
                        cy,
                        last_seen: now,
                    });
                    new += 1;
                }
                Some(i) => {
                    let t = &mut self.tracks[i];
                    (t.cx, t.cy, t.last_seen) = (cx, cy, now);
                }
            }
        }
        new
    }
}

/// Duplicate protection across bursts, matched as consensus matches within
/// one: a later, longer reading of a contact already announced is a
/// correction - said, and adopted as the name.
#[derive(Clone, Debug, Default)]
pub struct Announcer {
    pub cooldown: f64,
    /// Name -> when last announced, in the order first announced.
    pub last: Vec<(String, f64)>,
    pub counts: HashMap<String, usize>,
    /// The order counts were first made, for a stable summary.
    pub order: Vec<String>,
}

impl Announcer {
    pub fn new(cooldown: f64) -> Self {
        Announcer {
            cooldown,
            ..Default::default()
        }
    }

    fn matched(&self, name: &str) -> Option<usize> {
        self.last
            .iter()
            .position(|(known, _)| same_contact(name, known))
    }

    /// (announce it?, the name it corrects).
    pub fn should(&mut self, name: &str, now: f64) -> (bool, Option<String>) {
        let Some(i) = self.matched(name) else {
            self.last.push((name.to_string(), now));
            self.counts.insert(name.to_string(), 1);
            self.order.push(name.to_string());
            return (true, None);
        };
        let known = self.last[i].0.clone();
        *self.counts.entry(known.clone()).or_insert(0) += 1;
        self.last[i].1 = now;
        // A longer reading of the same contact supersedes the shorter one:
        // it moves to the end, as Python's dict pop-and-insert does.
        if name.chars().count() > known.chars().count()
            && name.to_uppercase().starts_with(&known.to_uppercase())
        {
            let (_, at) = self.last.remove(i);
            self.last.push((name.to_string(), at));
            let n = self.counts.remove(&known).unwrap_or(0);
            self.counts.insert(name.to_string(), n);
            if let Some(p) = self.order.iter().position(|o| *o == known) {
                self.order.remove(p);
            }
            self.order.push(name.to_string());
            return (true, Some(known));
        }
        (false, None)
    }

    /// Contacts and how many bursts announced or repeated each, most first
    /// (ties in the order they were counted).
    pub fn summary(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = self
            .order
            .iter()
            .map(|n| (n.clone(), self.counts.get(n).copied().unwrap_or(0)))
            .collect();
        out.sort_by_key(|e| std::cmp::Reverse(e.1));
        out
    }
}
