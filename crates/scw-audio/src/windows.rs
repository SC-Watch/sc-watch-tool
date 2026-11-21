//! Windows' voice and tone player, as audio.py uses them: SAPI 5 driven
//! directly (pyttsx3 once spoke only the first line of every session, and
//! nothing noticed), synchronous, the text always plain - SAPI reads a line
//! starting with "<" as markup, and some of what is spoken comes from
//! outside (a shared report's author name); and winmm's PlaySound, which
//! waits for the tone to finish so the next one does not cut it off.

use std::path::Path;

use windows::Win32::Media::Audio::{PlaySoundW, SND_FILENAME};
use windows::Win32::Media::Speech::{
    ISpObjectToken, ISpObjectTokenCategory, ISpVoice, SPCAT_VOICES, SPF_IS_NOT_XML,
    SpObjectTokenCategory, SpVoice,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::core::{HSTRING, PCWSTR};

use crate::alerts::{TonePlayer, Voice, VoiceMaker};
use crate::config::AudioConfig;
use crate::phrases::sapi_rate;

/// The SAPI 5 voice. Made on the thread that speaks with it (COM: a voice
/// belongs to the apartment of the thread that made it).
pub struct SapiVoice {
    voice: ISpVoice,
}

/// The installed voices' descriptions, in SAPI's order.
pub fn voice_names() -> Vec<String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        voice_tokens()
            .map(|t| t.into_iter().map(|(name, _)| name).collect())
            .unwrap_or_default()
    }
}

unsafe fn voice_tokens() -> windows::core::Result<Vec<(String, ISpObjectToken)>> {
    unsafe {
        let cat: ISpObjectTokenCategory =
            CoCreateInstance(&SpObjectTokenCategory, None, CLSCTX_ALL)?;
        cat.SetId(SPCAT_VOICES, false)?;
        let tokens = cat.EnumTokens(PCWSTR::null(), PCWSTR::null())?;
        let mut count = 0u32;
        tokens.GetCount(&mut count)?;
        let mut out = Vec::new();
        for i in 0..count {
            let token = tokens.Item(i)?;
            let name = token
                .GetStringValue(PCWSTR::null())
                .ok()
                .and_then(|p| p.to_string().ok())
                .unwrap_or_default();
            out.push((name, token));
        }
        Ok(out)
    }
}

impl SapiVoice {
    /// The voice at audio.json's volume and rate; the first installed voice
    /// whose description contains `cfg.voice` (any case), when set.
    pub fn new(cfg: &AudioConfig) -> Result<Self, String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let voice: ISpVoice =
                CoCreateInstance(&SpVoice, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            let volume = (cfg.volume.clamp(0.0, 1.0) * 100.0).round_ties_even() as u16;
            voice.SetVolume(volume).map_err(|e| e.to_string())?;
            voice
                .SetRate(sapi_rate(cfg.rate as f64))
                .map_err(|e| e.to_string())?;
            if !cfg.voice.is_empty() {
                let want = cfg.voice.to_lowercase();
                if let Ok(tokens) = voice_tokens()
                    && let Some((_, t)) = tokens
                        .iter()
                        .find(|(n, _)| n.to_lowercase().contains(&want))
                {
                    voice.SetVoice(t).map_err(|e| e.to_string())?;
                }
            }
            Ok(Self { voice })
        }
    }

    /// A maker for [`crate::alerts::Alerts`], run on its worker thread.
    pub fn maker() -> VoiceMaker {
        Box::new(|cfg| SapiVoice::new(cfg).map(|v| Box::new(v) as Box<dyn Voice>))
    }

    /// The underlying voice, for tests that point it at a file.
    pub fn raw(&self) -> &ISpVoice {
        &self.voice
    }
}

impl Voice for SapiVoice {
    fn speak(&mut self, text: &str) -> Result<(), String> {
        let text = HSTRING::from(text);
        unsafe {
            self.voice
                .Speak(&text, SPF_IS_NOT_XML.0 as u32, None)
                .map_err(|e| e.to_string())
        }
    }
}

/// winmm's PlaySound, waiting for the end.
#[derive(Clone, Copy, Debug, Default)]
pub struct WinTones;

impl TonePlayer for WinTones {
    fn play(&mut self, wav: &Path) -> Result<(), String> {
        let path = HSTRING::from(wav.as_os_str());
        if unsafe { PlaySoundW(&path, None, SND_FILENAME) }.as_bool() {
            Ok(())
        } else {
            Err(format!("PlaySound refused {}", wav.display()))
        }
    }
}
