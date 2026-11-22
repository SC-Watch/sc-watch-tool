//! The watcher - watch.py: what triggers a read, the burst of frames it
//! reads, what is announced and when, what is recorded, and who is looked up.
//!
//! Built from the pieces the other crates hold - frames (scw-capture), keys
//! (scw-input), names (scw-vision), the databases and settings (scw-store),
//! lookups (scw-rsi), alerts (scw-audio) - and given each of them rather
//! than reaching for them, so a test can drive a whole session on a script.

pub mod audit;
pub mod config;
pub mod debuglog;
pub mod enricher;
pub mod session;
pub mod sightings;
pub mod sinks;
pub mod tracker;

pub use config::{ConfigError, parse_bindings, parse_chat_region, parse_focus_size, waiting_label};
pub use debuglog::{DebugLog, Scrub};
pub use session::{Mode, Parts, Session, SessionOptions, Stop};
pub use sightings::SightingsLog;
pub use tracker::{Announcer, Track, Tracker};
