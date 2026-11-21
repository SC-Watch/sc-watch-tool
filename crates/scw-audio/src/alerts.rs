//! The alert player (audio.Alerts): events in, tones and speech out, on a
//! worker thread so the read loop never waits.
//!
//! Speech is slow ("Warning, <name>, two point four kilometres, piracy" is
//! four seconds), so it is rationed: flagged contacts first, at most
//! `max_spoken_per_burst` of them, a summary instead of a list when there are
//! too many, routine contacts as a tone. An alert still queued after 20 s is
//! about someone who has moved on, and is dropped. Phrases are read again
//! when audio.json changes, so an edit on the settings page is heard on the
//! next alert.
//!
//! The voice and the tone player are traits: Windows' are in
//! [`crate::windows`]; tests use recording ones.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crate::config::AudioConfig;
use crate::phrases::{Facts, live_flags, pick_reason, render_line};
use crate::tones::{EVENTS, tone_wav};

/// An alert still waiting this long is history, and is dropped.
pub const MAX_ALERT_AGE: Duration = Duration::from_secs(20);
/// The queue's length: past it, alerts are dropped, not queued.
pub const QUEUE: usize = 32;

/// Speaks a line and returns when it has been said.
pub trait Voice {
    fn speak(&mut self, text: &str) -> Result<(), String>;
}

/// Plays a WAV file to the end.
pub trait TonePlayer {
    fn play(&mut self, wav: &Path) -> Result<(), String>;
}

/// Makes the voice - on the worker thread, which a COM voice belongs to.
pub type VoiceMaker = Box<dyn Fn(&AudioConfig) -> Result<Box<dyn Voice>, String> + Send>;

/// Health, kept and reported: speech failing silently was the whole problem
/// once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    pub tones: bool,
    pub speech: bool,
    pub failures: u64,
    pub repairs: u64,
    /// Alerts dropped for waiting too long.
    pub expired: u64,
    pub last_error: String,
}

enum Job {
    Event {
        name: String,
        facts: Facts,
        tone_only: bool,
        queued: Instant,
    },
    Text(String),
    Close,
}

struct Shared {
    cfg: AudioConfig,
    config_path: PathBuf,
    mtime: Option<SystemTime>,
    health: Health,
}

impl Shared {
    /// audio.json changed: take its phrases (a half-written file keeps
    /// what was there).
    fn refresh_phrases(&mut self) {
        let m = mtime(&self.config_path);
        if m != self.mtime {
            self.mtime = m;
            let fresh = AudioConfig::load(&self.config_path);
            self.cfg.speech = fresh.speech;
            self.cfg.muted = fresh.muted;
        }
    }
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Writes the tone WAVs into `dir` (make_tones).
pub fn make_tones(cfg: &AudioConfig, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::new();
    for event in cfg.tones.keys() {
        let p = dir.join(format!("{event}.wav"));
        std::fs::write(
            &p,
            tone_wav(&cfg.tone_steps(event), cfg.volume, crate::tones::RATE),
        )?;
        out.push(p);
    }
    Ok(out)
}

/// Non-blocking audio. Never call this from the read loop and wait.
pub struct Alerts {
    tx: SyncSender<Job>,
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
}

impl Alerts {
    /// `config_path` is audio.json (re-read when it changes), `tone_dir`
    /// the folder of event WAVs (made when empty). `player` is None where
    /// tones cannot play; `voice` None where there is no voice.
    pub fn new(
        cfg: AudioConfig,
        config_path: &Path,
        tone_dir: &Path,
        player: Option<Box<dyn TonePlayer + Send>>,
        voice: Option<VoiceMaker>,
    ) -> Self {
        let mut health = Health::default();
        if cfg.tones_on() && player.is_some() {
            let empty = std::fs::read_dir(tone_dir).map_or(true, |mut d| {
                !d.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "wav")))
            });
            let made = !empty || make_tones(&cfg, tone_dir).is_ok();
            health.tones = made;
        }
        health.speech = cfg.speech_on() && voice.is_some();
        let shared = Arc::new(Mutex::new(Shared {
            mtime: mtime(config_path),
            config_path: config_path.to_path_buf(),
            cfg,
            health,
        }));
        let (tx, rx) = sync_channel(QUEUE);
        let worker = Worker {
            shared: shared.clone(),
            tone_dir: tone_dir.to_path_buf(),
            player,
            maker: voice,
        };
        let thread = std::thread::spawn(move || worker.run(rx));
        Self {
            tx,
            shared,
            thread: Some(thread),
        }
    }

    pub fn health(&self) -> Health {
        self.shared.lock().unwrap().health.clone()
    }

    fn queue(&self, job: Job) {
        // Dropping is correct: stale alerts are worse than missing ones, and
        // a backed-up queue would talk over the next encounter.
        if let Err(TrySendError::Full(_)) = self.tx.try_send(job) {}
    }

    /// Raises an event; returns at once, the audio happens on the worker.
    pub fn event(&self, name: &str, facts: Facts, tone_only: bool) {
        {
            let mut sh = self.shared.lock().unwrap();
            if sh.cfg.off() {
                return;
            }
            sh.refresh_phrases();
            // A lookup warning whose every flag is switched off is no
            // warning: nothing is played, tone included.
            if name == "flagged"
                && !facts.flags.is_empty()
                && live_flags(&facts.flags, &sh.cfg.muted).is_empty()
            {
                return;
            }
        }
        self.queue(Job::Event {
            name: name.to_string(),
            facts,
            tone_only,
            queued: Instant::now(),
        });
    }

    /// Does this contact still warrant a warning, with the phrases that are
    /// off taken away? Structured reasons decide when present; otherwise
    /// the older reason line does.
    fn alerting(cfg: &AudioConfig, c: &Facts) -> bool {
        if !c.flags.is_empty() && !live_flags(&c.flags, &cfg.muted).is_empty() {
            return true;
        }
        if !c.said.is_empty() {
            return pick_reason(&c.said, &cfg.muted).is_some();
        }
        !c.reason.is_empty()
    }

    /// Announces a whole burst, rationing speech.
    pub fn burst_result(&self, contacts: &[Facts]) {
        let cfg = {
            let mut sh = self.shared.lock().unwrap();
            if sh.cfg.off() {
                return;
            }
            if contacts.is_empty() {
                drop(sh);
                self.event("nothing", Facts::default(), false);
                return;
            }
            sh.refresh_phrases();
            sh.cfg.clone()
        };
        let flagged: Vec<&Facts> = contacts
            .iter()
            .filter(|c| Self::alerting(&cfg, c))
            .collect();
        let summary = |n: usize| Facts {
            count: Some(contacts.len()),
            flagged: Some(n),
            ..Default::default()
        };
        // Too many to read out: say how many, not who.
        if contacts.len() as i64 > cfg.summarise_above && flagged.is_empty() {
            self.event("contact", Facts::default(), true);
            self.event("summary", summary(0), false);
            return;
        }
        let cap = cfg.max_spoken_per_burst.max(0) as usize;
        for c in flagged.iter().take(cap) {
            let name = if c.reason.is_empty() {
                "flagged"
            } else {
                "known"
            };
            self.event(name, (*c).clone(), false);
        }
        if flagged.len() > cap {
            self.event("summary", summary(flagged.len()), false);
        }
        for c in contacts {
            if !flagged.contains(&c) {
                self.event("contact", c.clone(), true);
            }
        }
    }

    /// Speaks a line as it is - the settings page's "hear it". (With audio
    /// off, nothing - as every event.)
    pub fn say(&self, text: &str) {
        if self.shared.lock().unwrap().cfg.off() {
            return;
        }
        self.queue(Job::Text(text.to_string()));
    }

    /// Stops the worker once what is queued has been played.
    pub fn close(mut self) {
        let _ = self.tx.send(Job::Close);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Alerts {
    fn drop(&mut self) {
        let _ = self.tx.try_send(Job::Close);
    }
}

struct Worker {
    shared: Arc<Mutex<Shared>>,
    tone_dir: PathBuf,
    player: Option<Box<dyn TonePlayer + Send>>,
    maker: Option<VoiceMaker>,
}

/// The voice, made on and never leaving the worker thread.
type Speaker = Option<Box<dyn Voice>>;

impl Worker {
    fn note_failure(&self, what: String) {
        let mut sh = self.shared.lock().unwrap();
        sh.health.failures += 1;
        sh.health.last_error = what;
    }

    fn run(mut self, rx: Receiver<Job>) {
        let mut voice: Speaker = None;
        let speech = self.shared.lock().unwrap().health.speech;
        if speech && let Some(make) = &self.maker {
            let cfg = self.shared.lock().unwrap().cfg.clone();
            match make(&cfg) {
                Ok(v) => voice = Some(v),
                Err(e) => {
                    let mut sh = self.shared.lock().unwrap();
                    sh.health.speech = false;
                    sh.health.last_error = format!("engine init: {e}");
                }
            }
        }
        while let Ok(job) = rx.recv() {
            match job {
                Job::Close => return,
                Job::Text(text) => {
                    if self.shared.lock().unwrap().health.speech && !text.is_empty() {
                        self.speak(&mut voice, &text);
                    }
                }
                Job::Event {
                    name,
                    facts,
                    tone_only,
                    queued,
                } => {
                    if queued.elapsed() > MAX_ALERT_AGE {
                        self.shared.lock().unwrap().health.expired += 1;
                        continue;
                    }
                    self.play(&mut voice, &name, &facts, tone_only);
                }
            }
        }
    }

    /// Says one line; a failure is counted and the voice rebuilt once, so
    /// one bad utterance cannot end speech for the session.
    fn speak(&mut self, voice: &mut Speaker, text: &str) {
        let Some(v) = voice.as_mut() else {
            return;
        };
        if let Err(e) = v.speak(text) {
            self.note_failure(format!("speak: {e}"));
            let cfg = self.shared.lock().unwrap().cfg.clone();
            match self.maker.as_ref().map(|m| m(&cfg)) {
                Some(Ok(v)) => {
                    *voice = Some(v);
                    self.shared.lock().unwrap().health.repairs += 1;
                }
                _ => {
                    *voice = None;
                    self.shared.lock().unwrap().health.speech = false;
                }
            }
        }
    }

    fn play(&mut self, voice: &mut Speaker, name: &str, facts: &Facts, tone_only: bool) {
        let (cfg, health) = {
            let mut sh = self.shared.lock().unwrap();
            sh.refresh_phrases();
            (sh.cfg.clone(), sh.health.clone())
        };
        if health.tones && EVENTS.contains(&name) && cfg.tones_on() {
            let path = self.tone_dir.join(format!("{name}.wav"));
            if path.exists()
                && let Some(p) = self.player.as_mut()
                && let Err(e) = p.play(&path)
            {
                self.note_failure(format!("{name}: {e}"));
            }
        }
        if tone_only || !health.speech || !cfg.speech_on() {
            return;
        }
        if cfg.muted.iter().any(|m| m == name) {
            return; // the line is off: its tone, no words
        }
        let template = cfg.speech_for(name);
        if template.is_empty() {
            return;
        }
        let text = render_line(template, facts, &cfg.speech, cfg.speak_handles, &cfg.muted);
        if !text.is_empty() {
            self.speak(voice, &text);
        }
    }
}
