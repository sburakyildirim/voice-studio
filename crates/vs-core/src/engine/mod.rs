//! TTS engines and the honest mapping from script markup to engine controls.

pub mod chatterbox;
pub mod kokoro;

use crate::audio::AudioBuf;
use crate::download::Cancel;
use crate::markup::Emotion;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// A text run inside one segment; emphasis is applied per run where the engine can.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Run {
    pub text: String,
    pub emphasis: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CloneMode {
    /// Use the reference for speaker identity; script markup controls delivery.
    #[default]
    VoiceOnly,
    /// Also follow the reference's energy/cadence (markup intensity disabled).
    VoiceStyle,
}

/// How faithfully an engine realizes a requested control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    /// The model has a real control for this (e.g. Chatterbox exaggeration).
    Genuine,
    /// Approximated with rate / gain / pause / punctuation changes.
    Approximate,
    /// Not supported: ignored.
    Unsupported,
    /// Not requested (neutral).
    None,
}

/// Engine-level parameters for one segment, derived from markup. Serialized into the cache
/// key, so any change here invalidates cached audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StyleParams {
    pub emotion: Emotion,
    pub speed: f32,
    pub gain_db: f32,
    pub exaggeration: f32,
    pub cfg_weight: f32,
    pub temperature: f32,
    /// Multiplier for the silence that follows this segment.
    pub pause_scale: f32,
    /// Replace a final '.' with this punctuation (Kokoro intonation cue).
    pub terminal_punct: Option<char>,
    pub emotion_fidelity: Fidelity,
    pub emphasis_fidelity: Fidelity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub engine: String,
    pub cloning: bool,
    pub clone_modes: Vec<CloneMode>,
    pub emotion: Fidelity,
    pub emphasis: Fidelity,
    pub sample_rate: u32,
    pub notes: Vec<String>,
}

pub struct SegmentRequest<'a> {
    pub runs: &'a [Run],
    pub style: &'a StyleParams,
    /// Kokoro voice id (e.g. "af_heart").
    pub voice: &'a str,
    /// Reference WAV for cloning engines.
    pub reference: Option<&'a std::path::Path>,
    pub seed: u64,
}

pub trait Engine: Send {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    fn is_loaded(&self) -> bool;
    /// Load weights. Called lazily on the first MAKE; never at app start.
    fn load(&mut self, device: &str, log: &mut dyn FnMut(String)) -> Result<()>;
    fn unload(&mut self);
    fn device(&self) -> String;
    /// Identifier of the exact model weights (changes invalidate the cache).
    fn model_fingerprint(&self) -> String;
    fn synthesize(&mut self, req: &SegmentRequest, cancel: &Cancel) -> Result<AudioBuf>;
}

/// Map markup to engine parameters. `emotion_control=false` renders everything neutral
/// (pauses still apply). This is the single place where approximations are decided, and
/// every approximation is labelled via `Fidelity`.
pub fn plan_style(engine: &str, emotion: Emotion, emotion_control: bool, clone_mode: CloneMode) -> StyleParams {
    let emo = if emotion_control { emotion } else { Emotion::Neutral };
    match engine {
        "chatterbox" => {
            let style_from_ref = clone_mode == CloneMode::VoiceStyle;
            let (ex, cfg, temp, pause, fid) = if style_from_ref {
                // Reference delivery dominates: neutral intensity, lower CFG so the speech
                // prompt's prosody carries through. Markup emotions are not applied.
                (0.5, 0.3, 0.8, 1.0, if emo == Emotion::Neutral { Fidelity::None } else { Fidelity::Unsupported })
            } else {
                match emo {
                    Emotion::Neutral => (0.5, 0.5, 0.8, 1.0, Fidelity::None),
                    Emotion::Excited => (0.95, 0.3, 0.85, 0.85, Fidelity::Genuine),
                    Emotion::Calm => (0.3, 0.5, 0.7, 1.15, Fidelity::Genuine),
                    // No discrete suspense emotion in Chatterbox: low intensity + slower
                    // pacing via CFG and longer following pause.
                    Emotion::Mystery => (0.35, 0.6, 0.7, 1.5, Fidelity::Approximate),
                }
            };
            StyleParams {
                emotion: emo,
                speed: 1.0,
                gain_db: match emo {
                    Emotion::Mystery => -1.5,
                    _ => 0.0,
                },
                exaggeration: ex,
                cfg_weight: cfg,
                temperature: temp,
                pause_scale: pause,
                terminal_punct: None,
                emotion_fidelity: fid,
                emphasis_fidelity: Fidelity::Unsupported,
            }
        }
        _ => {
            // Kokoro: no emotion conditioning at all. Approximate with rate, level, final
            // punctuation (which does change Kokoro's intonation) and pause length.
            let (speed, gain, pause, punct) = match emo {
                Emotion::Neutral => (1.0, 0.0, 1.0, None),
                Emotion::Excited => (1.12, 1.5, 0.8, Some('!')),
                Emotion::Calm => (0.9, -1.5, 1.15, None),
                Emotion::Mystery => (0.84, -3.0, 1.5, Some('…')),
            };
            StyleParams {
                emotion: emo,
                speed,
                gain_db: gain,
                exaggeration: 0.0,
                cfg_weight: 0.0,
                temperature: 0.0,
                pause_scale: pause,
                terminal_punct: punct,
                emotion_fidelity: if emo == Emotion::Neutral { Fidelity::None } else { Fidelity::Approximate },
                emphasis_fidelity: Fidelity::Approximate,
            }
        }
    }
}

/// Plain text of the runs, applying the terminal punctuation cue.
pub fn runs_text(runs: &[Run], terminal: Option<char>) -> String {
    let mut s = runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("");
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(p) = terminal {
        let trimmed = s.trim_end();
        if trimmed.ends_with('.') && !trimmed.ends_with("..") {
            s = format!("{}{}", &trimmed[..trimmed.len() - 1], p);
        } else if !trimmed.ends_with(['!', '?', '…', '.', ',', ';', ':']) {
            s = format!("{trimmed}{p}");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kokoro_emotions_are_labelled_approximate() {
        for e in [Emotion::Excited, Emotion::Calm, Emotion::Mystery] {
            assert_eq!(plan_style("kokoro", e, true, CloneMode::VoiceOnly).emotion_fidelity, Fidelity::Approximate);
        }
        assert_eq!(plan_style("kokoro", Emotion::Neutral, true, CloneMode::VoiceOnly).emotion_fidelity, Fidelity::None);
    }
    #[test]
    fn chatterbox_voice_style_disables_markup_intensity() {
        let s = plan_style("chatterbox", Emotion::Excited, true, CloneMode::VoiceStyle);
        assert_eq!(s.exaggeration, 0.5);
        assert_eq!(s.emotion_fidelity, Fidelity::Unsupported);
        let s = plan_style("chatterbox", Emotion::Excited, true, CloneMode::VoiceOnly);
        assert!(s.exaggeration > 0.8);
        assert_eq!(s.emotion_fidelity, Fidelity::Genuine);
    }
    #[test]
    fn emotion_control_off_is_neutral() {
        let s = plan_style("chatterbox", Emotion::Excited, false, CloneMode::VoiceOnly);
        assert_eq!(s.emotion, Emotion::Neutral);
    }
    #[test]
    fn terminal_punct() {
        let r = vec![Run { text: "Go now.".into(), emphasis: false }];
        assert_eq!(runs_text(&r, Some('!')), "Go now!");
        let r = vec![Run { text: "Wait".into(), emphasis: false }];
        assert_eq!(runs_text(&r, Some('…')), "Wait…");
        let r = vec![Run { text: "Really?".into(), emphasis: false }];
        assert_eq!(runs_text(&r, Some('!')), "Really?");
    }
}
