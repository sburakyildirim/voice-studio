//! Kokoro 82M via ONNX Runtime (dynamically loaded, so the app binary stays small and the
//! runtime is an optional, removable download).

use super::{Capabilities, CloneMode, Engine, Fidelity, Run, SegmentRequest};
use crate::audio::AudioBuf;
use crate::catalog::Store;
use crate::download::Cancel;
use crate::g2p::{self, G2p, Lexicon};
use anyhow::{anyhow, bail, Context, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const SAMPLE_RATE: u32 = 24000;
const MAX_TOKENS: usize = 500;
const STYLE_DIM: usize = 256;

static ORT_ENV: OnceLock<std::result::Result<(PathBuf, ort::environment::Environment), String>> = OnceLock::new();

fn ort_lib_name() -> &'static str {
    if cfg!(windows) {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

/// Locate an installed ONNX Runtime package (preferring DirectML on Windows).
pub fn find_ort(store: &Store) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VOICE_STUDIO_ORT") {
        return Some(PathBuf::from(p));
    }
    crate::catalog::ort_package_candidates()
        .into_iter()
        .filter(|id| store.is_installed(id))
        .map(|id| store.file_path(&id, ort_lib_name()))
        .find(|p| p.exists())
}

/// Load ONNX Runtime once per process. A different path after the first successful load
/// requires an app restart (ONNX Runtime cannot be unloaded safely).
pub fn init_ort(lib: &Path) -> Result<ort::environment::Environment> {
    let r = ORT_ENV.get_or_init(|| {
        #[cfg(windows)]
        {
            // DirectML.dll sits next to onnxruntime.dll; make sure the loader finds that copy
            // rather than the older one in System32.
            if let Some(dir) = lib.parent() {
                let dml = dir.join("DirectML.dll");
                if dml.exists() {
                    let _ = preload(&dml);
                }
            }
        }
        let b = ort::init_from(lib).map_err(|e| format!("failed to load ONNX Runtime from {}: {e}", lib.display()))?;
        let env = b.with_name("voice-studio").build().map_err(|e| format!("ONNX Runtime init failed: {e}"))?;
        Ok((lib.to_path_buf(), env))
    });
    r.as_ref().map(|(_, e)| e.clone()).map_err(|e| anyhow!(e.clone()))
}

#[cfg(windows)]
fn preload(p: &Path) -> Result<()> {
    // Loading by full path pins this DLL for the process.
    unsafe {
        // The module handle is intentionally never freed.
        let _ = libloading_load(p);
    }
    Ok(())
}

#[cfg(windows)]
unsafe fn libloading_load(p: &Path) -> *mut core::ffi::c_void {
    use std::os::windows::ffi::OsStrExt;
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut core::ffi::c_void;
    }
    let w: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    LoadLibraryW(w.as_ptr())
}

/// Physical cores (capped): extra SMT threads and efficiency cores rarely help this model.
fn default_threads() -> usize {
    let phys = sysinfo::System::physical_core_count().unwrap_or(4);
    phys.clamp(1, 8)
}

pub struct KokoroPaths {
    pub model: PathBuf,
    pub voices_dir: PathBuf,
    pub lexicon_dir: PathBuf,
    pub ort_lib: PathBuf,
    pub model_sha: String,
}

impl KokoroPaths {
    /// Resolve all required files from the model store, with a precise error naming the
    /// missing package(s) so the UI can offer the download.
    pub fn resolve(store: &Store, variant: &str) -> Result<Self> {
        let mut missing = Vec::new();
        let variant = if store.is_installed(variant) {
            variant.to_string()
        } else if let Some(v) = ["kokoro-82m-fp32", "kokoro-82m-q8"].iter().find(|v| store.is_installed(v)) {
            v.to_string()
        } else {
            missing.push(variant.to_string());
            variant.to_string()
        };
        for p in ["kokoro-voices-en", "misaki-lexicon-en"] {
            if !store.is_installed(p) {
                missing.push(p.to_string());
            }
        }
        let ort_lib = find_ort(store);
        if ort_lib.is_none() {
            missing.push(crate::catalog::ort_package_candidates().first().cloned().unwrap_or("onnxruntime".into()));
        }
        if !missing.is_empty() {
            bail!("MISSING_PACKAGES:{}", missing.join(","));
        }
        let model_sha = store.manifest(&variant).and_then(|m| m.sources.first().cloned()).unwrap_or_default();
        Ok(Self {
            model: store.file_path(&variant, "model.onnx"),
            voices_dir: store.dir("kokoro-voices-en"),
            lexicon_dir: store.dir("misaki-lexicon-en"),
            ort_lib: ort_lib.unwrap(),
            model_sha,
        })
    }
}

pub struct Kokoro {
    paths: Option<KokoroPaths>,
    session: Option<Session>,
    g2p_us: Option<G2p>,
    g2p_gb: Option<G2p>,
    voices: HashMap<String, Vec<f32>>,
    device: String,
    fingerprint: String,
}

impl Kokoro {
    pub fn new(paths: KokoroPaths) -> Self {
        let fingerprint = format!("kokoro-v1.0:{}", &paths.model_sha);
        Self { paths: Some(paths), session: None, g2p_us: None, g2p_gb: None, voices: HashMap::new(), device: "unloaded".into(), fingerprint }
    }

    pub fn list_voices(voices_dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(voices_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.strip_suffix(".bin").map(|s| s.to_string())
            })
            .collect();
        v.sort_by_key(|n| (n != "af_heart", n.clone()));
        v
    }

    fn voice(&mut self, id: &str) -> Result<&Vec<f32>> {
        if !self.voices.contains_key(id) {
            let paths = self.paths.as_ref().unwrap();
            let safe = crate::fsutil::sanitize_file_name(id);
            let p = paths.voices_dir.join(format!("{safe}.bin"));
            let bytes = std::fs::read(&p).with_context(|| format!("voice '{id}' not installed"))?;
            if bytes.len() % (STYLE_DIM * 4) != 0 || bytes.is_empty() {
                bail!("voice file {} is corrupt", p.display());
            }
            let v: Vec<f32> = bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
            self.voices.insert(id.to_string(), v);
        }
        Ok(&self.voices[id])
    }

    fn base_builder(threads: usize) -> Result<ort::session::builder::SessionBuilder> {
        let env = ORT_ENV.get().and_then(|r| r.as_ref().ok()).map(|(_, e)| e.clone()).context("ONNX Runtime not initialized")?;
        Session::builder(&env)
            .map_err(|e| anyhow!("{e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow!("{e}"))?
            .with_intra_threads(threads)
            .map_err(|e| anyhow!("{e}"))
    }

    fn build_session(model: &Path, gpu: bool) -> Result<(Session, String)> {
        let threads = std::env::var("VS_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or_else(default_threads);
        #[cfg(windows)]
        if gpu {
            let attempt = (|| -> Result<Session> {
                let mut b = Self::base_builder(threads)?
                    .with_memory_pattern(false)
                    .map_err(|e| anyhow!("{e}"))?
                    .with_parallel_execution(false)
                    .map_err(|e| anyhow!("{e}"))?
                    .with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])
                    .map_err(|e| anyhow!("{e}"))?;
                b.commit_from_file(model).map_err(|e| anyhow!("{e}"))
            })();
            match attempt {
                Ok(s) => return Ok((s, "gpu:directml".into())),
                Err(e) => tracing::warn!("DirectML unavailable, falling back to CPU: {e:#}"),
            }
        }
        #[cfg(target_os = "macos")]
        if gpu {
            let attempt = (|| -> Result<Session> {
                let mut b = Self::base_builder(threads)?
                    .with_execution_providers([ort::ep::CoreML::default().build().error_on_failure()])
                    .map_err(|e| anyhow!("{e}"))?;
                b.commit_from_file(model).map_err(|e| anyhow!("{e}"))
            })();
            match attempt {
                Ok(s) => return Ok((s, "gpu:coreml".into())),
                Err(e) => tracing::warn!("CoreML unavailable, falling back to CPU: {e:#}"),
            }
        }
        let _ = gpu;
        let mut b = Self::base_builder(threads)?;
        let s = b.commit_from_file(model).map_err(|e| anyhow!("load Kokoro model: {e}"))?;
        Ok((s, format!("cpu:{threads}t")))
    }

    fn infer(&mut self, tokens: &[i64], voice: &str, speed: f32) -> Result<Vec<f32>> {
        let style: Vec<f32> = {
            let v = self.voice(voice)?;
            let rows = v.len() / STYLE_DIM;
            let idx = tokens.len().min(rows - 1);
            v[idx * STYLE_DIM..(idx + 1) * STYLE_DIM].to_vec()
        };
        let mut ids = Vec::with_capacity(tokens.len() + 2);
        ids.push(0i64);
        ids.extend_from_slice(tokens);
        ids.push(0);
        let n = ids.len();
        let session = self.session.as_mut().context("Kokoro not loaded")?;
        let input_ids = Tensor::from_array(([1usize, n], ids)).map_err(|e| anyhow!("{e}"))?;
        let style_t = Tensor::from_array(([1usize, STYLE_DIM], style)).map_err(|e| anyhow!("{e}"))?;
        let speed_t = Tensor::from_array(([1usize], vec![speed.clamp(0.5, 2.0)])).map_err(|e| anyhow!("{e}"))?;
        let t_run = std::time::Instant::now();
        let outputs = session
            .run(ort::inputs!["input_ids" => input_ids, "style" => style_t, "speed" => speed_t])
            .map_err(|e| anyhow!("Kokoro inference failed: {e}"))?;
        let (_, data) = outputs[0].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
        if std::env::var_os("VS_TIMING").is_some() {
            eprintln!("
[kokoro] {} tokens -> {:.2}s audio in {:.3}s", n, data.len() as f64 / 24000.0, t_run.elapsed().as_secs_f64());
        }
        if data.iter().any(|x| !x.is_finite()) {
            bail!("Kokoro produced invalid samples");
        }
        Ok(data.to_vec())
    }

    /// Phonemize runs (applying emphasis per run) into one phoneme string.
    /// Load the US or UK lexicon on first use (each is ~6 MB of JSON; only the accent in
    /// use is kept in memory).
    fn ensure_lexicon(&mut self, british: bool) -> Result<()> {
        let slot = if british { &mut self.g2p_gb } else { &mut self.g2p_us };
        if slot.is_none() {
            let dir = &self.paths.as_ref().context("no paths")?.lexicon_dir;
            let (g, s) = if british { ("gb_gold.json", "gb_silver.json") } else { ("us_gold.json", "us_silver.json") };
            *slot = Some(G2p::new(Lexicon::load(&dir.join(g), &dir.join(s), british)?));
        }
        Ok(())
    }

    pub fn phonemize(&self, runs: &[Run], terminal: Option<char>, british: bool) -> String {
        let g = if british { self.g2p_gb.as_ref() } else { self.g2p_us.as_ref() }.expect("lexicon loaded");
        let n = runs.len();
        let mut parts = Vec::new();
        for (i, r) in runs.iter().enumerate() {
            let mut text = r.text.clone();
            if i == n - 1 {
                text = super::runs_text(std::slice::from_ref(r), terminal);
            }
            let (p, _) = g.phonemize(&text, r.emphasis);
            if !p.is_empty() {
                parts.push(p);
            }
        }
        parts.join(" ")
    }
}

/// Split a phoneme string into chunks of at most `max` tokens at punctuation, then at
/// spaces.
pub fn chunk_phonemes(ps: &str, max: usize) -> Vec<String> {
    let chars: Vec<char> = ps.chars().collect();
    if chars.len() <= max {
        return vec![ps.to_string()];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if chars.len() - start <= max {
            out.push(chars[start..].iter().collect::<String>().trim().to_string());
            break;
        }
        let window = &chars[start..start + max];
        let cut = window
            .iter()
            .rposition(|c| matches!(c, '.' | '!' | '?' | '…'))
            .or_else(|| window.iter().rposition(|c| matches!(c, ',' | ';' | ':' | '—')))
            .or_else(|| window.iter().rposition(|c| *c == ' '))
            .map(|i| i + 1)
            .unwrap_or(max);
        out.push(chars[start..start + cut].iter().collect::<String>().trim().to_string());
        start += cut;
    }
    out.retain(|s| !s.is_empty());
    out
}

impl Engine for Kokoro {
    fn id(&self) -> &'static str {
        "kokoro"
    }

    fn capabilities(&self) -> Capabilities {
        caps()
    }

    fn is_loaded(&self) -> bool {
        self.session.is_some()
    }

    fn load(&mut self, device: &str, log: &mut dyn FnMut(String)) -> Result<()> {
        if self.session.is_some() {
            return Ok(());
        }
        let paths = self.paths.as_ref().context("no paths")?;
        log(format!("Loading ONNX Runtime from {}", paths.ort_lib.display()));
        init_ort(&paths.ort_lib)?;
        let t = std::time::Instant::now();
        // "auto" = CPU for Kokoro: on our test machine DirectML was no faster than 8 CPU
        // threads and adds ~5 s of graph compilation per load. "gpu" opts in explicitly.
        let want_gpu = device == "gpu";
        let (s, dev) = Self::build_session(&paths.model, want_gpu)?;
        log(format!("Kokoro model loaded on {dev} in {:.2}s", t.elapsed().as_secs_f32()));
        self.session = Some(s);
        self.device = dev;
        Ok(())
    }

    fn unload(&mut self) {
        self.session = None;
        self.g2p_us = None;
        self.g2p_gb = None;
        self.voices.clear();
        self.device = "unloaded".into();
    }

    fn device(&self) -> String {
        self.device.clone()
    }

    fn model_fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    fn synthesize(&mut self, req: &SegmentRequest, cancel: &Cancel) -> Result<AudioBuf> {
        let british = req.voice.starts_with('b');
        self.ensure_lexicon(british)?;
        let ps = self.phonemize(req.runs, req.style.terminal_punct, british);
        let mut out = Vec::new();
        for chunk in chunk_phonemes(&ps, MAX_TOKENS) {
            if cancel.is_cancelled() {
                bail!("cancelled");
            }
            let tokens = g2p::kokoro_tokens(&chunk);
            if tokens.is_empty() {
                continue;
            }
            let audio = match self.infer(&tokens, req.voice, req.style.speed) {
                Ok(a) => a,
                Err(e) if self.device.starts_with("gpu") => {
                    // GPU EP failed at run time: rebuild on CPU and retry once.
                    tracing::warn!("{e:#}; retrying on CPU");
                    let model = self.paths.as_ref().unwrap().model.clone();
                    let (s, dev) = Self::build_session(&model, false)?;
                    self.session = Some(s);
                    self.device = dev;
                    self.infer(&tokens, req.voice, req.style.speed)?
                }
                Err(e) => return Err(e),
            };
            if !out.is_empty() {
                out.extend(std::iter::repeat_n(0.0, (SAMPLE_RATE / 20) as usize));
            }
            out.extend(audio);
        }
        Ok(AudioBuf::new(SAMPLE_RATE, out))
    }
}

pub fn caps() -> Capabilities {
    Capabilities {
        engine: "kokoro".into(),
        cloning: false,
        clone_modes: vec![],
        emotion: Fidelity::Approximate,
        emphasis: Fidelity::Approximate,
        sample_rate: SAMPLE_RATE,
        notes: vec![
            "Kokoro has no emotion or reference-voice conditioning.".into(),
            "Excited / calm / mystery are approximated with speaking rate, level, end punctuation and pause length.".into(),
            "Emphasis is approximated by forcing primary stress on the emphasized words.".into(),
        ],
    }
}

#[allow(dead_code)]
fn _assert_clone_mode_unused(_: CloneMode) {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunking_respects_limit_and_keeps_text() {
        let s = "ðə kˈæt sˈæt. ".repeat(80);
        let chunks = chunk_phonemes(s.trim(), 100);
        assert!(chunks.iter().all(|c| c.chars().count() <= 100));
        let joined: String = chunks.join(" ");
        assert_eq!(joined.split_whitespace().count(), s.split_whitespace().count());
    }
}
