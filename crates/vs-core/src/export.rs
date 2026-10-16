//! Optional MP3 export through an installed FFmpeg (LAME). WAV is always produced natively;
//! MP3 is only offered when a working ffmpeg with libmp3lame is found.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn find_ffmpeg(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = configured.filter(|p| p.is_file()) {
        return Some(p.to_path_buf());
    }
    crate::pyenv::which(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" })
}

/// True if ffmpeg exists and was built with the LAME MP3 encoder. The probe result is
/// cached per ffmpeg path, so the UI can ask often without spawning processes.
pub fn mp3_supported(configured: Option<&Path>) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let Some(ff) = find_ffmpeg(configured) else { return false };
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap().get(&ff) {
        return *v;
    }
    let v = probe_mp3(&ff);
    cache.lock().unwrap().insert(ff, v);
    v
}

fn probe_mp3(ff: &Path) -> bool {
    crate::pyenv::no_window(Command::new(ff).args(["-hide_banner", "-encoders"]))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("libmp3lame"))
        .unwrap_or(false)
}

pub fn wav_to_mp3(wav: &Path, mp3: &Path, configured: Option<&Path>) -> Result<PathBuf> {
    let ff = find_ffmpeg(configured).context("FFmpeg not found (set its path in Options)")?;
    let tmp = mp3.with_extension("mp3.tmp");
    let out = crate::pyenv::no_window(
        Command::new(&ff)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(wav)
            .args(["-codec:a", "libmp3lame", "-b:a", "192k", "-f", "mp3"])
            .arg(&tmp),
    )
    .stdin(Stdio::null())
    .output()
    .context("run ffmpeg")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    crate::fsutil::replace_file(&tmp, mp3)?;
    Ok(mp3.to_path_buf())
}
