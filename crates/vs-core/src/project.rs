//! Project folders: metadata, script, settings, reference, segments, final renders.

use crate::engine::{CloneMode, Fidelity};
use crate::fsutil;
use crate::markup::{Emotion, Warning};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PROJECT_SCHEMA: u32 = 1;
pub const PROJECT_FILE: &str = "project.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentMeta {
    pub index: usize,
    pub text: String,
    pub emotion: Emotion,
    pub emphasis_words: usize,
    pub source_start: usize,
    pub source_end: usize,
    pub file: String,
    pub duration_secs: f64,
    /// Offset of the segment inside final.wav.
    pub offset_secs: f64,
    pub pause_after_ms: f32,
    pub cache_key: String,
    pub cached: bool,
    pub emotion_fidelity: Fidelity,
    pub emphasis_fidelity: Fidelity,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderStats {
    pub render_secs: f64,
    pub audio_secs: f64,
    /// Real-time factor = processing time / audio duration (lower is faster).
    pub rtf: f64,
    pub segments_total: usize,
    pub segments_cached: usize,
    pub device: String,
    pub loudness_before_lufs: Option<f64>,
    pub loudness_after_lufs: Option<f64>,
    pub peak_rss_mb: Option<f64>,
    pub peak_vram_mb: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub engine: String,
    pub model: String,
    pub voice: String,
    pub profile_id: Option<String>,
    pub profile_name: Option<String>,
    pub clone_mode: Option<CloneMode>,
    pub emotion_control: bool,
    pub settings: serde_json::Value,
    pub segments: Vec<SegmentMeta>,
    pub final_wav: String,
    pub final_mp3: Option<String>,
    pub duration_secs: f64,
    pub stats: RenderStats,
    pub warnings: Vec<Warning>,
    pub app_version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectSummary {
    pub id: String,
    pub title: String,
    pub dir: PathBuf,
    pub final_wav: PathBuf,
    pub final_mp3: Option<PathBuf>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub duration_secs: f64,
    pub engine: String,
    pub voice: String,
    pub script_preview: String,
}

pub fn new_project_dir(projects_root: &Path, title: &str, id: &str) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H%M%S");
    let name = format!("{stamp} {} [{}]", fsutil::sanitize_file_name(title), &id[..6.min(id.len())]);
    fsutil::unique_path(&projects_root.join(name))
}

pub fn load(dir: &Path) -> Result<ProjectMeta> {
    fsutil::read_json(&dir.join(PROJECT_FILE))
}

pub fn save(dir: &Path, meta: &ProjectMeta) -> Result<()> {
    fsutil::atomic_write_json(&dir.join(PROJECT_FILE), meta)
}

/// List projects (newest first), optionally filtered by a case-insensitive query matched
/// against title, voice, engine and the script text.
pub fn list(projects_root: &Path, query: &str) -> Vec<ProjectSummary> {
    let q = query.trim().to_lowercase();
    let mut out: Vec<ProjectSummary> = std::fs::read_dir(projects_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            let m = load(&dir).ok()?;
            let script = std::fs::read_to_string(dir.join("script.txt")).unwrap_or_default();
            if !q.is_empty() {
                let hay = format!("{} {} {} {} {}", m.title, m.voice, m.engine, m.profile_name.clone().unwrap_or_default(), script).to_lowercase();
                if !q.split_whitespace().all(|t| hay.contains(t)) {
                    return None;
                }
            }
            Some(ProjectSummary {
                id: m.id.clone(),
                title: m.title.clone(),
                final_wav: dir.join(&m.final_wav),
                final_mp3: m.final_mp3.as_ref().map(|p| dir.join(p)),
                dir,
                created_at: m.created_at,
                duration_secs: m.duration_secs,
                engine: m.engine,
                voice: m.profile_name.unwrap_or(m.voice),
                script_preview: crate::markup::plain_text(&script).chars().take(160).collect(),
            })
        })
        .collect();
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out
}

/// Rename a project (title in metadata only; the folder name is stable so external
/// references keep working).
pub fn rename(dir: &Path, title: &str) -> Result<()> {
    let mut m = load(dir)?;
    m.title = title.trim().to_string();
    save(dir, &m)
}

/// Delete a project folder (must be inside the projects root).
pub fn delete(projects_root: &Path, dir: &Path) -> Result<()> {
    if fsutil::is_within(projects_root, dir) && dir != projects_root && dir.join(PROJECT_FILE).exists() {
        std::fs::remove_dir_all(dir)?;
        Ok(())
    } else {
        anyhow::bail!("not a project folder: {}", dir.display())
    }
}
