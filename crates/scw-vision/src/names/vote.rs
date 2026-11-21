//! Turning a burst of noisy per-frame reads into confident identifications
//! (sc-watch's consensus.py, exactly).
//!
//! CLUSTER groups readings of one physical contact - range and screen
//! position barely move over a burst, and the names are near-identical.
//! VOTE takes each cluster's modal reading; ties go to the longer string,
//! because reading fails by dropping characters at the edges, never by
//! inventing them. The weighted edit distance that clusters also matches a
//! read against stored names: one confusable character costs a quarter.

/// Character groups the readers confuse, at a reduced substitution cost.
/// '_' sits with the tall thin glyphs (an underscore read as 'I' is common).
const CONFUSABLE: [&str; 18] = [
    "0OD", "0Q", "1IL_", "1T", "5S", "8B", "6G", "2Z", "9G", "9P", "CG", "EF", "MN", "UV", "VY",
    "KX", "PR", "7T",
];
const CONFUSION_COST: f64 = 0.25;
const NORMAL_COST: f64 = 1.0;

/// Readings at least this alike are one contact.
pub const NAME_SIMILARITY: f64 = 0.62;
/// km; a contact barely moves during a burst...
pub const RANGE_TOLERANCE: f64 = 0.6;
/// ...but the tolerance has to scale with distance.
pub const RANGE_TOLERANCE_FRAC: f64 = 0.15;
/// The shortest truncation trusted as the same contact.
pub const MIN_PREFIX: usize = 4;

/// Python's `str.upper()` of one character, when it is one character.
fn upper_char(c: char) -> Option<char> {
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(a), None) => Some(a),
        _ => None,
    }
}

fn upper(s: &str) -> Vec<char> {
    s.to_uppercase().chars().collect()
}

fn confusable(a: char, b: char) -> bool {
    CONFUSABLE.iter().any(|g| g.contains(a) && g.contains(b))
}

/// What substituting `b` for `a` costs.
pub fn sub_cost(a: char, b: char) -> f64 {
    if a == b {
        return 0.0;
    }
    match (upper_char(a), upper_char(b)) {
        (Some(ua), Some(ub)) if ua != ub && confusable(ua, ub) => CONFUSION_COST,
        _ => NORMAL_COST,
    }
}

fn distance_chars(a: &[char], b: &[char]) -> f64 {
    if a.is_empty() {
        return b.len() as f64;
    }
    if b.is_empty() {
        return a.len() as f64;
    }
    let mut prev: Vec<f64> = (0..=b.len()).map(|j| j as f64).collect();
    let mut cur = Vec::with_capacity(b.len() + 1);
    for (i, &ca) in a.iter().enumerate() {
        cur.clear();
        cur.push((i + 1) as f64);
        for (j, &cb) in b.iter().enumerate() {
            let v = (prev[j + 1] + 1.0)
                .min(cur[j] + 1.0)
                .min(prev[j] + sub_cost(ca, cb));
            cur.push(v);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Weighted Levenshtein, case-blind; a confusable substitution costs a quarter.
pub fn distance(a: &str, b: &str) -> f64 {
    distance_chars(&upper(a), &upper(b))
}

/// 0..1, where 1 is identical.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (la, lb) = (a.chars().count(), b.chars().count());
    if la == 0 && lb == 0 {
        return 1.0;
    }
    1.0 - distance(a, b) / la.max(lb).max(1) as f64
}

/// How far apart two ranges can be and still be one contact.
pub fn range_tolerance(km: f64) -> f64 {
    RANGE_TOLERANCE.max(km.abs() * RANGE_TOLERANCE_FRAC)
}

/// Two readings of one label: alike, or one a prefix of the other (readers
/// give up at the end of a line rather than mangle the middle).
pub fn same_contact(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_uppercase(), b.to_uppercase());
    if similarity(&a, &b) >= NAME_SIMILARITY {
        return true;
    }
    let (la, lb) = (a.chars().count(), b.chars().count());
    let (short, long, ls) = if la <= lb { (&a, &b, la) } else { (&b, &a, lb) };
    ls >= MIN_PREFIX && long.starts_with(short.as_str())
}

/// One frame's read of one contact.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub name: String,
    pub range_km: f64,
    pub cx: f64,
    pub cy: f64,
}

/// A contact, voted.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub name: String,
    pub range_km: f64,
    pub votes: usize,
    pub total: usize,
    /// The other names read, most common first.
    pub variants: Vec<(String, usize)>,
}

impl Resolved {
    pub fn confidence(&self) -> f64 {
        self.votes as f64 / self.total.max(1) as f64
    }
}

/// Names by count, most common first; ties in the order first seen
/// (`Counter.most_common`).
fn most_common<'a>(names: impl Iterator<Item = &'a str>) -> Vec<(&'a str, usize)> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for n in names {
        match counts.iter_mut().find(|c| c.0 == n) {
            Some(c) => c.1 += 1,
            None => counts.push((n, 1)),
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    counts
}

/// The upper median.
fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

/// Greedy agglomeration against each cluster's current best guess, so one
/// bad early read cannot anchor a group. Range only guards against two
/// different ships that read alike - an exact name match overrides it.
pub fn cluster(readings: &[Reading]) -> Vec<Vec<Reading>> {
    let mut clusters: Vec<Vec<Reading>> = Vec::new();
    for r in readings.iter().filter(|r| !r.name.is_empty()) {
        let mut placed = false;
        for c in clusters.iter_mut() {
            let rep = most_common(c.iter().map(|x| x.name.as_str()))[0]
                .0
                .to_string();
            let rep_range = median(c.iter().map(|x| x.range_km).collect());
            if !same_contact(&r.name, &rep) {
                continue;
            }
            let exact = r.name.to_uppercase() == rep.to_uppercase();
            let tol = range_tolerance(if r.range_km >= rep_range {
                r.range_km
            } else {
                rep_range
            });
            if exact || (r.range_km - rep_range).abs() <= tol {
                c.push(r.clone());
                placed = true;
                break;
            }
        }
        if !placed {
            clusters.push(vec![r.clone()]);
        }
    }
    clusters
}

/// The modal name; ties to the longer one (the first of equal length).
pub fn vote(group: &[Reading]) -> Resolved {
    let top = most_common(group.iter().map(|r| r.name.as_str()));
    let best_n = top[0].1;
    let mut winner = top[0].0;
    for &(n, c) in &top {
        if c == best_n && n.chars().count() > winner.chars().count() {
            winner = n;
        }
    }
    Resolved {
        name: winner.to_string(),
        range_km: median(group.iter().map(|r| r.range_km).collect()),
        votes: top.iter().find(|c| c.0 == winner).unwrap().1,
        total: group.len(),
        variants: top
            .iter()
            .filter(|c| c.0 != winner)
            .map(|&(n, c)| (n.to_string(), c))
            .collect(),
    }
}

/// Cluster, then vote; most votes first, then nearest. Contacts read once
/// stay when their cluster had `min_votes` readings.
pub fn resolve(readings: &[Reading], min_votes: usize) -> Vec<Resolved> {
    let mut out: Vec<Resolved> = cluster(readings).iter().map(|g| vote(g)).collect();
    out.sort_by(|a, b| {
        b.votes.cmp(&a.votes).then(
            a.range_km
                .partial_cmp(&b.range_km)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    out.retain(|r| r.votes >= min_votes || r.total >= min_votes);
    out
}
