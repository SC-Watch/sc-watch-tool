//! Looking handles up without holding up the read loop - watch.RsiEnricher.
//!
//! The name is announced at once; the profile arrives when it arrives, on
//! its own line. A lookup that fails for a reason that may pass is tried
//! again (RETRY_DELAYS); one met by a paused service waits the pause out
//! without using up its tries; an expired copy that stood in, or a profile
//! waiting on SENTRY to read an org, or a name SENTRY is still checking,
//! is asked again on its own ladder. "No such account" raises a misread
//! audit, with the pictures the read handed over. A flag is alerted once.
//!
//! [`Enricher`] is the state and the work, driven a step at a time:
//! [`Enricher::next_due`] and [`Enricher::process`]. [`EnricherThread`] runs
//! that loop on its own thread for the watcher; a test drives it directly.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use chrono::NaiveDateTime;
use serde_json::{Value, json};

use scw_rsi::{Clock, Failure, LookupError};
use scw_store::reputation::{Database, ProfileOrg, manual_line};

use crate::audit::{AuditPaths, Crops, save_audit_images};
use crate::sinks::{AlertSink, Console, EventLog, Lookups};

/// Handles whose crops are held while their lookups are in flight.
pub const PENDING_MAX: usize = 64;
/// Before the second and third tries of a lookup that failed for a reason
/// that might pass.
pub const RETRY_DELAYS: [f64; 2] = [15.0, 60.0];
/// For a profile missing an org's focus while SENTRY reads that org.
pub const SENTRY_WAIT_DELAYS: [f64; 2] = [90.0, 240.0];
/// For a name SENTRY is checking RSI for - never sooner than it says.
pub const SENTRY_CHECK_DELAYS: [f64; 3] = [30.0, 90.0, 240.0];

const PAD: &str = "          ";

/// What a read hands over with a handle, kept until its lookup returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending {
    pub crops: Crops,
    pub range_km: Option<f64>,
    pub votes: usize,
    pub total: usize,
    pub variants: Vec<(String, usize)>,
}

/// What processing a handle needs, lent for the call.
pub struct Deps<'a> {
    pub lookups: &'a dyn Lookups,
    pub console: &'a dyn Console,
    pub log: &'a dyn EventLog,
    pub alerts: Option<&'a dyn AlertSink>,
    pub db: Option<&'a Database>,
    pub data_root: &'a Path,
    pub clock: &'a dyn Clock,
    /// The local time, for the audit pictures' names.
    pub local_now: &'a dyn Fn() -> NaiveDateTime,
    /// Read enlistment dates as UTC (tests).
    pub utc_dates: bool,
}

#[derive(Debug, Default)]
pub struct Enricher {
    /// Raise misread audits (`--no-audit` turns both kinds off).
    pub audit: bool,
    pub seen: HashSet<String>,
    pub queue: VecDeque<String>,
    /// (due, sequence, handle), soonest first.
    pub retry: Vec<(f64, u64, String)>,
    seq: u64,
    pub attempts: HashMap<String, usize>,
    pub announced_pause: f64,
    pub stale_shown: HashSet<String>,
    pub check_shown: HashSet<String>,
    /// The delay _keep_refreshing used; None when it gave up.
    pub last_delay: Option<f64>,
    pub alerted: HashMap<String, Vec<String>>,
    /// Oldest first.
    pub pending: Vec<(String, Pending)>,
}

impl Enricher {
    pub fn new(audit: bool) -> Self {
        Enricher {
            audit,
            ..Default::default()
        }
    }

    /// Queues a handle once; `crops` and the read's numbers travel with it,
    /// for the audit a "no such account" would raise. False if it was
    /// already seen.
    pub fn submit(&mut self, handle: &str, ev: Option<Pending>) -> bool {
        if !self.seen.insert(handle.to_string()) {
            return false;
        }
        if let Some(ev) = ev.filter(|e| !e.crops.is_empty()) {
            if self.pending.len() >= PENDING_MAX {
                self.pending.remove(0); // the oldest
            }
            self.pending.push((handle.to_string(), ev));
        }
        self.queue.push_back(handle.to_string());
        true
    }

    fn take_pending(&mut self, handle: &str) -> Option<Pending> {
        let i = self.pending.iter().position(|(h, _)| h == handle)?;
        Some(self.pending.remove(i).1)
    }

    fn peek_pending(&self, handle: &str) -> Option<Pending> {
        self.pending
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, p)| p.clone())
    }

    fn push_retry(&mut self, due: f64, handle: &str) {
        self.seq += 1;
        let item = (due, self.seq, handle.to_string());
        let at = self
            .retry
            .partition_point(|r| (r.0, r.1) < (item.0, item.1));
        self.retry.insert(at, item);
    }

    /// When the next retry is due (monotonic), if any.
    pub fn next_retry_at(&self) -> Option<f64> {
        self.retry.first().map(|r| r.0)
    }

    /// The next handle to look up at `now` (monotonic): a retry that has
    /// come due, else the next submission.
    pub fn next_due(&mut self, now: f64) -> Option<String> {
        if self.retry.first().is_some_and(|r| r.0 <= now) {
            return Some(self.retry.remove(0).2);
        }
        self.queue.pop_front()
    }

    /// A lookup failed: retry it later if that could help, else give up for
    /// now - it leaves `seen`, so the next sighting asks again.
    fn failed(&mut self, handle: &str, e: &LookupError, d: &Deps) {
        let mono = d.clock.monotonic();
        let why = e.describe(mono);
        if let Failure::ServiceDown { until } | Failure::LiveLimited { until } = e.failure {
            // Nothing was sent: it waits the pause out, its tries unspent.
            self.push_retry(until + 0.5, handle);
            if until != self.announced_pause {
                self.announced_pause = until;
                let waiting = self.retry.len() + self.queue.len();
                let remaining = (until - mono).max(0.0);
                d.console.say(&format!(
                    "{PAD}rsi: {} is not responding - pausing lookups for {remaining:.0}s ({waiting} waiting)",
                    e.service.unwrap_or("the lookup service")
                ));
                d.log.event(
                    "rsi_paused",
                    &[
                        ("seconds", json!(remaining.round_ties_even() as i64)),
                        ("waiting", json!(waiting)),
                    ],
                );
            }
            let n = self.attempts.get(handle).copied().unwrap_or(0);
            d.lookups.note_failure(handle, &why, n, false);
            return;
        }
        let attempt = self.attempts.get(handle).copied().unwrap_or(0) + 1;
        self.attempts.insert(handle.to_string(), attempt);
        let retry = e.is_transient() && attempt <= RETRY_DELAYS.len();
        if retry {
            let delay = RETRY_DELAYS[attempt - 1];
            self.push_retry(mono + delay, handle);
            d.console.say(&format!(
                "{PAD}rsi: {handle} lookup failed - {why}; trying again in {delay:.0}s"
            ));
        } else {
            // No answer, so no audit: the next sighting brings fresh crops.
            self.take_pending(handle);
            self.seen.remove(handle);
            self.attempts.remove(handle);
            d.console.say(&format!(
                "{PAD}rsi: {handle} lookup failed - {why}; will try again next time it is read"
            ));
        }
        d.lookups.note_failure(handle, &why, attempt, !retry);
        d.log.event(
            "rsi_failed",
            &[
                ("handle", json!(handle)),
                ("why", json!(why)),
                ("attempt", json!(attempt)),
                ("retry", json!(retry)),
            ],
        );
    }

    /// An answer that is not the whole answer yet: ask again on the right
    /// ladder. True the first time, so it is reported once.
    fn keep_refreshing(
        &mut self,
        handle: &str,
        reason: &str,
        waiting: bool,
        checking: bool,
        hint: Option<f64>,
        d: &Deps,
    ) -> bool {
        let shown = if checking {
            &mut self.check_shown
        } else {
            &mut self.stale_shown
        };
        let first = shown.insert(handle.to_string());
        let attempt = self.attempts.get(handle).copied().unwrap_or(0) + 1;
        self.attempts.insert(handle.to_string(), attempt);
        let delays: &[f64] = if checking {
            &SENTRY_CHECK_DELAYS
        } else if waiting {
            &SENTRY_WAIT_DELAYS
        } else {
            &RETRY_DELAYS
        };
        let again = attempt <= delays.len();
        self.last_delay = None;
        if again {
            let delay = delays[attempt - 1].max(hint.unwrap_or(0.0));
            let delay = delay.max(d.lookups.down_for() + 0.5);
            self.last_delay = Some(delay);
            let due = d.clock.monotonic() + delay;
            self.push_retry(due, handle);
        } else {
            self.attempts.remove(handle);
            self.take_pending(handle);
            self.seen.remove(handle);
            self.stale_shown.remove(handle);
            self.check_shown.remove(handle);
        }
        d.lookups.note_failure(handle, reason, attempt, !again);
        d.log.event(
            "rsi_stale",
            &[
                ("handle", json!(handle)),
                ("why", json!(reason)),
                ("attempt", json!(attempt)),
                ("retry", json!(again)),
            ],
        );
        first
    }

    /// Looks one handle up, and says, alerts, audits and records what came
    /// back (the body of RsiEnricher._run).
    pub fn process(&mut self, handle: &str, d: &Deps) {
        let answer = d.lookups.citizen(handle);
        self.answered(handle, answer, d);
    }

    /// The second half of [`Enricher::process`]: what a lookup's answer
    /// leads to. Split so a thread can make the lookup without holding the
    /// state.
    pub fn answered(
        &mut self,
        handle: &str,
        answer: Result<scw_rsi::Citizen, LookupError>,
        d: &Deps,
    ) {
        let c = match answer {
            Ok(c) => c,
            Err(e) => return self.failed(handle, &e, d),
        };
        let stale = c.note.stale_reason.clone();
        if let Some(_state) = c.note.checking.as_ref().filter(|s| !s.is_empty()) {
            let reason = stale.clone().unwrap_or_default();
            let first = self.keep_refreshing(handle, &reason, false, true, c.note.retry_in, d);
            match self.last_delay {
                None => d.console.say(&format!(
                    "{PAD}rsi: {handle} - {reason}, not finished yet; will try again next time it is read"
                )),
                Some(delay) if first => d.console.say(&format!(
                    "{PAD}rsi: {handle} - {reason}; asking again in {delay:.0}s"
                )),
                _ => {}
            }
            return;
        }
        self.check_shown.remove(handle);
        let ev = match &stale {
            Some(reason) => {
                if !self.keep_refreshing(handle, reason, c.note.awaiting_sentry, false, None, d) {
                    return; // already reported from this copy
                }
                self.peek_pending(handle)
            }
            None => {
                self.attempts.remove(handle);
                self.stale_shown.remove(handle);
                self.take_pending(handle)
            }
        };
        if !c.exists {
            // Usually an OCR error - and RSI never having heard of the name
            // is the only evidence that catches a confident misread.
            d.console.say(&format!(
                "{PAD}rsi: {handle} - no profile (likely a misread)"
            ));
            let (votes, total) = match &ev {
                Some(e) => (json!(e.votes), json!(e.total)),
                None => (Value::Null, Value::Null),
            };
            d.log.event(
                "rsi",
                &[
                    ("handle", json!(handle)),
                    ("exists", json!(false)),
                    ("votes", votes),
                    ("total", total),
                ],
            );
            if let (Some(db), true) = (d.db, self.audit) {
                d.log.event(
                    "audit",
                    &[
                        ("handle", json!(handle)),
                        ("reason", json!("no_rsi_account")),
                    ],
                );
                let paths = match &ev {
                    Some(e) if !e.crops.is_empty() => {
                        save_audit_images(d.data_root, handle, &e.crops, None, 90, (d.local_now)())
                    }
                    _ => AuditPaths::default(),
                };
                let variants = json!(
                    ev.as_ref()
                        .map(|e| e
                            .variants
                            .iter()
                            .map(|(n, c)| json!([n, c]))
                            .collect::<Vec<_>>())
                        .unwrap_or_default()
                );
                let _ = db.add_audit(
                    handle,
                    ev.as_ref().and_then(|e| e.range_km),
                    ev.as_ref().map_or(0, |e| e.votes as i64),
                    ev.as_ref().map_or(0, |e| e.total as i64),
                    &variants,
                    &paths.frame_path,
                    &paths.crop_name,
                    &paths.crop_range,
                    "no_rsi_account",
                );
            }
            return;
        }
        // Lookups come back out of order, so the handle is on the line.
        let now = d.clock.time();
        let mut line = format!("{PAD}rsi: {handle} - {}", c.summary(now, d.utc_dates));
        if c.note.source == Some(scw_rsi::Source::Sentry) {
            line.push_str("  [via SENTRY]");
        }
        if let Some(reason) = &stale {
            let age = (now - c.note.stored_at.unwrap_or(now)) / 86400.0;
            line.push_str(&format!(
                "  (stored profile from {age:.0} days ago - {reason}; will refresh)"
            ));
        }
        let flags = c.flags(now, d.utc_dates);
        for flag in &flags {
            line.push_str(&format!("\n{PAD}!!   {handle}: {flag}"));
        }
        d.console.say(&line);

        // Flags arrive after the burst's own alert: a second one, for the
        // flags not already alerted.
        let known = self.alerted.entry(handle.to_string()).or_default();
        let new_flags: Vec<String> = flags
            .iter()
            .filter(|f| !known.contains(f))
            .cloned()
            .collect();
        known.extend(new_flags.iter().cloned());
        if let (Some(alerts), false) = (d.alerts, new_flags.is_empty()) {
            alerts.event(
                "flagged",
                Some(handle),
                &new_flags,
                ev.as_ref().and_then(|e| e.range_km),
            );
        }

        // Remember the orgs, so an org flag reaches this player later.
        if let (Some(db), false) = (d.db, c.orgs.is_empty()) {
            let orgs: Vec<ProfileOrg> = c
                .orgs
                .iter()
                .map(|o| ProfileOrg {
                    sid: o.sid.clone(),
                    is_main: o.is_main,
                })
                .collect();
            let _ = (|| -> rusqlite::Result<()> {
                db.set_orgs(handle, &orgs)?;
                db.update_profile(handle, &c.moniker, &c.enlisted)?;
                let a = db.assess(handle, None, false)?;
                for (sid, note) in &a.flagged_orgs {
                    let note = if note.is_empty() {
                        String::new()
                    } else {
                        format!(" - {note}")
                    };
                    d.console.say(&format!(
                        "{PAD}***  {handle}: FLAGGED ORG {sid}{note}{}",
                        manual_line(sid, &a.manual_orgs, &a.manual_by)
                    ));
                }
                Ok(())
            })();
        }
    }
}

/// The enricher on its own thread: the read loop submits, the thread looks
/// up. What it needs for each handle comes from `deps`, which builds them
/// on that thread (its own database connection, as dbopen gives each
/// thread one).
pub struct EnricherThread {
    shared: Arc<(Mutex<(Enricher, bool)>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Everything the thread owns, made on the thread.
pub struct Owned {
    pub lookups: Arc<dyn Lookups>,
    pub console: Arc<dyn Console>,
    pub log: Arc<dyn EventLog>,
    pub alerts: Option<Arc<dyn AlertSink>>,
    pub db: Option<Database>,
    pub data_root: PathBuf,
    pub clock: Arc<dyn Clock>,
}

impl EnricherThread {
    pub fn start(audit: bool, make: Box<dyn FnOnce() -> Owned + Send>) -> Self {
        let shared = Arc::new((Mutex::new((Enricher::new(audit), false)), Condvar::new()));
        let s = shared.clone();
        let thread = std::thread::spawn(move || {
            let own = make();
            let local_now = || chrono::Local::now().naive_local();
            let deps = Deps {
                lookups: &*own.lookups,
                console: &*own.console,
                log: &*own.log,
                alerts: own.alerts.as_deref(),
                db: own.db.as_ref(),
                data_root: &own.data_root,
                clock: &*own.clock,
                local_now: &local_now,
                utc_dates: false,
            };
            let (lock, cv) = &*s;
            loop {
                let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
                let handle = loop {
                    if g.1 {
                        return;
                    }
                    let now = own.clock.monotonic();
                    if let Some(h) = g.0.next_due(now) {
                        break h;
                    }
                    let wait = g.0.next_retry_at().map(|t| (t - now).max(0.0));
                    g = match wait {
                        Some(w) => {
                            cv.wait_timeout(g, std::time::Duration::from_secs_f64(w))
                                .unwrap_or_else(|e| e.into_inner())
                                .0
                        }
                        None => cv.wait(g).unwrap_or_else(|e| e.into_inner()),
                    };
                };
                // The network call runs without the lock, so a read can
                // submit meanwhile; its answer is handled with it.
                drop(g);
                let answer = deps.lookups.citizen(&handle);
                let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
                g.0.answered(&handle, answer, &deps);
            }
        });
        EnricherThread {
            shared,
            thread: Some(thread),
        }
    }

    pub fn submit(&self, handle: &str, ev: Option<Pending>) {
        let (lock, cv) = &*self.shared;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        if g.0.submit(handle, ev) {
            cv.notify_one();
        }
    }

    /// Handles queued and not yet looked up.
    pub fn waiting(&self) -> usize {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
            .queue
            .len()
    }

    pub fn stop(&mut self) {
        {
            let (lock, cv) = &*self.shared;
            lock.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
            cv.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for EnricherThread {
    fn drop(&mut self) {
        self.stop();
    }
}
