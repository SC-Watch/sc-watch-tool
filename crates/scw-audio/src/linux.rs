//! Speech and tones on Linux: speech-dispatcher speaks (through `spd-say`,
//! waiting for each line to finish), and the tone files play through
//! PipeWire (`pw-play`), or PulseAudio's `paplay` or ALSA's `aplay` where
//! PipeWire's player is missing.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::alerts::{TonePlayer, Voice, VoiceMaker};
use crate::config::AudioConfig;

/// Is this program there to run?
fn present(prog: &str) -> bool {
    Command::new(prog)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// speech-dispatcher's rate (-100..100, 0 its normal pace) for words a
/// minute, where 175 is the normal pace.
pub fn spd_rate(wpm: f64) -> i32 {
    (((wpm - 175.0) / 175.0) * 100.0)
        .round()
        .clamp(-100.0, 100.0) as i32
}

/// speech-dispatcher's volume (-100..100) for 0..1.
pub fn spd_volume(v: f64) -> i32 {
    (v.clamp(0.0, 1.0) * 200.0 - 100.0).round() as i32
}

/// The arguments one line is spoken with.
pub fn spd_args(rate: i32, volume: i32, text: &str) -> Vec<String> {
    vec![
        "-w".into(),
        "-r".into(),
        rate.to_string(),
        "-i".into(),
        volume.to_string(),
        "--".into(),
        text.into(),
    ]
}

/// speech-dispatcher, at audio.json's rate and volume. (Its voices are
/// named by the speech module, not as Windows names them, so audio.json's
/// `voice` is left to speech-dispatcher's own setting.)
pub struct SpdVoice {
    rate: i32,
    volume: i32,
}

impl SpdVoice {
    pub fn new(cfg: &AudioConfig) -> Result<Self, String> {
        if !present("spd-say") {
            return Err("no spd-say: install speech-dispatcher to hear alerts spoken".into());
        }
        Ok(SpdVoice {
            rate: spd_rate(cfg.rate as f64),
            volume: spd_volume(cfg.volume),
        })
    }

    /// A maker for [`crate::alerts::Alerts`].
    pub fn maker() -> VoiceMaker {
        Box::new(|cfg| SpdVoice::new(cfg).map(|v| Box::new(v) as Box<dyn Voice>))
    }
}

impl Voice for SpdVoice {
    fn speak(&mut self, text: &str) -> Result<(), String> {
        let status = Command::new("spd-say")
            .args(spd_args(self.rate, self.volume, text))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("spd-say: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("spd-say ended with {status}"))
        }
    }
}

/// Plays a tone file with the first player this system has.
pub struct CmdTones {
    prog: &'static str,
}

impl CmdTones {
    /// pw-play, then paplay, then aplay; None when there is none.
    pub fn find() -> Option<Self> {
        ["pw-play", "paplay", "aplay"]
            .into_iter()
            .find(|p| present(p))
            .map(|prog| CmdTones { prog })
    }

    pub fn program(&self) -> &str {
        self.prog
    }
}

impl TonePlayer for CmdTones {
    fn play(&mut self, wav: &Path) -> Result<(), String> {
        let mut cmd = Command::new(self.prog);
        if self.prog == "aplay" {
            cmd.arg("-q");
        }
        let status = cmd
            .arg(wav)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("{}: {e}", self.prog))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{} ended with {status}", self.prog))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_volumes_and_arguments() {
        assert_eq!(spd_rate(175.0), 0);
        assert_eq!(spd_rate(350.0), 100);
        assert_eq!(spd_rate(1000.0), 100);
        assert_eq!(spd_rate(87.5), -50);
        assert_eq!(spd_volume(0.5), 0);
        assert_eq!(spd_volume(1.0), 100);
        assert_eq!(spd_volume(0.0), -100);
        assert_eq!(spd_volume(0.9), 80);
        // a line that starts like an option is still the line
        assert_eq!(
            spd_args(0, 80, "-rf contact"),
            vec!["-w", "-r", "0", "-i", "80", "--", "-rf contact"]
        );
    }
}
