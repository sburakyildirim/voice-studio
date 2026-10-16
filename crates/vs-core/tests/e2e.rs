//! End-to-end tests against real models. They are ignored by default because they need the
//! downloaded packages; run them with:
//!
//!     cargo test -p vs-core --release --test e2e -- --ignored --test-threads=1
//!
//! Models are read from the user's normal model folder (or VS_TEST_MODELS); everything else
//! (cache, projects, profiles, queue) goes to a temporary VOICE_STUDIO_HOME.

use std::path::PathBuf;
use std::sync::OnceLock;
use vs_core::audio;
use vs_core::download::Cancel;
use vs_core::engine::CloneMode;
use vs_core::markup;
use vs_core::profiles::{self, ProfileStore};
use vs_core::project;
use vs_core::queue::{JobStatus, Queue};
use vs_core::render::{RenderOptions, Studio};
use vs_core::settings::Settings;

struct Env {
    _home: tempfile::TempDir,
    models: PathBuf,
    runtimes: PathBuf,
}

fn env() -> &'static Env {
    static E: OnceLock<Env> = OnceLock::new();
    E.get_or_init(|| {
        let models = std::env::var("VS_TEST_MODELS").map(PathBuf::from).unwrap_or_else(|_| Settings::load().models_dir);
        // The Python runtime is large: tests use the real one in place.
        let runtimes = Settings::load().runtimes_dir();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("VOICE_STUDIO_HOME", home.path());
        Env { _home: home, models, runtimes }
    })
}

fn settings() -> Settings {
    let e = env();
    let mut s = Settings::default();
    s.models_dir = e.models.clone();
    s.runtimes_dir = Some(e.runtimes.clone());
    s.output_dir = vs_core::settings::data_root().join("out");
    s.device = "cpu".into();
    s
}

fn kokoro_opts() -> RenderOptions {
    RenderOptions {
        title: "e2e".into(),
        engine: "kokoro".into(),
        voice: "af_heart".into(),
        emotion_control: true,
        voice_clone: false,
        profile_id: None,
        clone_mode: CloneMode::VoiceOnly,
        seed: None,
        speed: 1.0,
        export_mp3: Some(false),
        bypass_cache: false,
    }
}

const SCRIPT: &str = r#"Welcome back to the channel. "Today is going to be amazing!" -- [Take a slow breath.] --- {Something is moving in the dark.} This is *really* important.

Second paragraph, with numbers: 3 hearts and $5.50 in 2026."#;

#[test]
#[ignore]
fn kokoro_render_cache_and_invalidation() {
    let mut studio = Studio::new(settings());
    let r1 = studio.render(SCRIPT, &kokoro_opts(), &Cancel::new(), &mut |_| {}).expect("render");
    let a = audio::read_wav(&r1.final_wav).unwrap();
    assert!(a.duration_secs() > 8.0, "duration {}", a.duration_secs());
    assert!(a.peak() <= audio::db_to_lin(-1.0) + 1e-3, "peak ceiling");
    let l = audio::integrated_lufs(&a).unwrap();
    assert!((l - -16.0).abs() < 1.5, "loudness {l}");
    assert_eq!(r1.meta.stats.segments_cached, 0);
    // Words preserved: the segments' texts reproduce the parsed words exactly.
    let seg_words: Vec<String> = r1.meta.segments.iter().flat_map(|s| s.text.split_whitespace().map(String::from).collect::<Vec<_>>()).collect();
    assert_eq!(seg_words, markup::spoken_words(&markup::parse(SCRIPT)));
    // Project folder contents.
    for f in ["project.json", "script.txt", "final.wav", "segments/001.wav"] {
        assert!(r1.project_dir.join(f).exists(), "{f}");
    }

    // Unchanged script: every segment from cache, identical audio.
    let r2 = studio.render(SCRIPT, &kokoro_opts(), &Cancel::new(), &mut |_| {}).unwrap();
    assert_eq!(r2.meta.stats.segments_cached, r2.meta.stats.segments_total);
    let b = audio::read_wav(&r2.final_wav).unwrap();
    assert_eq!(a.samples.len(), b.samples.len());

    // Change one sentence: only that segment is re-synthesized.
    let edited = SCRIPT.replace("Take a slow breath.", "Take a deep breath.");
    let r3 = studio.render(&edited, &kokoro_opts(), &Cancel::new(), &mut |_| {}).unwrap();
    assert_eq!(r3.meta.stats.segments_cached, r3.meta.stats.segments_total - 1);

    // Different voice invalidates everything.
    let mut o = kokoro_opts();
    o.voice = "am_michael".into();
    let r4 = studio.render(SCRIPT, &o, &Cancel::new(), &mut |_| {}).unwrap();
    assert_eq!(r4.meta.stats.segments_cached, 0);

    // Search finds the project by script text.
    let found = project::list(&studio.settings.projects_dir(), "amazing breath");
    assert!(!found.is_empty());
    studio.unload_all();
}

#[test]
#[ignore]
fn missing_models_reported_precisely() {
    let mut s = settings();
    s.models_dir = vs_core::settings::data_root().join("empty-models");
    let mut studio = Studio::new(s);
    let e = studio.render("Hello there.", &kokoro_opts(), &Cancel::new(), &mut |_| {}).unwrap_err();
    let m = format!("{e:#}");
    assert!(m.contains("MISSING_PACKAGES") && m.contains("kokoro-voices-en"), "{m}");
    let mut o = kokoro_opts();
    o.engine = "chatterbox".into();
    let e = studio.render("Hello there.", &o, &Cancel::new(), &mut |_| {}).unwrap_err();
    assert!(format!("{e:#}").contains("chatterbox-en"));
}

#[test]
#[ignore]
fn cloning_rejected_on_engine_without_support() {
    let mut studio = Studio::new(settings());
    let mut o = kokoro_opts();
    o.voice_clone = true;
    o.profile_id = Some("x".into());
    let e = studio.render("Hello.", &o, &Cancel::new(), &mut |_| {}).unwrap_err();
    assert!(format!("{e:#}").contains("does not support voice cloning"));
}

#[test]
#[ignore]
fn cancel_stops_and_cleans_up() {
    let mut studio = Studio::new(settings());
    let cancel = Cancel::new();
    let long = "This sentence is here to keep the engine busy for a while. ".repeat(40);
    let c2 = cancel.clone();
    let mut n = 0;
    let r = studio.render(&long, &kokoro_opts(), &cancel, &mut |e| {
        if e.stage == "synth" {
            n += 1;
            if n == 2 {
                c2.cancel();
            }
        }
    });
    assert!(format!("{:#}", r.unwrap_err()).contains("cancelled"));
    // No half-written project left behind.
    let leftovers = project::list(&studio.settings.projects_dir(), "keep the engine busy");
    assert!(leftovers.is_empty());
}

#[test]
#[ignore]
fn corrupt_reference_is_detected() {
    let s = settings();
    let ps = ProfileStore::new(s.profiles_dir());
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/reference/synthetic_kokoro_bf_emma.wav");
    let r = profiles::load_reference(&src).unwrap();
    let p = ps.save("Corrupt test", &src, &r, 0.5, 20.0, true).unwrap();
    std::fs::write(ps.reference_path(&p.id), b"RIFF garbage").unwrap();
    let mut studio = Studio::new(s);
    let mut o = kokoro_opts();
    o.engine = "chatterbox".into();
    o.voice_clone = true;
    o.profile_id = Some(p.id.clone());
    let e = studio.render("Hello.", &o, &Cancel::new(), &mut |_| {}).unwrap_err();
    let m = format!("{e:#}");
    assert!(m.contains("corrupt") || m.contains("MISSING_PACKAGES"), "{m}");
    // A corrupt audio file is rejected at import time too.
    let bad = vs_core::settings::data_root().join("bad.mp3");
    std::fs::write(&bad, b"ID3\x03\x00\x00\x00\x00\x00\x00not really audio").unwrap();
    assert!(profiles::load_reference(&bad).is_err());
}

#[test]
#[ignore]
fn batch_queue_resumes_after_restart() {
    let s = settings();
    let qp = s.queue_path();
    let _ = std::fs::remove_file(&qp);
    {
        let mut q = Queue::load(&qp);
        for (i, t) in ["One short line.", "Two short lines here.", "Three, and the last one."].iter().enumerate() {
            q.add(&format!("job {i}"), t, kokoro_opts()).unwrap();
        }
        let first = q.jobs[0].id.clone();
        q.set_status(&first, JobStatus::Running, None, None).unwrap();
        // "crash" here: the queue object is dropped while a job is marked running.
    }
    let mut q = Queue::load(&qp);
    assert_eq!(q.jobs.iter().filter(|j| j.status == JobStatus::Pending).count(), 3);
    let mut studio = Studio::new(s);
    while let Some(job) = q.next_pending() {
        q.set_status(&job.id, JobStatus::Running, None, None).unwrap();
        match studio.render(&job.script, &job.options, &Cancel::new(), &mut |_| {}) {
            Ok(r) => q.set_status(&job.id, JobStatus::Done, None, Some(r.project_dir)).unwrap(),
            Err(e) => q.set_status(&job.id, JobStatus::Failed, Some(e.to_string()), None).unwrap(),
        }
    }
    assert!(q.jobs.iter().all(|j| j.status == JobStatus::Done), "{:?}", q.jobs.iter().map(|j| (&j.title, j.status, &j.error)).collect::<Vec<_>>());
    assert_eq!(q.jobs[0].attempts, 2, "first job ran twice (interrupted + resumed)");
}

#[test]
#[ignore]
fn chatterbox_clone_and_worker_lifecycle() {
    let mut s = settings();
    s.device = "auto".into();
    if !vs_core::pyenv::PyRuntime::chatterbox(&s.runtimes_dir()).is_installed() {
        eprintln!("skipping: Chatterbox runtime not installed");
        return;
    }
    let ps = ProfileStore::new(s.profiles_dir());
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/reference/synthetic_kokoro_bf_emma.wav");
    let r = profiles::load_reference(&src).unwrap();
    let p = ps.save("Synthetic Emma", &src, &r, 0.1, 41.0, true).unwrap();
    let mut studio = Studio::new(s);
    let mut o = kokoro_opts();
    o.engine = "chatterbox".into();
    o.voice_clone = true;
    o.profile_id = Some(p.id.clone());
    let res = studio.render(r#""This is a cloned voice test!" [Calm now.]"#, &o, &Cancel::new(), &mut |_| {}).expect("clone render");
    let a = audio::read_wav(&res.final_wav).unwrap();
    assert!(a.duration_secs() > 1.5);
    assert!(res.project_dir.join("reference.wav").exists());
    let pid = studio.chatterbox_worker_pid().expect("worker running");
    // Unloading must terminate the worker process (no orphans).
    studio.unload_all();
    std::thread::sleep(std::time::Duration::from_secs(3));
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    assert!(sys.process(sysinfo::Pid::from_u32(pid)).is_none(), "worker {pid} still alive");
}
