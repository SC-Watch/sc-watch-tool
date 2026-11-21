//! The alert tones: sine steps with a short fade, as plain WAV files you can
//! replace with your own (audio.write_tone and make_tones, byte for byte).

/// Every event the pipeline can raise.
pub const EVENTS: [&str; 6] = ["trigger", "contact", "flagged", "known", "nothing", "error"];

/// (frequency Hz, duration ms) steps per event: rising = fine, falling =
/// attention, buzz = broken.
pub fn default_tones() -> Vec<(&'static str, Vec<(f64, f64)>)> {
    vec![
        ("trigger", vec![(660.0, 60.0)]),
        ("contact", vec![(780.0, 70.0), (980.0, 90.0)]),
        (
            "flagged",
            vec![(1180.0, 90.0), (880.0, 90.0), (1180.0, 140.0)],
        ),
        ("known", vec![(1180.0, 90.0), (760.0, 160.0)]),
        ("nothing", vec![(420.0, 110.0)]),
        ("error", vec![(240.0, 180.0), (200.0, 220.0)]),
    ]
}

pub const RATE: u32 = 44100;

/// A mono 16-bit WAV of sine segments, each faded in and out over 6 ms so
/// it does not click.
pub fn tone_wav(steps: &[(f64, f64)], volume: f64, rate: u32) -> Vec<u8> {
    let r = rate as f64;
    let mut frames: Vec<u8> = Vec::new();
    for &(freq, ms) in steps {
        let n = (r * ms / 1000.0) as i64;
        let fade = ((r * 0.006) as i64).max(1) as f64;
        for i in 0..n.max(0) {
            let fi = i as f64;
            let env = 1.0f64.min(fi / fade).min((n - i) as f64 / fade);
            let v = (2.0 * std::f64::consts::PI * freq * fi / r).sin() * env * volume;
            let sample = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
            frames.extend(sample.to_le_bytes());
        }
    }
    let data = frames.len() as u32;
    let mut wav = Vec::with_capacity(44 + frames.len());
    wav.extend(b"RIFF");
    wav.extend((36 + data).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes()); // PCM
    wav.extend(1u16.to_le_bytes()); // mono
    wav.extend(rate.to_le_bytes());
    wav.extend((rate * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data.to_le_bytes());
    wav.extend(frames);
    wav
}
