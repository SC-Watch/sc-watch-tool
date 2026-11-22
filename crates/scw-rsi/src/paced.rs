//! One service's request pacing and outage breaker - `rsi._Paced`. Each
//! service has its own, so SENTRY timing out never pauses the fallback.

use crate::JITTER;
use crate::clock::Clock;
use crate::error::{Failure, LookupError};

/// After this many transient failures in a row the service is treated as
/// down for DOWN_FIRST seconds, doubling while it stays down, to DOWN_MAX.
pub const DOWN_AFTER: u32 = 3;
pub const DOWN_FIRST: f64 = 60.0;
pub const DOWN_MAX: f64 = 300.0;

#[derive(Clone, Debug)]
pub(crate) struct Pace {
    pub min_interval: f64,
    /// When the last request was booked to go (epoch seconds).
    pub last: f64,
    /// Transient failures in a row.
    pub trouble: u32,
    /// Monotonic; 0 = not paused.
    pub down_until: f64,
    pub pause: f64,
    /// Paused before and not yet answered since.
    pub probation: bool,
    /// Until when the service itself said to wait - which even a forced
    /// request respects.
    pub told_until: f64,
}

/// `seconds` plus SENTRY's "little random jitter".
pub(crate) fn jitter(clock: &dyn Clock, seconds: f64) -> f64 {
    seconds + clock.uniform(JITTER.0, JITTER.1)
}

impl Pace {
    pub fn new(min_interval: f64) -> Self {
        Pace {
            min_interval,
            last: 0.0,
            trouble: 0,
            down_until: 0.0,
            pause: DOWN_FIRST,
            probation: false,
            told_until: 0.0,
        }
    }

    pub fn down_for(&self, mono: f64) -> f64 {
        (self.down_until - mono).max(0.0)
    }

    pub fn check_paused(
        &self,
        force: bool,
        mono: f64,
        service: &'static str,
    ) -> Result<(), LookupError> {
        let until = if force {
            self.told_until
        } else {
            self.down_until
        };
        if mono < until {
            return Err(LookupError::new(Failure::ServiceDown { until }).tagged(service));
        }
        Ok(())
    }

    pub fn went_ok(&mut self) {
        self.trouble = 0;
        self.down_until = 0.0;
        self.pause = DOWN_FIRST;
        self.probation = false;
    }

    /// Counts a transient failure; pauses when there have been enough, or at
    /// once when the service said how long to wait (`retry_after`).
    pub fn went_wrong(&mut self, retry_after: Option<f64>, clock: &dyn Clock) {
        self.trouble += 1;
        let mono = clock.monotonic();
        if let Some(r) = retry_after.filter(|r| *r != 0.0) {
            self.down_until = self.down_until.max(mono + jitter(clock, r));
            self.told_until = self.told_until.max(self.down_until);
            self.trouble = 0;
            self.probation = true;
            return;
        }
        if self.trouble >= DOWN_AFTER || self.probation {
            self.down_until = mono + self.pause * clock.uniform(1.0, 1.1);
            self.pause = (self.pause * 2.0).min(DOWN_MAX);
            self.trouble = 0;
            self.probation = true;
        }
    }

    /// Reserves the next slot, `min_interval` after the previous one (or
    /// `gap` when that is given); returns when it is (epoch seconds). The
    /// caller sleeps outside the lock.
    pub fn book(&mut self, now: f64, gap: f64) -> f64 {
        let slot = now.max(self.last + gap);
        self.last = slot;
        slot
    }
}
