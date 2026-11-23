//! sc-watch's audible alerts (audio.py): what is said ([`phrases`], exactly
//! as the Python says it, and the settings page's checks), the tones
//! ([`tones`], the same WAV bytes), audio.json ([`config`]), and the player
//! ([`alerts`]) with its Windows voice and tones.

pub mod alerts;
pub mod config;
pub mod format;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod phrases;
pub mod tones;
#[cfg(windows)]
pub mod windows;

pub use alerts::{Alerts, Health, TonePlayer, Voice};
pub use config::AudioConfig;
pub use phrases::{Facts, Reason, ReasonKind, check_phrase, render_line, sample};
