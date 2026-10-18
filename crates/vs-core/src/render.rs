//! The MAKE pipeline: parse → plan segments → synthesize (with cache) → post-process →
//! stitch → normalize → write project folder. Also owns the lazily loaded engines.

use crate::audio::{self, AudioBuf, Piece};
use crate::catalog::Store;
use crate::download::Cancel;
use crate::engine::chatterbox::{Chatterbox, ChatterboxPaths, Variant};
use crate::engine::kokoro::{Kokoro, KokoroPaths};
use crate::engine::python::{self, PyEngine};
use crate::engine::{plan_style, CloneMode, Engine, Run, SegmentRequest, StyleParams};
use crate::markup::{self, Emotion, PauseKind, Span};
use crate::profiles::ProfileStore;
use crate::project::{self, ProjectMeta, RenderStats, SegmentMeta};
use crate::settings::Settings;
use crate::{fsutil, sysmon};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderOptions {
    pub title: String,
    /// "kokoro" | "chatterbox"
    pub engine: String,
    /// Kokoro voice id; ignored when cloning.
    pub voice: String,
    pub emotion_control: bool,
    pub voice_clone: bool,
    pub profile_id: Option<String>,
    pub clone_mode: CloneMode,
    #[serde(default)]
    pub seed: Option<u64>,
    /// Global speaking-rate multiplier (Kokoro).
    #[serde(default = "one")]
    pub speed: f32,
    #[serde(default)]
    pub export_mp3: Option<bool>,
    /// Language for multilingual engines (ISO 639-1, e.g. "tr").
    #[serde(default)]
    pub language: Option<String>,
    /// Synthesize every segment even if cached (benchmarks). Results are still stored.
    #[serde(default)]
    pub bypass_cache: bool,
}

fn one() -> f32 {
    1.0
}

/// A planned segment (before synthesis).
#[derive(Debug, Clone, Serialize)]
pub struct PlannedSegment {
    pub runs: Vec<Run>,
    pub emotion: Emotion,
    pub start: usize,
    pub end: usize,
    /// Pause after this segment (ms), before emotion scaling.
    pub pause_after_ms: f32,
    pub pause_kind: Option<PauseKind>,
}

impl PlannedSegment {
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        cur.push(chars[i]);
        let end = matches!(chars[i], '.' | '!' | '?' | '…' | '。' | '！' | '？');
        if end {
            // absorb trailing punctuation/quotes
            while i + 1 < chars.len() && matches!(chars[i + 1], '.' | '!' | '?' | '…' | '"' | '”' | '’' | ')' | '」' | '』') {
                i += 1;
                cur.push(chars[i]);
            }
            // CJK full-width sentence marks end a sentence even without a following space.
            let cjk = matches!(chars[i], '。' | '！' | '？');
            if i + 1 >= chars.len() || chars[i + 1].is_whitespace() || cjk {
                out.push(std::mem::take(&mut cur));
            }
        }
        i += 1;
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    } else if !cur.is_empty() {
        if let Some(l) = out.last_mut() {
            l.push_str(&cur);
        }
    }
    out
}

/// Plan synthesis segments from parsed spans. Consecutive spans with the same emotion form
/// one block (emphasis stays inline as runs). Blocks are split into sentences and packed
/// up to `max_chars`. Every spoken word lands in exactly one segment, in order.
pub fn plan(parsed: &markup::Parsed, settings: &Settings, max_chars: usize) -> Vec<PlannedSegment> {
    struct Block {
        runs: Vec<Run>,
        emotion: Emotion,
        start: usize,
        end: usize,
        pause: Option<PauseKind>,
    }
    let mut blocks: Vec<Block> = Vec::new();
    for s in &parsed.spans {
        match s {
            Span::Text { text, emotion, emphasis, start, end } => {
                let run = Run { text: text.clone(), emphasis: *emphasis };
                match blocks.last_mut() {
                    Some(b) if b.emotion == *emotion && b.pause.is_none() => {
                        b.runs.push(run);
                        b.end = *end;
                    }
                    _ => blocks.push(Block { runs: vec![run], emotion: *emotion, start: *start, end: *end, pause: None }),
                }
            }
            Span::Pause { kind, .. } => {
                if let Some(b) = blocks.last_mut() {
                    b.pause = Some(*kind);
                }
            }
        }
    }
    let mut out = Vec::new();
    for b in blocks {
        // Split runs into sentences while keeping run boundaries (emphasis) intact.
        let mut sentences: Vec<Vec<Run>> = vec![vec![]];
        for r in &b.runs {
            let parts = split_sentences(&r.text);
            for (k, p) in parts.iter().enumerate() {
                if k > 0 {
                    sentences.push(vec![]);
                }
                sentences.last_mut().unwrap().push(Run { text: p.clone(), emphasis: r.emphasis });
            }
            // A run that ends a sentence (punctuation followed by whitespace) starts a new
            // one for the next run. Without whitespace (`end.*more*`) it is the same word.
            if r.text.ends_with(char::is_whitespace) && r.text.trim_end().ends_with(['.', '!', '?', '…']) {
                sentences.push(vec![]);
            }
        }
        sentences.retain(|s| s.iter().any(|r| !r.text.trim().is_empty()));
        // Pack sentences into segments.
        let mut packed: Vec<Vec<Run>> = Vec::new();
        for s in sentences {
            let len: usize = s.iter().map(|r| r.text.len()).sum();
            match packed.last_mut() {
                Some(p) if p.iter().map(|r| r.text.len()).sum::<usize>() + len <= max_chars => {
                    if let Some(last) = p.last_mut() {
                        if !last.text.ends_with(' ') {
                            last.text.push(' ');
                        }
                    }
                    p.extend(s);
                }
                _ => packed.push(s),
            }
        }
        // Very long sentences: split at commas/spaces.
        let mut final_segments: Vec<Vec<Run>> = Vec::new();
        for p in packed {
            let len: usize = p.iter().map(|r| r.text.len()).sum();
            if len <= max_chars * 3 / 2 {
                final_segments.push(p);
                continue;
            }
            let mut cur: Vec<Run> = vec![];
            let mut cur_len = 0;
            for r in p {
                for word in r.text.split_inclusive(' ') {
                    // Only cut at a word boundary (never inside `un*believ*able`).
                    let at_boundary = cur.last().map(|x: &Run| x.text.ends_with(char::is_whitespace)).unwrap_or(false);
                    if cur_len + word.len() > max_chars && cur_len > 0 && at_boundary {
                        let ends_clause = cur.last().map(|x: &Run| x.text.trim_end().ends_with([',', ';', ':'])).unwrap_or(false);
                        if ends_clause || cur_len + word.len() > max_chars * 3 / 2 {
                            final_segments.push(std::mem::take(&mut cur));
                            cur_len = 0;
                        }
                    }
                    match cur.last_mut() {
                        Some(l) if l.emphasis == r.emphasis => l.text.push_str(word),
                        _ => cur.push(Run { text: word.to_string(), emphasis: r.emphasis }),
                    }
                    cur_len += word.len();
                }
            }
            if !cur.is_empty() {
                final_segments.push(cur);
            }
        }
        let n = final_segments.len();
        for (k, runs) in final_segments.into_iter().enumerate() {
            let last = k + 1 == n;
            let pause_kind = if last { b.pause } else { None };
            let pause_after_ms = match pause_kind {
                Some(PauseKind::Short) => settings.pause_short_ms,
                Some(PauseKind::Long) => settings.pause_long_ms,
                Some(PauseKind::Paragraph) => settings.pause_paragraph_ms,
                None => settings.segment_gap_ms,
            };
            let runs: Vec<Run> = runs.into_iter().filter(|r| !r.text.is_empty()).collect();
            out.push(PlannedSegment { runs, emotion: b.emotion, start: b.start, end: b.end, pause_after_ms, pause_kind });
        }
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    pub stage: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
    pub fraction: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderResult {
    pub project_dir: PathBuf,
    pub final_wav: PathBuf,
    pub final_mp3: Option<PathBuf>,
    pub meta: ProjectMeta,
}

/// Owns engines (loaded lazily on first MAKE, unloaded when idle).
pub struct Studio {
    pub settings: Settings,
    kokoro: Option<Kokoro>,
    chatterbox: Option<Chatterbox>,
    /// The generic Python engine currently resident (one at a time).
    python: Option<PyEngine>,
    pub last_used: Instant,
}

impl Studio {
    pub fn new(settings: Settings) -> Self {
        Self { settings, kokoro: None, chatterbox: None, python: None, last_used: Instant::now() }
    }

    pub fn store(&self) -> Store {
        Store::new(&self.settings.models_dir)
    }

    pub fn profiles(&self) -> ProfileStore {
        ProfileStore::new(self.settings.profiles_dir())
    }

    pub fn loaded_engines(&self) -> Vec<(String, String)> {
        let mut v = vec![];
        if let Some(k) = &self.kokoro {
            if k.is_loaded() {
                v.push(("kokoro".into(), k.device()));
            }
        }
        if let Some(c) = &self.chatterbox {
            if c.is_loaded() {
                v.push((c.id().into(), c.device()));
            }
        }
        if let Some(p) = &self.python {
            if p.is_loaded() {
                v.push((p.id().into(), p.device()));
            }
        }
        v
    }

    /// Free all model memory (RAM/VRAM); Chatterbox's worker process exits.
    pub fn unload_all(&mut self) {
        if let Some(k) = self.kokoro.as_mut() {
            k.unload();
        }
        if let Some(c) = self.chatterbox.as_mut() {
            c.unload();
        }
        if let Some(p) = self.python.as_mut() {
            p.unload();
        }
        self.kokoro = None;
        self.chatterbox = None;
        self.python = None;
    }

    pub fn unload_if_idle(&mut self) -> bool {
        let t = self.settings.idle_unload_secs;
        if t > 0 && self.last_used.elapsed().as_secs() >= t && !self.loaded_engines().is_empty() {
            self.unload_all();
            return true;
        }
        false
    }

    pub fn chatterbox_worker_pid(&self) -> Option<u32> {
        self.chatterbox.as_ref().and_then(|c| c.worker_pid()).or_else(|| self.python.as_ref().and_then(|p| p.worker_pid()))
    }

    fn engine(&mut self, id: &str, log: &mut dyn FnMut(String)) -> Result<&mut dyn Engine> {
        let store = self.store();
        let device = self.settings.device.clone();
        match id {
            "kokoro" => {
                if self.kokoro.is_none() {
                    let paths = KokoroPaths::resolve(&store, &self.settings.kokoro_variant)?;
                    self.kokoro = Some(Kokoro::new(paths));
                }
                let k = self.kokoro.as_mut().unwrap();
                k.load(&device, log)?;
                Ok(k)
            }
            id if Variant::from_engine(id).is_some() => {
                let variant = Variant::from_engine(id).unwrap();
                // Only one Chatterbox checkpoint is kept in memory (VRAM is limited).
                if self.chatterbox.as_ref().is_some_and(|c| c.variant() != variant) {
                    if let Some(mut c) = self.chatterbox.take() {
                        c.unload();
                    }
                }
                // GPU memory is limited: a resident generic GPU engine makes room.
                if let Some(mut p) = self.python.take() {
                    p.unload();
                }
                if self.chatterbox.is_none() {
                    let paths = ChatterboxPaths::resolve(&store, &self.settings.runtimes_dir(), self.settings.cache_dir().join("tmp"), variant)?;
                    self.chatterbox = Some(Chatterbox::new(paths));
                }
                let c = self.chatterbox.as_mut().unwrap();
                c.load(&device, log)?;
                Ok(c)
            }
            id if python::spec(id).is_some() => {
                if self.python.as_ref().is_some_and(|p| p.id() != id) {
                    if let Some(mut p) = self.python.take() {
                        p.unload();
                    }
                }
                if python::spec(id).is_some_and(|s| s.gpu) {
                    if let Some(mut c) = self.chatterbox.take() {
                        c.unload();
                    }
                }
                if self.python.is_none() {
                    self.python = Some(PyEngine::resolve(&store, &self.settings.runtimes_dir(), self.settings.cache_dir().join("tmp"), id)?);
                }
                let p = self.python.as_mut().unwrap();
                p.load(&device, log)?;
                Ok(p)
            }
            other => bail!("unknown engine '{other}'"),
        }
    }

    pub fn peak_vram(&self) -> Option<u64> {
        self.chatterbox.as_ref().and_then(|c| c.last_peak_vram).or_else(|| self.python.as_ref().and_then(|p| p.last_peak_vram))
    }

    /// Render a script into a new project folder.
    pub fn render(
        &mut self,
        script: &str,
        opts: &RenderOptions,
        cancel: &Cancel,
        progress: &mut dyn FnMut(ProgressEvent),
    ) -> Result<RenderResult> {
        let t0 = Instant::now();
        self.last_used = Instant::now();
        let mon = sysmon::PeakMonitor::start(self.chatterbox_worker_pid());
        let parsed = markup::parse(script);
        let engine_id = opts.engine.as_str();
        let is_chatterbox = Variant::from_engine(engine_id).is_some();
        let py_spec = python::spec(engine_id);
        let max_chars = match &py_spec {
            Some(s) => s.max_chars,
            None if is_chatterbox => 220,
            None => 320,
        };
        let can_clone = is_chatterbox || py_spec.as_ref().is_some_and(|s| s.cloning);
        let segments = plan(&parsed, &self.settings, max_chars);
        if segments.is_empty() {
            bail!("The script has no speakable text.");
        }
        // Safety net: never lose, duplicate or reorder words.
        // Compared without whitespace so languages without spaces (Chinese, Japanese) that
        // are split at sentence punctuation still verify; any lost, duplicated or reordered
        // character fails the check.
        let planned: String = segments.iter().flat_map(|s| s.text().chars().collect::<Vec<_>>()).filter(|c| !c.is_whitespace()).collect();
        let spoken: String = markup::spoken_words(&parsed).concat();
        if planned != spoken {
            bail!("internal error: segment plan does not preserve the script's words");
        }

        // Voice / reference.
        let (reference, profile) = if opts.voice_clone {
            if !can_clone {
                bail!("The selected engine ({engine_id}) does not support voice cloning.");
            }
            let pid = opts.profile_id.as_deref().context("Voice Clone is on but no voice profile is selected.")?;
            let ps = self.profiles();
            ps.verify(pid)?;
            (Some(ps.reference_path(pid)), Some(ps.get(pid)?))
        } else {
            (None, None)
        };
        let ref_sha = profile.as_ref().map(|p| p.sha256.clone()).unwrap_or_default();
        let seed = opts.seed.unwrap_or(self.settings.chatterbox_seed);
        let language: Option<String> = if engine_id == "chatterbox-mtl" {
            let l = opts.language.clone().unwrap_or_else(|| "en".into()).to_lowercase();
            if !crate::engine::chatterbox::MTL_LANGUAGES.iter().any(|(c, _)| *c == l) {
                bail!("Unsupported language '{l}' for Chatterbox Multilingual.");
            }
            Some(l)
        } else if let Some(sp) = py_spec.as_ref().filter(|s| !s.languages.is_empty()) {
            let l = opts.language.clone().unwrap_or_else(|| sp.languages[0].clone()).to_lowercase();
            if !sp.languages.contains(&l) {
                bail!("Unsupported language '{l}' for {}.", sp.name);
            }
            Some(l)
        } else {
            None
        };
        let base_voice = if engine_id == "kokoro" {
            opts.voice.clone()
        } else if profile.is_some() {
            "clone".to_string()
        } else if let Some(sp) = py_spec.as_ref().filter(|s| !s.voices.is_empty()) {
            // Use the requested voice when it belongs to this engine, else its first voice.
            if sp.voices.iter().any(|v| v.id == opts.voice) { opts.voice.clone() } else { sp.voices[0].id.clone() }
        } else {
            "builtin".to_string()
        };
        // The language is part of the voice key so cached segments never mix languages.
        let voice = match &language {
            Some(l) => format!("{base_voice}@{l}"),
            None => base_voice,
        };

        let cache_dir = self.settings.cache_dir().join("segments");
        let id = uuid::Uuid::new_v4().simple().to_string();
        let title = if opts.title.trim().is_empty() { auto_title(script) } else { opts.title.trim().to_string() };
        let proj_dir = project::new_project_dir(&self.settings.projects_dir(), &title, &id);
        let settings = self.settings.clone();
        progress(ProgressEvent { stage: "load".into(), current: 0, total: segments.len(), message: format!("Loading {engine_id}…"), fraction: 0.0 });
        let mut logs: Vec<String> = Vec::new();
        let engine = {
            let n_segments = segments.len();
            let mut log = |m: String| {
                tracing::info!("{m}");
                progress(ProgressEvent { stage: "load".into(), current: 0, total: n_segments, message: m.clone(), fraction: 0.0 });
                logs.push(m);
            };
            self.engine(engine_id, &mut log)?
        };
        let fingerprint = engine.model_fingerprint();
        let device = engine.device();
        let sr = engine.capabilities().sample_rate;
        let seg_dir = proj_dir.join("segments");
        std::fs::create_dir_all(&seg_dir)?;

        let mut pieces = Vec::new();
        let mut metas = Vec::new();
        let mut cached_n = 0;
        let mut gap_before = 0.0f32;
        let total = segments.len();
        for (i, seg) in segments.iter().enumerate() {
            if cancel.is_cancelled() {
                let _ = std::fs::remove_dir_all(&proj_dir);
                bail!("cancelled");
            }
            let mut style: StyleParams = match &py_spec {
                Some(sp) => python::plan_style(sp, seg.emotion, opts.emotion_control),
                None => plan_style(engine_id, seg.emotion, opts.emotion_control, opts.clone_mode),
            };
            style.speed *= opts.speed.clamp(0.5, 2.0);
            let key = cache_key(&fingerprint, &voice, &ref_sha, seed, &seg.runs, &style, sr);
            let cache_path = cache_dir.join(&key[..2]).join(format!("{key}.wav"));
            let text = seg.text();
            progress(ProgressEvent {
                stage: "synth".into(),
                current: i + 1,
                total,
                message: format!("Segment {}/{}: {}", i + 1, total, ellipsize(&text, 60)),
                fraction: i as f64 / total as f64,
            });
            let cached_audio = if opts.bypass_cache { None } else { audio::read_wav(&cache_path).ok().filter(|b| !b.samples.is_empty()) };
            let (mut buf, cached) = match cached_audio {
                Some(b) => (b, true),
                _ => {
                    let req = SegmentRequest {
                        runs: &seg.runs,
                        style: &style,
                        voice: &voice,
                        reference: reference.as_deref(),
                        seed,
                        language: language.as_deref(),
                    };
                    let b = match engine.synthesize(&req, cancel) {
                        Ok(b) => b,
                        Err(e) => {
                            let _ = std::fs::remove_dir_all(&proj_dir);
                            return Err(e);
                        }
                    };
                    if b.samples.is_empty() {
                        bail!("engine returned no audio for segment {}", i + 1);
                    }
                    let _ = audio::write_wav_f32(&cache_path, &b);
                    (b, false)
                }
            };
            if cached {
                cached_n += 1;
            }
            // Per-segment cleanup: DC, trim engine padding, style gain, fades.
            audio::remove_dc(&mut buf);
            audio::trim_silence(&mut buf, -50.0, 30.0);
            if buf.samples.is_empty() {
                buf = AudioBuf::silence(sr, 50.0);
            }
            audio::apply_gain_db(&mut buf, style.gain_db);
            let seg_file = format!("segments/{:03}.wav", i + 1);
            audio::write_wav(&proj_dir.join(&seg_file), &buf)?;
            let pause_after = seg.pause_after_ms * if seg.pause_kind.is_some() { 1.0 } else { style.pause_scale };
            metas.push(SegmentMeta {
                index: i + 1,
                text,
                emotion: seg.emotion,
                emphasis_words: seg.runs.iter().filter(|r| r.emphasis).map(|r| r.text.split_whitespace().count()).sum(),
                source_start: seg.start,
                source_end: seg.end,
                file: seg_file,
                duration_secs: buf.duration_secs(),
                offset_secs: 0.0,
                pause_after_ms: pause_after,
                cache_key: key,
                cached,
                emotion_fidelity: style.emotion_fidelity,
                emphasis_fidelity: if seg.runs.iter().any(|r| r.emphasis) { style.emphasis_fidelity } else { crate::engine::Fidelity::None },
                params: serde_json::to_value(&style)?,
            });
            pieces.push(Piece { audio: buf, gap_before_ms: gap_before });
            gap_before = pause_after;
        }

        progress(ProgressEvent { stage: "mix".into(), current: total, total, message: "Stitching and normalizing…".into(), fraction: 0.97 });
        // Offsets for the segment inspector.
        let mut t = 0.0f64;
        for (m, p) in metas.iter_mut().zip(pieces.iter()) {
            t += p.gap_before_ms as f64 / 1000.0;
            m.offset_secs = t;
            t += p.audio.duration_secs();
        }
        let lead = AudioBuf::silence(sr, 150.0);
        let mut all = vec![Piece { audio: lead, gap_before_ms: 0.0 }];
        all.extend(pieces);
        all.push(Piece { audio: AudioBuf::silence(sr, 250.0), gap_before_ms: 0.0 });
        for m in metas.iter_mut() {
            m.offset_secs += 0.15;
        }
        let mut mix = audio::stitch(all, sr, settings.fade_ms, 10.0);
        let (before, _gain) = audio::normalize_loudness(&mut mix, settings.loudness_target_lufs, settings.peak_ceiling_db);
        let after = audio::integrated_lufs(&mix);
        let final_wav = proj_dir.join("final.wav");
        audio::write_wav(&final_wav, &mix)?;

        let want_mp3 = opts.export_mp3.unwrap_or(settings.export_mp3);
        let final_mp3 = if want_mp3 {
            match crate::export::wav_to_mp3(&final_wav, &proj_dir.join("final.mp3"), settings.ffmpeg_path.as_deref()) {
                Ok(p) => Some(p),
                Err(e) => {
                    logs.push(format!("MP3 export skipped: {e:#}"));
                    None
                }
            }
        } else {
            None
        };

        // Project files: script, reference copy, metadata.
        fsutil::atomic_write(&proj_dir.join("script.txt"), script.as_bytes())?;
        if let Some(r) = &reference {
            let _ = std::fs::copy(r, proj_dir.join("reference.wav"));
        }
        let peaks = mon.stop();
        let audio_secs = mix.duration_secs();
        let render_secs = t0.elapsed().as_secs_f64();
        let meta = ProjectMeta {
            schema_version: project::PROJECT_SCHEMA,
            id,
            title,
            created_at: chrono::Utc::now(),
            engine: engine_id.into(),
            model: fingerprint,
            voice: voice.clone(),
            profile_id: profile.as_ref().map(|p| p.id.clone()),
            profile_name: profile.as_ref().map(|p| p.name.clone()),
            clone_mode: opts.voice_clone.then_some(opts.clone_mode),
            emotion_control: opts.emotion_control,
            settings: serde_json::json!({
                "options": opts,
                "pause_short_ms": settings.pause_short_ms,
                "pause_long_ms": settings.pause_long_ms,
                "pause_paragraph_ms": settings.pause_paragraph_ms,
                "segment_gap_ms": settings.segment_gap_ms,
                "loudness_target_lufs": settings.loudness_target_lufs,
                "peak_ceiling_db": settings.peak_ceiling_db,
                "seed": seed,
            }),
            segments: metas,
            final_wav: "final.wav".into(),
            final_mp3: final_mp3.as_ref().map(|_| "final.mp3".into()),
            duration_secs: audio_secs,
            stats: RenderStats {
                render_secs,
                audio_secs,
                rtf: render_secs / audio_secs.max(0.001),
                segments_total: total,
                segments_cached: cached_n,
                device,
                loudness_before_lufs: before,
                loudness_after_lufs: after,
                peak_rss_mb: peaks.0,
                peak_vram_mb: self.peak_vram().map(|b| b as f64 / 1e6).or(peaks.1),
            },
            warnings: parsed.warnings.clone(),
            app_version: env!("CARGO_PKG_VERSION").into(),
        };
        project::save(&proj_dir, &meta)?;
        let _ = std::fs::write(proj_dir.join("render.log"), logs.join("\n"));
        self.last_used = Instant::now();
        progress(ProgressEvent { stage: "done".into(), current: total, total, message: "Done".into(), fraction: 1.0 });
        Ok(RenderResult { project_dir: proj_dir, final_wav, final_mp3, meta })
    }
}

pub fn cache_key(fingerprint: &str, voice: &str, ref_sha: &str, seed: u64, runs: &[Run], style: &StyleParams, sr: u32) -> String {
    let v = serde_json::json!({
        "v": CACHE_VERSION, "model": fingerprint, "voice": voice, "ref": ref_sha, "seed": seed,
        "runs": runs, "style": style, "sr": sr,
    });
    fsutil::sha256_bytes(v.to_string().as_bytes())
}

pub fn auto_title(script: &str) -> String {
    let plain = markup::plain_text(script);
    let words: Vec<&str> = plain.split_whitespace().take(7).collect();
    let t = words.join(" ");
    let t = t.trim_end_matches(|c: char| !c.is_alphanumeric()).to_string();
    if t.is_empty() {
        "Untitled".into()
    } else {
        t
    }
}

fn ellipsize(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Remove least-recently-written cache files until the cache is below `max_bytes`.
pub fn prune_cache(cache_dir: &Path, max_bytes: u64) -> Result<u64> {
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    fn walk(d: &Path, out: &mut Vec<(PathBuf, u64, std::time::SystemTime)>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if let Ok(m) = e.metadata() {
                out.push((p, m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
            }
        }
    }
    walk(cache_dir, &mut files);
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.2);
    let mut freed = 0;
    for (p, len, _) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
            freed += len;
        }
    }
    Ok(freed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(segs: &[PlannedSegment]) -> Vec<String> {
        segs.iter().flat_map(|s| s.text().split_whitespace().map(String::from).collect::<Vec<_>>()).collect()
    }

    #[test]
    fn plan_preserves_words_and_splits() {
        let s = Settings::default();
        let script = r#"Welcome to the channel. Today we test *everything*. "This is huge!" -- [Breathe in. Breathe out.] --- {Something is watching.}

New paragraph here."#;
        let p = markup::parse(script);
        let segs = plan(&p, &s, 40);
        assert_eq!(words(&segs), markup::spoken_words(&p));
        assert!(segs.iter().any(|g| g.emotion == Emotion::Excited));
        assert!(segs.iter().any(|g| g.pause_kind == Some(PauseKind::Long)));
        assert!(segs.iter().any(|g| g.pause_kind == Some(PauseKind::Paragraph)));
        assert!(segs.iter().any(|g| g.runs.iter().any(|r| r.emphasis)));
    }

    #[test]
    fn plan_splits_very_long_sentence() {
        let s = Settings::default();
        let long = "word, ".repeat(200);
        let p = markup::parse(&long);
        let segs = plan(&p, &s, 100);
        assert!(segs.len() > 5);
        assert!(segs.iter().all(|g| g.text().len() <= 160));
        assert_eq!(words(&segs), markup::spoken_words(&p));
    }

    #[test]
    fn cache_key_changes_with_inputs() {
        let runs = vec![Run { text: "hi".into(), emphasis: false }];
        let st = plan_style("kokoro", Emotion::Neutral, true, CloneMode::VoiceOnly);
        let a = cache_key("m1", "af", "", 1, &runs, &st, 24000);
        assert_eq!(a, cache_key("m1", "af", "", 1, &runs, &st, 24000));
        assert_ne!(a, cache_key("m2", "af", "", 1, &runs, &st, 24000));
        assert_ne!(a, cache_key("m1", "am", "", 1, &runs, &st, 24000));
        let st2 = plan_style("kokoro", Emotion::Excited, true, CloneMode::VoiceOnly);
        assert_ne!(a, cache_key("m1", "af", "", 1, &runs, &st2, 24000));
        let runs2 = vec![Run { text: "hi!".into(), emphasis: false }];
        assert_ne!(a, cache_key("m1", "af", "", 1, &runs2, &st, 24000));
    }

    #[test]
    fn titles() {
        assert_eq!(auto_title(r#""Wow!" this is [a test] of the title system ok"#), "Wow! this is a test of the");
        assert_eq!(auto_title("--- --"), "Untitled");
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;

    fn words(segs: &[PlannedSegment]) -> Vec<String> {
        segs.iter().flat_map(|s| s.text().split_whitespace().map(String::from).collect::<Vec<_>>()).collect()
    }

    #[test]
    fn plan_preserves_words_fuzz() {
        let s = Settings::default();
        let pieces = [
            "Hello", "world.", "Wait!", "Really?", "ok", "end...", "\"wow\"", "[calm.]", "{dark}", "*hit*", "--", "---",
            "\n\n", "\n", "un*believ*able", "a.b", "3.5", "$4", "Mr.", "e.g.", "“quoted.”", "(aside.)", "x!?", "*", "\"", "[", "}",
            "Done.\"", "yes.)", "  ", "word,", "…", "Hi.Hi", "*emph.*", "[a. b.]", "\"A! B?\"",
        ];
        let mut seed: u64 = 0x2545F4914F6CDD1D;
        for case in 0..20000 {
            let mut script = String::new();
            let n = 1 + (case % 25);
            for _ in 0..n {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                script.push_str(pieces[(seed % pieces.len() as u64) as usize]);
                if seed & 0x100 != 0 {
                    script.push(' ');
                }
            }
            let p = markup::parse(&script);
            for max in [20usize, 60, 220] {
                let segs = plan(&p, &s, max);
                assert_eq!(words(&segs), markup::spoken_words(&p), "max={max} script={script:?}");
            }
        }
    }
}
