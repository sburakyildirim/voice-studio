//! vscli — headless Voice Studio for scripting, tests and benchmarks.
//!
//! Uses exactly the same core as the desktop app (same model folder, cache and projects).

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::time::Instant;
use vs_core::catalog::{self, Store};
use vs_core::download::Cancel;
use vs_core::engine::CloneMode;
use vs_core::profiles::{self, ProfileStore};
use vs_core::render::{RenderOptions, Studio};
use vs_core::settings::Settings;
use vs_core::{pyenv, queue, sysmon};

const HELP: &str = "vscli — Voice Studio command line

USAGE:
  vscli packages                         List model/runtime packages and install state
  vscli install <package-id>...          Download + verify packages (resumable)
  vscli adopt <package-id> [dir]         Verify files already on disk and register them
  vscli verify <package-id>              Re-hash an installed package
  vscli remove <package-id>              Delete an installed package
  vscli runtime <install|probe|remove>   Manage the isolated Chatterbox Python runtime
  vscli engine <list|install ID|remove ID>  Extra engines (Piper, XTTS, F5, …) in isolated runtimes
  vscli voices                           List Kokoro voices
  vscli profile add <name> <audio> [--start S] [--end S] --consent
  vscli profiles                         List voice profiles
  vscli say [--engine kokoro|chatterbox|chatterbox-mtl|chatterbox-turbo] [--language tr] [--voice ID] [--clone PROFILE] [--mode voice-only|voice-style]
            [--no-emotion] [--title T] [--mp3] [--device auto|cpu|gpu] [--variant kokoro-82m-fp32|kokoro-82m-q8]
            (--text TEXT | --file FILE)
  vscli batch <file> [same options as say]   Scripts separated by lines containing ===
  vscli bench [--engine E] [--voice V] [--clone P] [--file F]   Measure cold start, RTF, peak RAM
  vscli idle [secs]                      Sample this process at idle (CPU %, RSS)
  vscli diag                             System and installation diagnostics
";

fn main() {
    let filter = std::env::var("VS_LOG").unwrap_or_else(|_| "info".into());
    let _ = filter;
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

struct Args(Vec<String>);
impl Args {
    fn flag(&mut self, name: &str) -> bool {
        if let Some(i) = self.0.iter().position(|a| a == name) {
            self.0.remove(i);
            true
        } else {
            false
        }
    }
    fn opt(&mut self, name: &str) -> Option<String> {
        let i = self.0.iter().position(|a| a == name)?;
        if i + 1 >= self.0.len() {
            return None;
        }
        let v = self.0.remove(i + 1);
        self.0.remove(i);
        Some(v)
    }
}

fn bar(done: u64, total: u64, bps: f64, label: &str) {
    let pct = if total > 0 { done as f64 / total as f64 * 100.0 } else { 0.0 };
    eprint!("\r  {label:<28} {pct:5.1}%  {:>8.1} MB / {:.1} MB  {:>6.1} MB/s   ", done as f64 / 1e6, total as f64 / 1e6, bps / 1e6);
}

fn run() -> Result<()> {
    let mut args = Args(std::env::args().skip(1).collect());
    if args.0.is_empty() || args.flag("--help") || args.flag("-h") {
        print!("{HELP}");
        return Ok(());
    }
    let cmd = args.0.remove(0);
    let mut settings = Settings::load();
    if let Some(d) = args.opt("--device") {
        settings.device = d;
    }
    if let Some(v) = args.opt("--variant") {
        settings.kokoro_variant = v;
    }
    let store = Store::new(&settings.models_dir);
    match cmd.as_str() {
        "packages" => {
            println!("model folder: {}  (free: {:.1} GB)", settings.models_dir.display(), store.free_space().unwrap_or(0) as f64 / 1e9);
            for p in catalog::packages() {
                let s = store.status(&p);
                let state = if !s.supported {
                    "n/a (other platform)".to_string()
                } else if s.installed {
                    format!("installed ({:.1} MB)", s.disk_size as f64 / 1e6)
                } else if s.partial_bytes > 0 {
                    format!("partial {:.1} MB", s.partial_bytes as f64 / 1e6)
                } else {
                    "not installed".into()
                };
                println!("  {:<32} {:>9.1} MB  {:<24} {}", p.id, p.download_size() as f64 / 1e6, state, p.license);
            }
            let rt = pyenv::PyRuntime::chatterbox(&settings.runtimes_dir());
            println!("  {:<32} {:>12}  {}", "chatterbox-runtime", "", match rt.info() {
                Some(i) => format!("installed (Python {}, torch {}, CUDA {})", i.python, i.torch, i.cuda),
                None => "not installed".into(),
            });
        }
        "install" => {
            for id in args.0.clone() {
                let p = catalog::package(&id).with_context(|| format!("unknown package {id}"))?;
                eprintln!("Installing {} ({:.1} MB)", p.title, p.download_size() as f64 / 1e6);
                let t = Instant::now();
                store.install(&p, &Cancel::new(), |f, pr| bar(pr.done, pr.total, pr.bytes_per_sec, f))?;
                eprintln!("\n  done in {:.1}s, verified", t.elapsed().as_secs_f64());
            }
        }
        "adopt" => {
            let id = args.0.first().context("package id required")?.clone();
            let p = catalog::package(&id).context("unknown package")?;
            let dir = args.0.get(1).map(PathBuf::from).unwrap_or_else(|| store.dir(&id));
            let n = store.import_from_dir(&p, &dir)?;
            println!("{id}: {n} file(s) verified by SHA-256 and registered");
        }
        "verify" => {
            let id = args.0.first().context("package id required")?;
            let bad = store.verify(id)?;
            if bad.is_empty() {
                println!("{id}: OK");
            } else {
                bail!("{id}: corrupt files: {}", bad.join(", "));
            }
        }
        "remove" => {
            let id = args.0.first().context("package id required")?;
            store.remove(id)?;
            println!("removed {id}");
        }
        "runtime" => {
            let rt = pyenv::PyRuntime::chatterbox(&settings.runtimes_dir());
            match args.0.first().map(|s| s.as_str()) {
                Some("install") => {
                    let uv = match pyenv::find_uv(&store) {
                        Some(u) => u,
                        None => {
                            let p = catalog::package(&catalog::uv_package_id()).context("no uv package for this platform")?;
                            store.install(&p, &Cancel::new(), |f, pr| bar(pr.done, pr.total, pr.bytes_per_sec, f))?;
                            pyenv::find_uv(&store).context("uv missing after install")?
                        }
                    };
                    let info = rt.install(&uv, &Cancel::new(), &mut |l| eprintln!("  {l}"))?;
                    println!("{}", serde_json::to_string_pretty(&info)?);
                }
                Some("probe") => println!("{}", serde_json::to_string_pretty(&rt.probe()?)?),
                Some("remove") => {
                    rt.remove()?;
                    println!("removed");
                }
                _ => bail!("usage: vscli runtime <install|probe|remove>"),
            }
        }
        "engine" => {
            use vs_core::engine::python;
            let runtimes = settings.runtimes_dir();
            match args.0.first().map(|s| s.as_str()) {
                Some("list") | None => {
                    for e in python::specs() {
                        let st = python::install_state(&e, &store, &runtimes);
                        let state = if !e.supported_here() {
                            "not supported here".to_string()
                        } else if st.runtime && st.weights {
                            "installed".into()
                        } else {
                            format!("runtime {} · weights {}", if st.runtime { "ok" } else { "missing" }, if st.weights { "ok" } else { "missing" })
                        };
                        println!("  {:<14} {:<48} ~{:>5} MB  {}", e.id, e.name, e.approx_download_mb, state);
                    }
                }
                Some("install") => {
                    let id = args.0.get(1).context("engine id required")?.clone();
                    let spec = python::spec(&id).with_context(|| format!("unknown engine {id}"))?;
                    let uv = match pyenv::find_uv(&store) {
                        Some(u) => u,
                        None => {
                            let p = catalog::package(&catalog::uv_package_id()).context("no uv for this platform")?;
                            store.install(&p, &Cancel::new(), |f, pr| bar(pr.done, pr.total, pr.bytes_per_sec, f))?;
                            pyenv::find_uv(&store).context("uv missing after install")?
                        }
                    };
                    python::install(&spec, &store, &runtimes, &uv, &Cancel::new(), &mut |l| eprintln!("  {l}"), &mut |f, pr| {
                        bar(pr.done, pr.total, pr.bytes_per_sec, f)
                    })?;
                    eprintln!();
                }
                Some("remove") => {
                    let id = args.0.get(1).context("engine id required")?.clone();
                    let spec = python::spec(&id).with_context(|| format!("unknown engine {id}"))?;
                    python::remove(&spec, &store, &runtimes)?;
                    println!("removed {id}");
                }
                _ => bail!("usage: vscli engine <list|install ID|remove ID>"),
            }
        }
        "voices" => {
            for v in vs_core::engine::kokoro::Kokoro::list_voices(&store.dir("kokoro-voices-en")) {
                println!("{v}");
            }
        }
        "profile" => {
            if args.0.first().map(|s| s.as_str()) != Some("add") {
                bail!("usage: vscli profile add <name> <audio> [--start S] [--end S] --consent");
            }
            let consent = args.flag("--consent");
            let start = args.opt("--start").map(|s| s.parse::<f64>()).transpose()?;
            let end = args.opt("--end").map(|s| s.parse::<f64>()).transpose()?;
            let name = args.0.get(1).context("name required")?.clone();
            let file = PathBuf::from(args.0.get(2).context("audio file required")?);
            let r = profiles::load_reference(&file)?;
            let pv = profiles::preview(&r);
            println!("reference: {:.1}s @ {} Hz, suggested {:.1}–{:.1}s, quality {:?}", pv.duration_secs, pv.source_sample_rate, pv.suggested_start, pv.suggested_end, pv.quality.verdict);
            for i in &pv.quality.issues {
                println!("  ! {i}");
            }
            if !consent {
                println!("\n{}\nRe-run with --consent to confirm.", profiles::CONSENT_TEXT);
                return Ok(());
            }
            let ps = ProfileStore::new(settings.profiles_dir());
            let p = ps.save(&name, &file, &r, start.unwrap_or(pv.suggested_start), end.unwrap_or(pv.suggested_end), true)?;
            println!("saved profile {} ({}), {:.1}s, verdict {:?}", p.name, p.id, p.duration_secs, p.quality.verdict);
        }
        "profiles" => {
            for p in ProfileStore::new(settings.profiles_dir()).list() {
                println!("{:<28} {:<24} {:>5.1}s  {:?}  SNR≈{:.0} dB", p.id, p.name, p.duration_secs, p.quality.verdict, p.quality.snr_estimate_db);
            }
        }
        "say" | "bench" | "batch" => {
            let engine = args.opt("--engine").unwrap_or_else(|| "kokoro".into());
            let voice = args.opt("--voice").unwrap_or_else(|| settings.last_voice.clone());
            let clone = args.opt("--clone");
            let mode = match args.opt("--mode").as_deref() {
                Some("voice-style") => CloneMode::VoiceStyle,
                _ => CloneMode::VoiceOnly,
            };
            let no_emotion = args.flag("--no-emotion");
            let language = args.opt("--language");
            let mp3 = args.flag("--mp3");
            let title = args.opt("--title").unwrap_or_default();
            let text = if let Some(t) = args.opt("--text") {
                t
            } else if let Some(f) = args.opt("--file") {
                std::fs::read_to_string(&f).with_context(|| format!("read {f}"))?
            } else if cmd == "batch" {
                std::fs::read_to_string(args.0.first().context("batch file required")?)?
            } else if cmd == "bench" {
                include_str!("../../../examples/scripts/benchmark_60s.txt").to_string()
            } else {
                bail!("--text or --file required")
            };
            let opts = RenderOptions {
                title,
                engine: engine.clone(),
                voice,
                emotion_control: !no_emotion,
                voice_clone: clone.is_some(),
                profile_id: clone,
                clone_mode: mode,
                seed: None,
                speed: 1.0,
                export_mp3: Some(mp3),
                bypass_cache: cmd == "bench",
                language: language.clone(),
            };
            let mut studio = Studio::new(settings.clone());
            let cancel = Cancel::new();
            let mut prog = |e: vs_core::render::ProgressEvent| eprint!("\r  [{:>3.0}%] {:<90}", e.fraction * 100.0, e.message);
            if cmd == "batch" {
                let mut q = queue::Queue::load(&settings.queue_path());
                for (t, body) in queue::split_batch(&text) {
                    let mut o = opts.clone();
                    o.title = t.clone();
                    q.add(&t, &body, o)?;
                }
                while let Some(job) = q.next_pending() {
                    q.set_status(&job.id, queue::JobStatus::Running, None, None)?;
                    eprintln!("\n▶ {}", job.title);
                    match studio.render(&job.script, &job.options, &cancel, &mut prog) {
                        Ok(r) => q.set_status(&job.id, queue::JobStatus::Done, None, Some(r.project_dir))?,
                        Err(e) => q.set_status(&job.id, queue::JobStatus::Failed, Some(format!("{e:#}")), None)?,
                    }
                }
                eprintln!();
                for j in &q.jobs {
                    println!("{:<10?} {:<40} {}", j.status, j.title, j.error.clone().unwrap_or_default());
                }
                return Ok(());
            }
            let t = Instant::now();
            let r = studio.render(&text, &opts, &cancel, &mut prog)?;
            let cold = t.elapsed().as_secs_f64();
            eprintln!();
            let s = &r.meta.stats;
            println!("project: {}", r.project_dir.display());
            println!("final:   {}", r.final_wav.display());
            if let Some(m) = &r.final_mp3 {
                println!("mp3:     {}", m.display());
            }
            println!(
                "audio {:.2}s | render {:.2}s | RTF {:.3} | device {} | segments {} ({} cached) | loudness {:.1} -> {:.1} LUFS | peak RSS {:.0} MB{}",
                s.audio_secs, s.render_secs, s.rtf, s.device, s.segments_total, s.segments_cached,
                s.loudness_before_lufs.unwrap_or(f64::NAN), s.loudness_after_lufs.unwrap_or(f64::NAN),
                s.peak_rss_mb.unwrap_or(0.0),
                s.peak_vram_mb.map(|v| format!(" | peak VRAM {v:.0} MB")).unwrap_or_default()
            );
            if cmd == "bench" {
                // Warm run (model already loaded) on the same text measures steady-state
                // speed; the cache is bypassed for both runs.
                let warm = opts.clone();
                let t2 = Instant::now();
                let r2 = studio.render(&text, &warm, &cancel, &mut prog)?;
                eprintln!();
                let w = &r2.meta.stats;
                let result = serde_json::json!({
                    "engine": engine,
                    "model": r2.meta.model,
                    "voice": r2.meta.voice,
                    "device": w.device,
                    "date": chrono::Utc::now(),
                    "system": sysmon::system_summary(),
                    "audio_secs": w.audio_secs,
                    "cold_total_secs": cold,
                    "warm_render_secs": t2.elapsed().as_secs_f64(),
                    "warm_rtf": w.rtf,
                    "cold_rtf": s.rtf,
                    "peak_rss_mb": w.peak_rss_mb.into_iter().chain(s.peak_rss_mb).fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b)))),
                    "peak_vram_mb_total_gpu": w.peak_vram_mb.into_iter().chain(s.peak_vram_mb).fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b)))),
                    "segments": w.segments_total,
                });
                println!("{}", serde_json::to_string_pretty(&result)?);
                let path = settings.benchmarks_path();
                let mut all: Vec<serde_json::Value> = vs_core::fsutil::read_json(&path).unwrap_or_default();
                all.push(result);
                vs_core::fsutil::atomic_write_json(&path, &all)?;
                println!("saved to {}", path.display());
            }
            studio.unload_all();
        }
        "idle" => {
            let secs: u64 = args.0.first().and_then(|s| s.parse().ok()).unwrap_or(5);
            let s = sysmon::sample(&[], std::time::Duration::from_secs(secs));
            println!("{}", serde_json::to_string_pretty(&s)?);
        }
        "diag" => {
            let rt = pyenv::PyRuntime::chatterbox(&settings.runtimes_dir());
            let d = serde_json::json!({
                "app_version": vs_core::APP_VERSION,
                "platform": catalog::platform_id(),
                "system": sysmon::system_summary(),
                "models_dir": settings.models_dir,
                "output_dir": settings.output_dir,
                "data_root": vs_core::settings::data_root(),
                "ort": vs_core::engine::kokoro::find_ort(&store),
                "chatterbox_runtime": rt.info(),
                "uv": pyenv::find_uv(&store),
                "ffmpeg_mp3": vs_core::export::mp3_supported(settings.ffmpeg_path.as_deref()),
                "nvidia": pyenv::has_nvidia_gpu(),
            });
            println!("{}", serde_json::to_string_pretty(&d)?);
        }
        other => bail!("unknown command '{other}'\n\n{HELP}"),
    }
    Ok(())
}
