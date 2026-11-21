//! sc-watch's audible alerts (audio.py): what is said ([`phrases`], exactly
//! as the Python says it, and the settings page's checks), the tones
//! ([`tones`], the same WAV bytes), and audio.json ([`config`]).

pub mod config;
pub mod format;
pub mod phrases;
pub mod tones;

pub use config::AudioConfig;
pub use phrases::{Facts, Reason, ReasonKind, check_phrase, render_line, sample};
