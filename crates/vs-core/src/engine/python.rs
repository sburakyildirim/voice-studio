//! Generic engines that run inside their own isolated Python environment through a small
//! adapter (worker/adapters/<name>.py). Each engine is described in catalog/engines.json:
//! pinned pip requirements, pinned Hugging Face weights, voices, languages and honest
//! capability labels. Installing one creates `runtimes/<id>` (venv via uv) and downloads the
//! weights into `models/engine-<id>` with the normal resumable, verified downloader.

use super::chatterbox::{Worker, WorkerLaunch};
use super::{runs_text, Capabilities, CloneMode, Engine, Fidelity, SegmentRequest, StyleParams};
use crate::audio::{self, AudioBuf};
use crate::catalog::{self, FileSpec, Package, Store};
use crate::download::Cancel;
use crate::fsutil;
use crate::markup::Emotion;
use crate::pyenv::PyRuntime;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const ENGINES_JSON: &str = include_str!("../../../../catalog/engines.json");
const HOST_PY: &str = include_str!("../../../../worker/vs_worker.py");
/// Adapter sources embedded in the binary (name, code).
const ADAPTERS: &[(&str, &str)] = &[
    ("piper", include_str!("../../../../worker/adapters/piper.py")),
    ("xtts", include_str!("../../../../worker/adapters/xtts.py")),
    ("f5", include_str!("../../../../worker/adapters/f5.py")),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSpec {
    pub python: String,
    /// PyTorch version to install first (CUDA/CPU/MPS build chosen per machine), if needed.
    #[serde(default)]
    pub torch: Option<String>,
    #[serde(default)]
    pub torchaudio: bool,
    pub packages: Vec<String>,
    /// Extra arguments for `uv pip install` (e.g. "--no-deps").
    #[serde(default)]
    pub pip_args: Vec<String>,
    /// Python snippet that must import cleanly for the runtime to count as installed.
    pub probe: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightsSpec {
    pub repo: String,
    pub revision: String,
    /// Folders whose files are downloaded (non-recursive).
    #[serde(default)]
    pub dirs: Vec<String>,
    /// Individual files.
    #[serde(default)]
    pub files: Vec<String>,
    /// Sub-folder of the engine's model folder these files go into ("" = root).
    #[serde(default)]
    pub subdir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSpec {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSpec {
    pub id: String,
    pub catalog_id: String,
    pub name: String,
    pub adapter: String,
    pub license: String,
    pub commercial: String,
    pub platforms: Vec<String>,
    pub runtime: RuntimeSpec,
    pub weights: WeightsSpec,
    /// Additional repositories (vocoders, ASR for reference transcripts, …).
    #[serde(default)]
    pub extra_weights: Vec<WeightsSpec>,
    #[serde(default)]
    pub voices: Vec<VoiceSpec>,
    /// Languages selectable per render (empty: the language follows the voice).
    #[serde(default)]
    pub languages: Vec<String>,
    pub cloning: bool,
    /// "approximate" | "genuine" | "none"
    pub emotion: String,
    pub emphasis: String,
    pub max_chars: usize,
    /// Uses PyTorch on the GPU (unloads other GPU engines first).
    #[serde(default)]
    pub gpu: bool,
    #[serde(default)]
    pub approx_download_mb: u64,
    #[serde(default)]
    pub notes: Vec<String>,
    /// Shown instead of an install button when the engine cannot run on this platform.
    #[serde(default)]
    pub unsupported_reason: Option<String>,
}

impl EngineSpec {
    pub fn supported_here(&self) -> bool {
        self.unsupported_reason.is_none() && self.platforms.iter().any(|p| *p == catalog::platform_id())
    }
    pub fn package_id(&self) -> String {
        format!("engine-{}", self.id)
    }
    pub fn runtime(&self, runtimes_dir: &Path) -> PyRuntime {
        PyRuntime { dir: runtimes_dir.join(fsutil::sanitize_file_name(&self.id)) }
    }
    pub fn emotion_fidelity(&self) -> Fidelity {
        match self.emotion.as_str() {
            "genuine" => Fidelity::Genuine,
            "approximate" => Fidelity::Approximate,
            _ => Fidelity::Unsupported,
        }
    }
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            engine: self.id.clone(),
            cloning: self.cloning,
            clone_modes: if self.cloning { vec![CloneMode::VoiceOnly] } else { vec![] },
            emotion: self.emotion_fidelity(),
            emphasis: if self.emphasis == "approximate" { Fidelity::Approximate } else { Fidelity::Unsupported },
            sample_rate: 24000,
            notes: self.notes.clone(),
        }
    }
}

#[derive(Deserialize)]
struct EnginesFile {
    engines: Vec<EngineSpec>,
}

pub fn specs() -> Vec<EngineSpec> {
    serde_json::from_str::<EnginesFile>(ENGINES_JSON).expect("embedded engines.json is valid").engines
}

pub fn spec(id: &str) -> Option<EngineSpec> {
    specs().into_iter().find(|s| s.id == id)
}

/// Markup → controls for generic engines. Engines without emotion inputs get the same
/// labelled approximations as Kokoro (rate, level, pause); "genuine" engines receive the
/// emotion name and apply it themselves (e.g. a text description).
pub fn plan_style(spec: &EngineSpec, emotion: Emotion, emotion_control: bool) -> StyleParams {
    let emo = if emotion_control { emotion } else { Emotion::Neutral };
    let (speed, gain, pause) = match (spec.emotion.as_str(), emo) {
        (_, Emotion::Neutral) => (1.0, 0.0, 1.0),
        ("genuine", Emotion::Excited) => (1.0, 0.0, 0.85),
        ("genuine", Emotion::Calm) => (1.0, 0.0, 1.15),
        ("genuine", Emotion::Mystery) => (1.0, 0.0, 1.5),
        ("approximate", Emotion::Excited) => (1.1, 1.5, 0.8),
        ("approximate", Emotion::Calm) => (0.92, -1.5, 1.15),
        ("approximate", Emotion::Mystery) => (0.86, -3.0, 1.5),
        // No emotion support: only pauses follow markup.
        (_, Emotion::Excited) => (1.0, 0.0, 0.85),
        (_, Emotion::Calm) => (1.0, 0.0, 1.15),
        (_, Emotion::Mystery) => (1.0, 0.0, 1.5),
    };
    StyleParams {
        emotion: emo,
        speed,
        gain_db: gain,
        exaggeration: match emo {
            Emotion::Excited => 0.9,
            Emotion::Calm => 0.3,
            Emotion::Mystery => 0.35,
            Emotion::Neutral => 0.5,
        },
        cfg_weight: 0.5,
        temperature: 0.8,
        pause_scale: pause,
        terminal_punct: None,
        emotion_fidelity: if emo == Emotion::Neutral {
            Fidelity::None
        } else if spec.emotion == "none" {
            Fidelity::Approximate // pauses only
        } else {
            spec.emotion_fidelity()
        },
        emphasis_fidelity: if spec.emphasis == "approximate" { Fidelity::Approximate } else { Fidelity::Unsupported },
    }
}

// ------------------------------------------------------------------ installation

/// Build the weights package by listing the pinned Hugging Face revision. LFS files carry
/// the publisher's SHA-256; small text files are pinned by revision and size.
pub fn weights_package(spec: &EngineSpec) -> Result<Package> {
    let mut files = Vec::new();
    for w in std::iter::once(&spec.weights).chain(spec.extra_weights.iter()) {
        files.extend(weights_files(w)?);
    }
    Ok(Package {
        id: spec.package_id(),
        title: format!("{} weights", spec.name),
        kind: "model".into(),
        engine: spec.id.clone(),
        note: None,
        license: spec.license.clone(),
        platforms: spec.platforms.clone(),
        files,
    })
}

fn weights_files(w: &WeightsSpec) -> Result<Vec<FileSpec>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(concat!("VoiceStudio/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let list = |dir: &str| -> Result<Vec<Value>> {
        let url = format!("https://huggingface.co/api/models/{}/tree/{}/{}", w.repo, w.revision, dir);
        let body = agent.get(&url).call().with_context(|| format!("list {url}"))?.into_body().read_to_string()?;
        let v: Value = serde_json::from_str(&body)?;
        v.as_array().cloned().ok_or_else(|| anyhow!("unexpected listing for {dir}: {body}"))
    };
    let to_spec = |f: &Value| -> FileSpec {
        let path = f["path"].as_str().unwrap_or_default().to_string();
        let size = f["lfs"]["size"].as_u64().or_else(|| f["size"].as_u64()).unwrap_or(0);
        let sha = f["lfs"]["oid"].as_str().unwrap_or_default().to_string();
        FileSpec {
            url: format!("https://huggingface.co/{}/resolve/{}/{}", w.repo, w.revision, path),
            sha256: sha,
            size,
            dest: if w.subdir.is_empty() { path } else { format!("{}/{}", w.subdir, path) },
            extract: vec![],
        }
    };
    let mut files = Vec::new();
    for d in &w.dirs {
        let items: Vec<FileSpec> = list(d)?.iter().filter(|f| f["type"] == "file").map(to_spec).collect();
        if items.is_empty() {
            bail!("no files in {}/{}", w.repo, d);
        }
        files.extend(items);
    }
    for path in &w.files {
        let parent = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        let item = list(parent)?.into_iter().find(|f| f["path"] == json!(path)).ok_or_else(|| anyhow!("{path} not found in {}", w.repo))?;
        files.push(to_spec(&item));
    }
    Ok(files)
}

pub struct InstallState {
    pub runtime: bool,
    pub weights: bool,
}

pub fn install_state(spec: &EngineSpec, store: &Store, runtimes_dir: &Path) -> InstallState {
    InstallState { runtime: spec.runtime(runtimes_dir).is_installed(), weights: store.is_installed(&spec.package_id()) }
}

/// Install runtime (uv venv + pinned packages) and weights. Idempotent: finished parts are
/// skipped, partial downloads resume.
pub fn install(
    spec: &EngineSpec,
    store: &Store,
    runtimes_dir: &Path,
    uv: &Path,
    cancel: &Cancel,
    log: &mut dyn FnMut(String),
    progress: &mut dyn FnMut(&str, crate::download::Progress),
) -> Result<()> {
    if !spec.supported_here() {
        bail!("{} is not supported on {}: {}", spec.name, catalog::platform_id(), spec.unsupported_reason.clone().unwrap_or_default());
    }
    let rt = spec.runtime(runtimes_dir);
    if !rt.is_installed() {
        log(format!("Installing the {} Python environment…", spec.name));
        rt.install_spec(uv, &spec.runtime, &spec.id, cancel, log)?;
    }
    if !store.is_installed(&spec.package_id()) {
        log(format!("Listing {} weights ({})…", spec.name, spec.weights.repo));
        let pkg = weights_package(spec)?;
        log(format!("Downloading {} files ({:.0} MB)…", pkg.files.len(), pkg.download_size() as f64 / 1e6));
        store.install(&pkg, cancel, |f, p| progress(f, p))?;
    }
    log(format!("{} is ready.", spec.name));
    Ok(())
}

pub fn remove(spec: &EngineSpec, store: &Store, runtimes_dir: &Path) -> Result<()> {
    store.remove(&spec.package_id())?;
    spec.runtime(runtimes_dir).remove()
}

// ------------------------------------------------------------------ engine

pub struct PyEngine {
    spec: EngineSpec,
    id: &'static str,
    runtime: PathBuf,
    weights: PathBuf,
    tmp: PathBuf,
    log_file: PathBuf,
    worker: Option<Worker>,
    device: String,
    fingerprint: String,
    pub last_peak_vram: Option<u64>,
}

impl PyEngine {
    pub fn resolve(store: &Store, runtimes_dir: &Path, tmp: PathBuf, id: &str) -> Result<Self> {
        let spec = spec(id).ok_or_else(|| anyhow!("unknown engine '{id}'"))?;
        if !spec.supported_here() {
            bail!("{} is not supported on this computer. {}", spec.name, spec.unsupported_reason.clone().unwrap_or_default());
        }
        let st = install_state(&spec, store, runtimes_dir);
        if !st.runtime || !st.weights {
            bail!("MISSING_PACKAGES:{}", spec.package_id());
        }
        let sha = store.manifest(&spec.package_id()).map(|m| m.sources.iter().map(|s| s.get(..8).unwrap_or("")).collect::<String>()).unwrap_or_default();
        // Adapter code is part of the fingerprint: changing how a model is driven must
        // invalidate cached segments.
        let adapter_code = ADAPTERS.iter().find(|(n, _)| *n == spec.adapter).map(|(_, c)| *c).unwrap_or("");
        let fingerprint = format!(
            "{}:{}:{}:{}",
            spec.id,
            spec.weights.revision,
            fsutil::sha256_bytes(sha.as_bytes()),
            &fsutil::sha256_bytes(adapter_code.as_bytes())[..12]
        );
        Ok(Self {
            id: Box::leak(spec.id.clone().into_boxed_str()),
            runtime: spec.runtime(runtimes_dir).dir,
            weights: store.dir(&spec.package_id()),
            log_file: crate::settings::data_root().join("logs").join(format!("{}-worker.log", spec.id)),
            spec,
            tmp,
            worker: None,
            device: "unloaded".into(),
            fingerprint,
            last_peak_vram: None,
        })
    }

    pub fn spec(&self) -> &EngineSpec {
        &self.spec
    }

    pub fn worker_pid(&self) -> Option<u32> {
        self.worker.as_ref().map(|w| w.pid())
    }

    /// Write the host script and adapters next to the venv (refreshed when they change).
    fn write_scripts(&self) -> Result<PathBuf> {
        let dir = self.runtime.join("vs_worker");
        let host = dir.join("vs_worker.py");
        let write_if_changed = |p: &Path, code: &str| -> Result<()> {
            if std::fs::read_to_string(p).unwrap_or_default() != code {
                fsutil::atomic_write(p, code.as_bytes())?;
            }
            Ok(())
        };
        write_if_changed(&host, HOST_PY)?;
        write_if_changed(&dir.join("adapters").join("__init__.py"), "")?;
        for (name, code) in ADAPTERS {
            write_if_changed(&dir.join("adapters").join(format!("{name}.py")), code)?;
        }
        Ok(host)
    }
}

impl Engine for PyEngine {
    fn id(&self) -> &'static str {
        self.id
    }

    fn capabilities(&self) -> Capabilities {
        self.spec.capabilities()
    }

    fn is_loaded(&self) -> bool {
        self.worker.is_some()
    }

    fn load(&mut self, device: &str, log: &mut dyn FnMut(String)) -> Result<()> {
        if self.worker.is_some() {
            return Ok(());
        }
        log(format!("Starting isolated {} worker…", self.spec.name));
        let script = self.write_scripts()?;
        let rt = PyRuntime { dir: self.runtime.clone() };
        let mut w = Worker::spawn_with(&WorkerLaunch {
            python: rt.python(),
            script,
            cwd: self.runtime.clone(),
            log_file: Some(self.log_file.clone()),
            label: self.spec.name.clone(),
        })?;
        let r = w.call_with_status(
            json!({"cmd": "load", "adapter": self.spec.adapter, "weights_dir": self.weights, "device": device}),
            Duration::from_secs(900),
            &Cancel::new(),
            log,
        )?;
        self.device = match r["device"].as_str().unwrap_or("cpu") {
            "cuda" => format!("gpu:cuda ({})", r["gpu_name"].as_str().unwrap_or("NVIDIA")),
            "mps" => "gpu:mps".into(),
            d => d.to_string(),
        };
        log(format!("{} ready on {} in {:.1}s", self.spec.name, self.device, r["load_secs"].as_f64().unwrap_or(0.0)));
        self.worker = Some(w);
        Ok(())
    }

    fn unload(&mut self) {
        self.worker = None;
        self.device = "unloaded".into();
    }

    fn device(&self) -> String {
        self.device.clone()
    }

    fn model_fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    fn synthesize(&mut self, req: &SegmentRequest, cancel: &Cancel) -> Result<AudioBuf> {
        let w = self.worker.as_mut().context("engine not loaded")?;
        std::fs::create_dir_all(&self.tmp)?;
        let out = self.tmp.join(format!("{}-{}.wav", self.spec.id, uuid::Uuid::new_v4().simple()));
        // The voice key may carry "@lang"; the adapter only needs the voice id.
        let voice = req.voice.split('@').next().unwrap_or(req.voice);
        let r = w.call(
            json!({
                "cmd": "synth",
                "text": runs_text(req.runs, None),
                "voice": voice,
                "ref": req.reference,
                "language": req.language,
                "speed": req.style.speed,
                "emotion": req.style.emotion.label(),
                "exaggeration": req.style.exaggeration,
                "seed": req.seed,
                "out": out,
            }),
            Duration::from_secs(900),
            cancel,
        );
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                if let Some(mut w) = self.worker.take() {
                    w.kill();
                }
                self.device = "unloaded".into();
                let _ = std::fs::remove_file(&out);
                return Err(e);
            }
        };
        self.last_peak_vram = r["peak_vram"].as_u64();
        let buf = audio::read_wav(&out);
        let _ = std::fs::remove_file(&out);
        // Normalise every engine to 24 kHz so stitching and the cache stay uniform.
        buf.map(|b| if b.sample_rate == 24000 { b } else { audio::resample(&b, 24000) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn engines_json_is_valid_and_adapters_exist() {
        let s = specs();
        assert!(!s.is_empty());
        for e in &s {
            assert!(ADAPTERS.iter().any(|(n, _)| *n == e.adapter), "adapter {} not embedded", e.adapter);
            assert!(!e.weights.revision.is_empty() && e.weights.revision.len() >= 7, "{} weights must be pinned", e.id);
            assert!(crate::catalog::models_catalog()["models"].as_array().unwrap().iter().any(|m| m["id"] == json!(e.catalog_id)), "{} missing from models.json", e.catalog_id);
        }
    }
    #[test]
    fn styles_are_labelled() {
        let p = spec("piper").unwrap();
        assert_eq!(plan_style(&p, Emotion::Excited, true).emotion_fidelity, Fidelity::Approximate);
        assert_eq!(plan_style(&p, Emotion::Neutral, true).emotion_fidelity, Fidelity::None);
    }
}
