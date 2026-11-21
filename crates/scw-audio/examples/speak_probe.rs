//! Measures that the Windows voice and tone player really produce sound, not
//! just return: the voice speaks a line into a WAV file (its audio is
//! measured), then the same line to the speakers at volume 0 (the call must
//! last about as long as that audio - pyttsx3 once returned in 0.1 s
//! without a sound); a silent tone must take its own length to play.
//!
//!     cargo run --release -p scw-audio --example speak_probe
#[cfg(windows)]
fn main() {
    use scw_audio::windows::{SapiVoice, WinTones, voice_names};
    use scw_audio::{AudioConfig, TonePlayer, Voice};
    use std::time::Instant;
    use windows::Win32::Media::Audio::{WAVE_FORMAT_PCM, WAVEFORMATEX};
    use windows::Win32::Media::Speech::{ISpStream, SPFM_CREATE_ALWAYS, SpStream};
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::core::{GUID, HSTRING};
    // SAPI's SPDFID_WaveFormatEx (sapi.h), which the bindings leave out.
    const SPDFID_WAVE_FORMAT_EX: GUID = GUID::from_u128(0xc31adbae_527f_4ff5_a230_f62bb61ff70c);

    let names = voice_names();
    println!("{} voices: {}", names.len(), names.join(" | "));
    let line = "Warning. Sample pilot. 2.4 kilometres. Piracy";
    let dir = std::env::temp_dir().join(format!("scw-speak-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("line.wav");

    // 1. Into a file.
    let mut cfg = AudioConfig::default();
    let voice = SapiVoice::new(&cfg).expect("voice");
    unsafe {
        let stream: ISpStream = CoCreateInstance(&SpStream, None, CLSCTX_ALL).expect("stream");
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 1,
            nSamplesPerSec: 22050,
            nAvgBytesPerSec: 44100,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        stream
            .BindToFile(
                &HSTRING::from(wav.as_os_str()),
                SPFM_CREATE_ALWAYS,
                Some(&SPDFID_WAVE_FORMAT_EX),
                Some(&fmt),
                0,
            )
            .expect("bind");
        voice.raw().SetOutput(&stream, true).expect("output");
        let mut v = voice;
        let t = Instant::now();
        v.speak(line).expect("speak to file");
        println!("spoken into a file in {:.2} s", t.elapsed().as_secs_f64());
        stream.Close().expect("close");
    }
    let bytes = std::fs::read(&wav).unwrap();
    let samples: Vec<i16> = bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect();
    let secs = samples.len() as f64 / 22050.0;
    let rms = (samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>()
        / samples.len().max(1) as f64)
        .sqrt();
    let loud = samples.iter().filter(|s| s.unsigned_abs() > 1000).count();
    println!(
        "the file: {secs:.2} s of audio, RMS {rms:.0}, {:.0}% of samples above 1000",
        100.0 * loud as f64 / samples.len().max(1) as f64
    );

    // 2. To the speakers, silently: the call must take about as long.
    cfg.volume = 0.0;
    let mut quiet = SapiVoice::new(&cfg).expect("voice");
    let t = Instant::now();
    quiet.speak(line).expect("speak");
    let took = t.elapsed().as_secs_f64();
    println!(
        "spoken to the speakers at volume 0 in {took:.2} s ({:.0}% of the audio's length)",
        100.0 * took / secs
    );
    let t = Instant::now();
    quiet.speak(line).expect("speak again");
    println!(
        "and again in {:.2} s (the second line of a session is where pyttsx3 went silent)",
        t.elapsed().as_secs_f64()
    );

    // 3. A silent tone: PlaySound must wait for its end.
    let tone = dir.join("flagged.wav");
    let steps = [(1180.0, 90.0), (880.0, 90.0), (1180.0, 140.0)];
    std::fs::write(&tone, scw_audio::tones::tone_wav(&steps, 0.0, 44100)).unwrap();
    let t = Instant::now();
    WinTones.play(&tone).expect("play");
    println!("a 0.32 s tone played in {:.2} s", t.elapsed().as_secs_f64());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the voice and tone player are Windows-only so far");
}
