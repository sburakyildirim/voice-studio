//! Reusable voice profiles built from user-authorized reference audio.

use crate::audio::{self, AudioBuf, QualityReport};
use crate::fsutil;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PROFILE_SCHEMA: u32 = 1;
pub const REF_SAMPLE_RATE: u32 = 24000;
pub const CONSENT_TEXT: &str = "I confirm that I am the person in this recording, or that I have the speaker's explicit permission to create a synthetic copy of their voice, and that I will not use it to deceive, impersonate or harm anyone.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Consent {
    pub acknowledged: bool,
    pub statement: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceProfile {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub source_file: String,
    pub trim_start_secs: f64,
    pub trim_end_secs: f64,
    pub duration_secs: f64,
    pub sha256: String,
    pub quality: QualityReport,
    pub consent: Consent,
    #[serde(default)]
    pub notes: String,
}

/// A decoded, not-yet-saved reference (for preview/trim in the UI).
pub struct PendingReference {
    pub audio: AudioBuf,
    pub source_sr: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReferencePreview {
    pub duration_secs: f64,
    pub source_sample_rate: u32,
    pub suggested_start: f64,
    pub suggested_end: f64,
    pub peaks: Vec<(f32, f32)>,
    pub quality: QualityReport,
}

pub fn load_reference(path: &Path) -> Result<PendingReference> {
    let raw = audio::decode_file(path).with_context(|| format!("cannot read {}", path.display()))?;
    let sr = raw.sample_rate;
    if raw.duration_secs() < 1.0 {
        bail!("reference is shorter than one second");
    }
    if raw.duration_secs() > 600.0 {
        bail!("reference is longer than 10 minutes; trim it first");
    }
    let mut a = audio::resample(&raw, REF_SAMPLE_RATE);
    audio::remove_dc(&mut a);
    Ok(PendingReference { audio: a, source_sr: sr })
}

pub fn preview(r: &PendingReference) -> ReferencePreview {
    let (s, e) = audio::silence_bounds(&r.audio, -45.0, 100.0);
    let sr = r.audio.sample_rate as f64;
    let start = s as f64 / sr;
    let mut end = e as f64 / sr;
    if end - start > 60.0 {
        end = start + 60.0;
    }
    ReferencePreview {
        duration_secs: r.audio.duration_secs(),
        source_sample_rate: r.source_sr,
        suggested_start: start,
        suggested_end: end,
        peaks: audio::waveform_peaks(&r.audio, 600),
        quality: audio::analyze_quality(&slice(&r.audio, start, end), r.source_sr),
    }
}

pub fn slice(a: &AudioBuf, start: f64, end: f64) -> AudioBuf {
    let sr = a.sample_rate as f64;
    let s = ((start.max(0.0)) * sr) as usize;
    let e = ((end * sr) as usize).min(a.samples.len()).max(s);
    AudioBuf::new(a.sample_rate, a.samples[s..e].to_vec())
}

/// Lowercase ASCII slug for ids/folder names ("Synthetic Emma (Kokoro)" -> "synthetic-emma-kokoro").
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    // Letters that do not decompose to ASCII under NFD.
    let folded: String = name
        .chars()
        .map(|c| match c {
            'ı' => "i".to_string(),
            'ß' => "ss".to_string(),
            'ø' | 'Ø' => "o".to_string(),
            'æ' | 'Æ' => "ae".to_string(),
            'đ' | 'Đ' => "d".to_string(),
            'ł' | 'Ł' => "l".to_string(),
            c => c.to_string(),
        })
        .collect();
    for c in unicode_normalization::UnicodeNormalization::nfd(folded.as_str()).filter(|c| c.is_ascii()) {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let s = out.trim_matches('-').chars().take(40).collect::<String>();
    if s.is_empty() {
        "voice".into()
    } else {
        s
    }
}

pub struct ProfileStore {
    pub root: PathBuf,
}

impl ProfileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.root.join(fsutil::sanitize_file_name(id))
    }

    pub fn reference_path(&self, id: &str) -> PathBuf {
        self.dir(id).join("reference.wav")
    }

    /// Save a trimmed reference as a named profile. Consent is mandatory.
    pub fn save(&self, name: &str, source: &Path, r: &PendingReference, start: f64, end: f64, consent: bool) -> Result<VoiceProfile> {
        if !consent {
            bail!("You must confirm you have the rights and consent to use this voice.");
        }
        let name = name.trim();
        if name.is_empty() {
            bail!("Profile name is required");
        }
        if end - start < 3.0 {
            bail!("Selected region is too short (minimum 3 s, 30–60 s recommended)");
        }
        let mut clip = slice(&r.audio, start, end);
        audio::apply_fades(&mut clip, 15.0, 15.0);
        audio::normalize_loudness(&mut clip, -20.0, -1.5);
        let quality = audio::analyze_quality(&clip, r.source_sr);
        let id = format!("{}-{}", slug(name), &uuid::Uuid::new_v4().simple().to_string()[..6]);
        let dir = self.dir(&id);
        std::fs::create_dir_all(&dir)?;
        let wav = dir.join("reference.wav");
        audio::write_wav(&wav, &clip)?;
        let p = VoiceProfile {
            schema_version: PROFILE_SCHEMA,
            id: id.clone(),
            name: name.to_string(),
            created_at: chrono::Utc::now(),
            source_file: source.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
            trim_start_secs: start,
            trim_end_secs: end,
            duration_secs: clip.duration_secs(),
            sha256: fsutil::sha256_file(&wav)?,
            quality,
            consent: Consent { acknowledged: true, statement: CONSENT_TEXT.into(), at: chrono::Utc::now() },
            notes: String::new(),
        };
        fsutil::atomic_write_json(&dir.join("profile.json"), &p)?;
        Ok(p)
    }

    pub fn list(&self) -> Vec<VoiceProfile> {
        let mut v: Vec<VoiceProfile> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| fsutil::read_json(&e.path().join("profile.json")).ok())
            .collect();
        v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        v
    }

    pub fn get(&self, id: &str) -> Result<VoiceProfile> {
        fsutil::read_json(&self.dir(id).join("profile.json"))
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<VoiceProfile> {
        let mut p = self.get(id)?;
        p.name = name.trim().to_string();
        fsutil::atomic_write_json(&self.dir(id).join("profile.json"), &p)?;
        Ok(p)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let d = self.dir(id);
        if d.exists() && fsutil::is_within(&self.root, &d) && d != self.root {
            std::fs::remove_dir_all(d)?;
        }
        Ok(())
    }

    /// Verify the stored reference still matches its checksum (detects corruption).
    pub fn verify(&self, id: &str) -> Result<()> {
        let p = self.get(id)?;
        let h = fsutil::sha256_file(&self.reference_path(id))?;
        if h != p.sha256 {
            bail!("reference audio for '{}' is corrupt or was modified", p.name);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slugs() {
        assert_eq!(slug("Synthetic Emma (Kokoro)"), "synthetic-emma-kokoro");
        assert_eq!(slug("Çağrı Öz"), "cagri-oz");
        assert_eq!(slug("!!!"), "voice");
    }

    #[test]
    fn save_requires_consent_and_roundtrips() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("ref.wav");
        let n = 44100 * 20;
        let a = AudioBuf::new(44100, (0..n).map(|i| 0.2 * ((i as f32) * 0.03).sin() * (((i / 8000) % 2) as f32)).collect());
        audio::write_wav(&src, &a).unwrap();
        let r = load_reference(&src).unwrap();
        assert_eq!(r.audio.sample_rate, REF_SAMPLE_RATE);
        let pv = preview(&r);
        assert!(pv.duration_secs > 19.0);
        let store = ProfileStore::new(d.path().join("voices"));
        assert!(store.save("Me", &src, &r, 0.0, 15.0, false).is_err());
        let p = store.save("Me", &src, &r, 0.0, 15.0, true).unwrap();
        assert!(store.reference_path(&p.id).exists());
        store.verify(&p.id).unwrap();
        assert_eq!(store.list().len(), 1);
        store.rename(&p.id, "Me 2").unwrap();
        assert_eq!(store.get(&p.id).unwrap().name, "Me 2");
        store.delete(&p.id).unwrap();
        assert!(store.list().is_empty());
    }
}
