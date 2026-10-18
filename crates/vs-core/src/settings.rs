//! Application settings and well-known directories. Versioned, written atomically.

use crate::fsutil;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SETTINGS_SCHEMA: u32 = 1;

/// Root for app-owned data (settings, runtimes, cache, queue). Lives outside the install
/// directory so models and settings survive app updates and reinstalls.
pub fn data_root() -> PathBuf {
    if let Ok(p) = std::env::var("VOICE_STUDIO_HOME") {
        return PathBuf::from(p);
    }
    if let Some(p) = portable_root() {
        return p;
    }
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("VoiceStudio")
}

/// Portable edition: a file named `portable.txt` next to the executable keeps all data
/// (settings, models, runtimes, cache, outputs) in a `VoiceStudioData` folder beside it.
pub fn portable_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    dir.join("portable.txt").exists().then(|| dir.join("VoiceStudioData"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    /// Where model packages are stored (user-selectable).
    pub models_dir: PathBuf,
    /// Where projects and final renders are written (user-selectable).
    pub output_dir: PathBuf,
    pub pause_short_ms: f32,
    pub pause_long_ms: f32,
    pub pause_paragraph_ms: f32,
    /// Gap between consecutive sentences/segments with no explicit pause.
    pub segment_gap_ms: f32,
    pub fade_ms: f32,
    pub loudness_target_lufs: f64,
    pub peak_ceiling_db: f32,
    /// "auto" | "cpu" | "gpu"
    pub device: String,
    /// Unload models after this many idle seconds (0 = keep until exit).
    pub idle_unload_secs: u64,
    pub ffmpeg_path: Option<PathBuf>,
    pub export_mp3: bool,
    pub advanced_mode: bool,
    pub last_engine: String,
    pub last_voice: String,
    pub last_profile: Option<String>,
    /// Kokoro model variant package id.
    pub kokoro_variant: String,
    pub chatterbox_seed: u64,
    pub consent_acknowledged: bool,
    /// Optional location of isolated runtimes (default: <data>/runtimes).
    pub runtimes_dir: Option<PathBuf>,
    /// Last language used with Chatterbox Multilingual.
    pub last_language: String,
}

impl Default for Settings {
    fn default() -> Self {
        let root = data_root();
        let docs = if portable_root().is_some() { root.clone() } else { dirs::document_dir().unwrap_or_else(|| root.clone()) };
        Self {
            schema_version: SETTINGS_SCHEMA,
            models_dir: root.join("models"),
            output_dir: docs.join("Voice Studio"),
            pause_short_ms: 350.0,
            pause_long_ms: 900.0,
            pause_paragraph_ms: 650.0,
            segment_gap_ms: 140.0,
            fade_ms: 6.0,
            loudness_target_lufs: -16.0,
            peak_ceiling_db: -1.0,
            device: "auto".into(),
            idle_unload_secs: 300,
            ffmpeg_path: None,
            export_mp3: false,
            advanced_mode: false,
            last_engine: "kokoro".into(),
            last_voice: "af_heart".into(),
            last_profile: None,
            kokoro_variant: "kokoro-82m-fp32".into(),
            chatterbox_seed: 1234,
            consent_acknowledged: false,
            runtimes_dir: None,
            last_language: "en".into(),
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        data_root().join("settings.json")
    }

    /// Load settings; a missing or corrupt file yields defaults (the corrupt file is kept
    /// aside as settings.json.bad for diagnostics).
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        match fsutil::read_json::<Settings>(path) {
            Ok(mut s) => {
                if s.schema_version > SETTINGS_SCHEMA {
                    tracing::warn!("settings written by a newer version; unknown fields ignored");
                }
                s.schema_version = SETTINGS_SCHEMA;
                s
            }
            Err(e) => {
                if path.exists() {
                    tracing::warn!("settings unreadable ({e}); using defaults");
                    let _ = std::fs::rename(path, path.with_extension("json.bad"));
                }
                Settings::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        fsutil::atomic_write_json(&Self::path(), self)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        fsutil::atomic_write_json(path, self)
    }

    pub fn runtimes_dir(&self) -> PathBuf {
        self.runtimes_dir.clone().unwrap_or_else(|| data_root().join("runtimes"))
    }
    pub fn cache_dir(&self) -> PathBuf {
        data_root().join("cache")
    }
    pub fn profiles_dir(&self) -> PathBuf {
        data_root().join("voices")
    }
    pub fn queue_path(&self) -> PathBuf {
        data_root().join("queue.json")
    }
    pub fn logs_dir(&self) -> PathBuf {
        data_root().join("logs")
    }
    pub fn benchmarks_path(&self) -> PathBuf {
        data_root().join("benchmarks.json")
    }
    pub fn projects_dir(&self) -> PathBuf {
        self.output_dir.join("Projects")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupt_settings_fall_back() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        std::fs::write(&p, b"{not json").unwrap();
        let s = Settings::load_from(&p);
        assert_eq!(s.schema_version, SETTINGS_SCHEMA);
        assert!(p.with_extension("json.bad").exists());
    }
    #[test]
    fn unknown_and_missing_fields_tolerated() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        std::fs::write(&p, br#"{"schema_version":1,"pause_short_ms":500,"future_field":true}"#).unwrap();
        let s = Settings::load_from(&p);
        assert_eq!(s.pause_short_ms, 500.0);
        assert_eq!(s.pause_long_ms, 900.0);
    }
}
