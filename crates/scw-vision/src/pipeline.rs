//! From frames to contacts with the IRIS reader - what sc-watch's watcher
//! does per frame and per burst with `--reader iris` (iris_read.IrisReader
//! with no OCR fallback, then watch.Session._tally and consensus.resolve).
//!
//! Pure: frames in, typed results out. Lists the user keeps (which make a
//! world label count as a player) come in as a function, not a database.

use std::collections::HashMap;
use std::sync::Arc;

use crate::iris::ops::boundary_from_percents;
use crate::iris::{DetectorSettings, ImageOps, ReadClass, RowReader, ScanGeometry};
use crate::names::vote::{Reading, Resolved, resolve};
use crate::names::{NameKind, classify_name};
use crate::{Image, Rect};

/// How much of the screen the scan looks at: (top, bottom, left, right)
/// margins in percent, IRIS's TrackingBoundary order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanArea {
    Whole,
    Narrow,
    /// IRIS's own default.
    Iris,
    Centre,
}

impl ScanArea {
    pub const ALL: [ScanArea; 4] = [
        ScanArea::Whole,
        ScanArea::Narrow,
        ScanArea::Iris,
        ScanArea::Centre,
    ];

    pub fn margins(self) -> (i32, i32, i32, i32) {
        match self {
            ScanArea::Whole => (0, 0, 0, 0),
            ScanArea::Narrow => (5, 5, 3, 3),
            ScanArea::Iris => (15, 15, 8, 8),
            ScanArea::Centre => (25, 25, 20, 20),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ScanArea::Whole => "whole",
            ScanArea::Narrow => "narrow",
            ScanArea::Iris => "iris",
            ScanArea::Centre => "centre",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == s)
    }
}

/// One plate read off one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Contact {
    /// The cleaned name; None when the read was unusable.
    pub name: Option<String>,
    pub kind: NameKind,
    /// The distance line's range, when it read as one.
    pub range_km: Option<f64>,
    /// Around all the plate's name rows.
    pub name_box: Rect,
    /// The distance line.
    pub range_box: Rect,
}

/// What one frame gave.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameRead {
    /// Distance lines the scan kept.
    pub lines: usize,
    /// Plates IRIS read as a name, nearest first.
    pub contacts: Vec<Contact>,
    /// Plates it would not read, by its reason, in the order first met.
    pub refused: Vec<(&'static str, usize)>,
}

/// Reads frames with IRIS's finder and reader; one set of plate operations
/// per frame size, all sharing one letter model.
pub struct FrameReader {
    rows: Arc<RowReader>,
    area: ScanArea,
    settings: DetectorSettings,
    ops: HashMap<(usize, usize), ImageOps>,
}

impl FrameReader {
    pub fn new(
        rows: impl Into<Arc<RowReader>>,
        area: ScanArea,
        settings: DetectorSettings,
    ) -> Self {
        Self {
            rows: rows.into(),
            area,
            settings,
            ops: HashMap::new(),
        }
    }

    pub fn area(&self) -> ScanArea {
        self.area
    }

    /// The plate operations for a frame size; None when the scan area
    /// leaves nothing of it.
    pub fn ops_for(&mut self, width: usize, height: usize) -> Option<&ImageOps> {
        if !self.ops.contains_key(&(width, height)) {
            let (t, b, l, r) = self.area.margins();
            let geo = ScanGeometry::create(
                width as i32,
                height as i32,
                &self.settings,
                Some(boundary_from_percents(t, b, l, r)),
            )?;
            self.ops
                .insert((width, height), ImageOps::new(self.rows.clone(), geo));
        }
        self.ops.get(&(width, height))
    }

    /// Every distance line in the frame, read (iris_read.IrisReader.detect
    /// without the OCR fallback).
    pub fn read(&mut self, frame: &Image<u8>) -> FrameRead {
        let rows = self.rows.clone();
        let Some(ops) = self.ops_for(frame.width, frame.height) else {
            return FrameRead::default();
        };
        let mut out = FrameRead::default();
        for line in ops.scan(frame) {
            out.lines += 1;
            let km = rows.read_distance(frame, line).map(|d| d.km());
            let r = ops.read_line(frame, line);
            match (&r.class, &r.name, &r.rows) {
                (ReadClass::Name, Some(name), Some(plate)) if !name.is_empty() => {
                    let (kind, cleaned) = classify_name(name);
                    let x0 = plate.iter().map(|b| b.x).min().unwrap_or(line.x);
                    let y0 = plate.iter().map(|b| b.y).min().unwrap_or(line.y);
                    let x1 = plate.iter().map(|b| b.x + b.w).max().unwrap_or(line.x);
                    let y1 = plate.iter().map(|b| b.y + b.h).max().unwrap_or(line.y);
                    out.contacts.push(Contact {
                        name: cleaned,
                        kind,
                        range_km: km,
                        name_box: Rect::new(x0, y0, x1 - x0, y1 - y0),
                        range_box: line,
                    });
                }
                _ => match out.refused.iter_mut().find(|e| e.0 == r.reason) {
                    Some(e) => e.1 += 1,
                    None => out.refused.push((r.reason, 1)),
                },
            }
        }
        // Nearest first; a plate with no range last (stable, as sorted()).
        out.contacts.sort_by(|a, b| {
            let k = |c: &Contact| c.range_km.unwrap_or(1e9);
            k(a).partial_cmp(&k(b)).unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }
}

/// A burst's reads, gathered frame by frame (watch.Session._tally).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Burst {
    /// The players' readings, in the order read.
    pub accumulated: Vec<Reading>,
    /// Plates read as nothing usable.
    pub unreadable: usize,
    /// Contacts that are not people, by kind, in the order first met.
    pub discarded: Vec<(NameKind, usize)>,
    /// Players read in each frame.
    pub per_frame: Vec<usize>,
}

impl Burst {
    /// Sorts one frame's contacts into the burst; returns the players read.
    /// A world label counts as a player when `listed` says the handle is on
    /// one of the user's lists. Plates with no range are left out.
    pub fn tally(&mut self, contacts: &[Contact], listed: &dyn Fn(&str) -> bool) -> usize {
        self.tally_with(contacts, listed, |_| {})
    }

    /// [`Burst::tally`], telling `player` about each contact taken as a
    /// player (the watcher keeps the boxes behind each reading).
    pub fn tally_with(
        &mut self,
        contacts: &[Contact],
        listed: &dyn Fn(&str) -> bool,
        mut player: impl FnMut(&Contact),
    ) -> usize {
        let mut named = 0;
        for c in contacts {
            let Some(km) = c.range_km else {
                continue;
            };
            let mut kind = c.kind;
            if kind == NameKind::WorldLabel && c.name.as_deref().is_some_and(listed) {
                kind = NameKind::Player;
            }
            match kind {
                NameKind::Player => {
                    self.accumulated.push(Reading {
                        name: c.name.clone().unwrap_or_default(),
                        range_km: km,
                        cx: c.name_box.x as f64 + c.name_box.w as f64 / 2.0,
                        cy: c.name_box.y as f64,
                    });
                    player(c);
                    named += 1;
                }
                NameKind::Unreadable => self.unreadable += 1,
                other => match self.discarded.iter_mut().find(|e| e.0 == other) {
                    Some(e) => e.1 += 1,
                    None => self.discarded.push((other, 1)),
                },
            }
        }
        self.per_frame.push(named);
        named
    }

    /// The burst's contacts, voted.
    pub fn resolve(&self, min_votes: usize) -> Vec<Resolved> {
        resolve(&self.accumulated, min_votes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(name: &str, km: Option<f64>) -> Contact {
        let (kind, cleaned) = classify_name(name);
        Contact {
            name: cleaned,
            kind,
            range_km: km,
            name_box: Rect::new(10, 20, 30, 8),
            range_box: Rect::new(12, 30, 26, 8),
        }
    }

    #[test]
    fn tally_sorts_reads_as_the_watcher_does() {
        let mut b = Burst::default();
        let frame = [
            contact("EXMPL0NAME", Some(1.5)),
            contact("EXMPL0NAME", None),
            contact("GOLD", Some(2.0)),
            contact("DIAMOND", Some(2.5)),
            contact("XY-1234-AB", Some(3.0)),
            contact("UNKNOWN", Some(4.0)),
            contact("!!", Some(5.0)),
        ];
        let named = b.tally(&frame, &|n| n == "GOLD");
        assert_eq!(named, 2, "the player and the listed world label");
        assert_eq!(b.accumulated[0].name, "EXMPL0NAME");
        assert_eq!((b.accumulated[0].cx, b.accumulated[0].cy), (25.0, 20.0));
        assert_eq!(b.accumulated[1].name, "GOLD");
        assert_eq!(b.unreadable, 1);
        assert_eq!(
            b.discarded,
            vec![
                (NameKind::WorldLabel, 1),
                (NameKind::ShipSerial, 1),
                (NameKind::UnknownContact, 1)
            ]
        );
        b.tally(&frame[..1], &|_| false);
        let r = b.resolve(1);
        assert_eq!(
            (r[0].name.as_str(), r[0].votes, r[0].total),
            ("EXMPL0NAME", 2, 2)
        );
        assert_eq!(b.per_frame, vec![2, 1]);
    }

    #[test]
    fn scan_areas_round_trip() {
        for a in ScanArea::ALL {
            assert_eq!(ScanArea::parse(a.name()), Some(a));
        }
        assert_eq!(ScanArea::parse("everything"), None);
    }
}
