//! Tauri commands. Errors are returned as user-readable strings.

use crate::{AppState, WorkerMsg};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;
use vs_core::catalog::{self, Store};
use vs_core::download::Cancel;
use vs_core::engine::{self, CloneMode};
use vs_core::markup;
use vs_core::profiles::{self, ProfileStore};
use vs_core::project;
use vs_core::pyenv::{self, PyRuntime};
use vs_core::queue::split_batch;
use vs_core::render::{self, RenderOptions};
use vs_core::settings::{data_root, Settings};
use vs_core::{audio, fsutil, sysmon};

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    format!("{e:#}")
}
fn aerr(e: anyhow::Error) -> String {
    format!("{e:#}")
}

fn settings(st: &State<AppState>) -> Settings {
    st.settings.lock().unwrap().clone()
}

#[tauri::command]
pub fn app_info(st: State<AppState>) -> Value {
    let s = settings(&st);
    json!({
        "version": vs_core::APP_VERSION,
        "platform": catalog::platform_id(),
        "data_root": data_root(),
        "models_dir": s.models_dir,
        "output_dir": s.output_dir,
        "logs_dir": st.log_dir,
        "consent_text": profiles::CONSENT_TEXT,
    })
}

#[tauri::command]
pub fn get_settings(st: State<AppState>) -> Settings {
    settings(&st)
}

#[tauri::command]
pub fn save_settings(st: State<AppState>, new: Settings) -> CmdResult<Settings> {
    apply_settings(&st, new)
}

fn apply_settings(st: &State<AppState>, new: Settings) -> CmdResult<Settings> {
    let mut new = new;
    new.schema_version = vs_core::settings::SETTINGS_SCHEMA;
    new.save().map_err(aerr)?;
    let old = std::mem::replace(&mut *st.settings.lock().unwrap(), new.clone());
    // The worker reloads engines lazily with the new settings.
    let _ = old;
    let _ = st.tx.lock().unwrap().send(WorkerMsg::UpdateSettings(new.clone()));
    Ok(new)
}

/// Change the model folder, optionally moving existing packages there.
#[tauri::command]
pub async fn move_models_dir(st: State<'_, AppState>, dir: PathBuf, move_existing: bool) -> CmdResult<Settings> {
    let mut s = settings(&st);
    if st.shared.lock().unwrap().busy.is_some() {
        return Err("Wait for the current job to finish before moving models.".into());
    }
    std::fs::create_dir_all(&dir).map_err(err)?;
    if move_existing && s.models_dir.exists() && s.models_dir != dir {
        let _ = st.tx.lock().unwrap().send(WorkerMsg::Unload);
        std::thread::sleep(std::time::Duration::from_millis(300));
        for e in std::fs::read_dir(&s.models_dir).map_err(err)?.flatten() {
            let target = dir.join(e.file_name());
            if target.exists() {
                continue;
            }
            if std::fs::rename(e.path(), &target).is_err() {
                copy_dir(&e.path(), &target).map_err(aerr)?;
                std::fs::remove_dir_all(e.path()).map_err(err)?;
            }
        }
    }
    s.models_dir = dir;
    apply_settings(&st, s)
}

fn copy_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let p = e.path();
        let t = dst.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &t)?;
        } else {
            std::fs::copy(&p, &t)?;
        }
    }
    Ok(())
}

#[derive(Serialize)]
pub struct SegmentPreview {
    index: usize,
    text: String,
    emotion: markup::Emotion,
    emphasis: bool,
    start: usize,
    end: usize,
    pause_after_ms: f32,
    pause_kind: Option<markup::PauseKind>,
    emotion_fidelity: engine::Fidelity,
    params: Value,
}

/// Parse a script and show how it will be segmented for the chosen engine (Advanced mode).
#[tauri::command]
pub fn parse_script(st: State<AppState>, script: String, engine_id: String, emotion_control: bool, clone_mode: Option<CloneMode>) -> Value {
    let s = settings(&st);
    let parsed = markup::parse(&script);
    let max = if engine_id == "chatterbox" { 220 } else { 320 };
    let segs = render::plan(&parsed, &s, max);
    let mode = clone_mode.unwrap_or_default();
    let preview: Vec<SegmentPreview> = segs
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let style = engine::plan_style(&engine_id, g.emotion, emotion_control, mode);
            SegmentPreview {
                index: i + 1,
                text: g.text(),
                emotion: g.emotion,
                emphasis: g.runs.iter().any(|r| r.emphasis),
                start: g.start,
                end: g.end,
                pause_after_ms: g.pause_after_ms * if g.pause_kind.is_some() { 1.0 } else { style.pause_scale },
                pause_kind: g.pause_kind,
                emotion_fidelity: style.emotion_fidelity,
                params: serde_json::to_value(&style).unwrap_or_default(),
            }
        })
        .collect();
    json!({
        "spans": parsed.spans,
        "warnings": parsed.warnings,
        "segments": preview,
        "word_count": markup::spoken_words(&parsed).len(),
    })
}

#[tauri::command]
pub async fn catalog(st: State<'_, AppState>) -> CmdResult<Value> {
    let s = settings(&st);
    tauri::async_runtime::spawn_blocking(move || catalog_blocking(s)).await.map_err(err)
}

fn catalog_blocking(s: Settings) -> Value {
    let store = Store::new(&s.models_dir);
    let pkgs: Vec<_> = catalog::packages().iter().filter(|p| p.supports_this_platform()).map(|p| store.status(p)).collect();
    let benchmarks: Value = fsutil::read_json(&s.benchmarks_path()).unwrap_or(json!([]));
    json!({
        "packages": pkgs,
        "models": catalog::models_catalog(),
        "free_space": store.free_space(),
        "models_dir": s.models_dir,
        "benchmarks": benchmarks,
        "ort_candidates": catalog::ort_package_candidates(),
    })
}

#[tauri::command]
pub fn install_package(app: AppHandle, st: State<AppState>, id: String) -> CmdResult<()> {
    let s = settings(&st);
    let p = catalog::package(&id).ok_or("unknown package")?;
    let needed = p.download_size();
    let store = Store::new(&s.models_dir);
    if let Some(free) = store.free_space() {
        if free < needed + 200_000_000 {
            return Err(format!("Not enough disk space: {:.1} GB needed, {:.1} GB free.", needed as f64 / 1e9, free as f64 / 1e9));
        }
    }
    let cancel = Cancel::new();
    {
        let mut d = st.downloads.lock().unwrap();
        if d.contains_key(&id) {
            return Err("already downloading".into());
        }
        d.insert(id.clone(), cancel.clone());
    }
    std::thread::spawn(move || {
        let r = store.install(&p, &cancel, |file, pr| {
            let _ = app.emit("download-progress", json!({"id": p.id, "file": file, "done": pr.done, "total": pr.total, "bps": pr.bytes_per_sec}));
        });
        let st = tauri::Manager::state::<AppState>(&app);
        st.downloads.lock().unwrap().remove(&p.id);
        let _ = app.emit(
            "download-done",
            json!({"id": p.id, "ok": r.is_ok(), "cancelled": cancel.is_cancelled(), "error": r.err().map(|e| format!("{e:#}"))}),
        );
    });
    Ok(())
}

#[tauri::command]
pub fn cancel_download(st: State<AppState>, id: String) {
    if let Some(c) = st.downloads.lock().unwrap().get(&id) {
        c.cancel();
    }
}

#[tauri::command]
pub async fn remove_package(st: State<'_, AppState>, id: String) -> CmdResult<()> {
    let _ = st.tx.lock().unwrap().send(WorkerMsg::Unload);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let s = settings(&st);
    Store::new(&s.models_dir).remove(&id).map_err(aerr)
}

#[tauri::command]
pub async fn verify_package(st: State<'_, AppState>, id: String) -> CmdResult<Vec<String>> {
    let s = settings(&st);
    tauri::async_runtime::spawn_blocking(move || Store::new(&s.models_dir).verify(&id)).await.map_err(err)?.map_err(aerr)
}

#[tauri::command]
pub async fn import_package(st: State<'_, AppState>, id: String, dir: PathBuf) -> CmdResult<usize> {
    let s = settings(&st);
    let p = catalog::package(&id).ok_or("unknown package")?;
    tauri::async_runtime::spawn_blocking(move || Store::new(&s.models_dir).import_from_dir(&p, &dir)).await.map_err(err)?.map_err(aerr)
}

#[tauri::command]
pub async fn runtime_status(st: State<'_, AppState>) -> CmdResult<Value> {
    let s = settings(&st);
    let installing = st.downloads.lock().unwrap().contains_key("chatterbox-runtime");
    tauri::async_runtime::spawn_blocking(move || runtime_status_blocking(s, installing)).await.map_err(err)
}

fn runtime_status_blocking(s: Settings, installing: bool) -> Value {
    let rt = PyRuntime::chatterbox(&s.runtimes_dir());
    let store = Store::new(&s.models_dir);
    json!({
        "installed": rt.info(),
        "unsupported": pyenv::chatterbox_unsupported_reason(),
        "dir": rt.dir,
        "disk_size": fsutil::dir_size(&rt.dir),
        "uv": pyenv::find_uv(&store),
        "nvidia": pyenv::has_nvidia_gpu(),
        "torch_index": PyRuntime::torch_index(),
        "estimated_download_gb": if pyenv::has_nvidia_gpu() { 3.5 } else { 1.2 },
        "installing": installing,
    })
}

#[tauri::command]
pub fn install_runtime(app: AppHandle, st: State<AppState>) -> CmdResult<()> {
    let s = settings(&st);
    let cancel = Cancel::new();
    {
        let mut d = st.downloads.lock().unwrap();
        if d.contains_key("chatterbox-runtime") {
            return Err("already installing".into());
        }
        d.insert("chatterbox-runtime".into(), cancel.clone());
    }
    std::thread::spawn(move || {
        let store = Store::new(&s.models_dir);
        let mut log = |l: String| {
            let _ = app.emit("runtime-log", l);
        };
        let r = (|| -> anyhow::Result<()> {
            let uv = match pyenv::find_uv(&store) {
                Some(u) => u,
                None => {
                    log("Downloading uv…".into());
                    let p = catalog::package(&catalog::uv_package_id()).ok_or_else(|| anyhow::anyhow!("uv is not available for this platform"))?;
                    store.install(&p, &cancel, |_, pr| {
                        let _ = app.emit("download-progress", json!({"id": "chatterbox-runtime", "file": "uv", "done": pr.done, "total": pr.total, "bps": pr.bytes_per_sec}));
                    })?;
                    pyenv::find_uv(&store).ok_or_else(|| anyhow::anyhow!("uv missing after install"))?
                }
            };
            PyRuntime::chatterbox(&s.runtimes_dir()).install(&uv, &cancel, &mut log)?;
            Ok(())
        })();
        let st = tauri::Manager::state::<AppState>(&app);
        st.downloads.lock().unwrap().remove("chatterbox-runtime");
        let _ = app.emit("download-done", json!({"id": "chatterbox-runtime", "ok": r.is_ok(), "cancelled": cancel.is_cancelled(), "error": r.err().map(|e| format!("{e:#}"))}));
    });
    Ok(())
}

#[tauri::command]
pub async fn remove_runtime(st: State<'_, AppState>) -> CmdResult<()> {
    let _ = st.tx.lock().unwrap().send(WorkerMsg::Unload);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let s = settings(&st);
    PyRuntime::chatterbox(&s.runtimes_dir()).remove().map_err(aerr)
}

#[tauri::command]
pub async fn voices(st: State<'_, AppState>) -> CmdResult<Value> {
    let s = settings(&st);
    tauri::async_runtime::spawn_blocking(move || voices_blocking(s)).await.map_err(err)
}

fn voices_blocking(s: Settings) -> Value {
    let store = Store::new(&s.models_dir);
    let kokoro: Vec<Value> = engine::kokoro::Kokoro::list_voices(&store.dir("kokoro-voices-en"))
        .into_iter()
        .map(|v| {
            let (accent, gender) = match &v[..2.min(v.len())] {
                "af" => ("American", "female"),
                "am" => ("American", "male"),
                "bf" => ("British", "female"),
                "bm" => ("British", "male"),
                _ => ("", ""),
            };
            let name = v.split('_').nth(1).map(|n| {
                let mut c = n.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            });
            json!({"id": v, "name": name, "accent": accent, "gender": gender})
        })
        .collect();
    let profiles = ProfileStore::new(s.profiles_dir()).list();
    json!({"kokoro": kokoro, "profiles": profiles})
}

/// Which engines can run right now and what they genuinely support.
#[tauri::command]
pub async fn engine_status(st: State<'_, AppState>) -> CmdResult<Value> {
    let s = settings(&st);
    let (loaded, busy, batch_running) = {
        let sh = st.shared.lock().unwrap();
        (sh.loaded.clone(), sh.busy.clone(), sh.batch_running)
    };
    tauri::async_runtime::spawn_blocking(move || engine_status_blocking(s, loaded, busy, batch_running)).await.map_err(err)
}

fn engine_status_blocking(s: Settings, loaded: Vec<(String, String)>, busy: Option<String>, batch_running: bool) -> Value {
    let store = Store::new(&s.models_dir);
    let kok = engine::kokoro::KokoroPaths::resolve(&store, &s.kokoro_variant);
    let cb = engine::chatterbox::ChatterboxPaths::resolve(&store, &s.runtimes_dir(), s.cache_dir().join("tmp"));
    fn missing<T>(r: &anyhow::Result<T>) -> Vec<String> {
        match r {
            Ok(_) => vec![],
            Err(e) => format!("{e:#}").split("MISSING_PACKAGES:").nth(1).map(|x| x.split(',').map(|s| s.trim().to_string()).collect()).unwrap_or_default(),
        }
    }
    json!({
        "engines": [
            {"id": "kokoro", "name": "Kokoro 82M (fast, built-in voices)", "ready": kok.is_ok(), "missing": missing(&kok), "caps": engine::kokoro::caps()},
            {"id": "chatterbox", "name": "Chatterbox (voice cloning, emotion intensity)", "ready": cb.is_ok(), "missing": missing(&cb), "caps": engine::chatterbox::caps(), "unsupported": pyenv::chatterbox_unsupported_reason()},
        ],
        "loaded": loaded,
        "busy": busy,
        "batch_running": batch_running,
        "mp3": vs_core::export::mp3_supported(s.ffmpeg_path.as_deref()),
    })
}

#[tauri::command]
pub fn make(st: State<AppState>, script: String, options: RenderOptions) -> CmdResult<String> {
    if st.shared.lock().unwrap().busy.is_some() {
        return Err("A job is already running (one job at a time).".into());
    }
    if script.trim().is_empty() {
        return Err("The script is empty.".into());
    }
    let id = format!("make-{}", chrono::Utc::now().timestamp_millis());
    st.shared.lock().unwrap().busy = Some(id.clone());
    st.tx.lock().unwrap().send(WorkerMsg::Make { id: id.clone(), script, options }).map_err(err)?;
    Ok(id)
}

#[tauri::command]
pub fn cancel_render(st: State<AppState>) {
    st.cancel.cancel();
}

#[tauri::command]
pub fn unload_models(st: State<AppState>) {
    let _ = st.tx.lock().unwrap().send(WorkerMsg::Unload);
}

#[tauri::command]
pub fn queue_list(st: State<AppState>) -> Value {
    let q = st.queue.lock().unwrap();
    json!({"jobs": q.jobs, "running": st.shared.lock().unwrap().batch_running})
}

/// Add scripts to the persistent queue. `text` may contain several scripts separated by
/// lines with `===`; `files` are read as one script each (or split the same way).
#[tauri::command]
pub fn queue_add(st: State<AppState>, text: Option<String>, files: Option<Vec<PathBuf>>, options: RenderOptions) -> CmdResult<usize> {
    let mut scripts = Vec::new();
    if let Some(t) = text {
        scripts.extend(split_batch(&t));
    }
    for f in files.unwrap_or_default() {
        let body = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let parts = split_batch(&body);
        let stem = f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let n = parts.len();
        for (i, (t, b)) in parts.into_iter().enumerate() {
            let title = if !t.is_empty() { t } else if n > 1 { format!("{stem} ({})", i + 1) } else { stem.clone() };
            scripts.push((title, b));
        }
    }
    let mut q = st.queue.lock().unwrap();
    for (t, b) in &scripts {
        let mut o = options.clone();
        o.title = t.clone();
        q.add(t, b, o).map_err(aerr)?;
    }
    Ok(scripts.len())
}

#[tauri::command]
pub fn queue_start(st: State<AppState>) -> CmdResult<()> {
    if st.shared.lock().unwrap().busy.is_some() {
        return Err("A job is already running.".into());
    }
    st.tx.lock().unwrap().send(WorkerMsg::RunQueue).map_err(err)
}

/// Pause after the current job finishes (does not interrupt it).
#[tauri::command]
pub fn queue_pause(st: State<AppState>) {
    st.shared.lock().unwrap().batch_running = false;
}

#[tauri::command]
pub fn queue_retry(st: State<AppState>, id: Option<String>) -> CmdResult<()> {
    let mut q = st.queue.lock().unwrap();
    match id {
        Some(i) => q.retry(&i),
        None => q.retry_failed(),
    }
    .map_err(aerr)
}

/// Cancel: a pending job is marked cancelled; the running job is interrupted.
#[tauri::command]
pub fn queue_cancel(st: State<AppState>, id: Option<String>) -> CmdResult<()> {
    let busy = st.shared.lock().unwrap().busy.clone();
    if id.is_none() || busy == id {
        st.cancel.cancel();
    }
    st.queue.lock().unwrap().cancel_pending(id.as_deref()).map_err(aerr)
}

#[tauri::command]
pub fn queue_remove(st: State<AppState>, id: String) -> CmdResult<()> {
    st.queue.lock().unwrap().remove(&id).map_err(aerr)
}

#[tauri::command]
pub fn queue_clear_finished(st: State<AppState>) -> CmdResult<()> {
    st.queue.lock().unwrap().clear_finished().map_err(aerr)
}

#[tauri::command]
pub async fn projects(st: State<'_, AppState>, query: String) -> CmdResult<Vec<project::ProjectSummary>> {
    let root = settings(&st).projects_dir();
    tauri::async_runtime::spawn_blocking(move || project::list(&root, &query)).await.map_err(err)
}

#[tauri::command]
pub fn project_get(st: State<AppState>, dir: PathBuf) -> CmdResult<project::ProjectMeta> {
    check_project(&st, &dir)?;
    project::load(&dir).map_err(aerr)
}

#[tauri::command]
pub fn project_rename(st: State<AppState>, dir: PathBuf, title: String) -> CmdResult<()> {
    check_project(&st, &dir)?;
    if title.trim().is_empty() {
        return Err("Title cannot be empty".into());
    }
    project::rename(&dir, &title).map_err(aerr)
}

#[tauri::command]
pub fn project_delete(st: State<AppState>, dir: PathBuf) -> CmdResult<()> {
    let root = settings(&st).projects_dir();
    project::delete(&root, &dir).map_err(aerr)
}

fn check_project(st: &State<AppState>, dir: &Path) -> CmdResult<()> {
    let root = settings(st).projects_dir();
    if fsutil::is_within(&root, dir) {
        Ok(())
    } else {
        Err("path is outside the projects folder".into())
    }
}

/// Raw bytes of an audio file inside the projects or voices folders (for the player).
#[tauri::command]
pub async fn read_audio(st: State<'_, AppState>, path: PathBuf) -> CmdResult<Response> {
    let s = settings(&st);
    let ok = [s.projects_dir(), s.profiles_dir(), s.output_dir.clone()].iter().any(|r| fsutil::is_within(r, &path));
    if !ok {
        return Err("access denied: file is outside Voice Studio folders".into());
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if !matches!(ext.as_str(), "wav" | "mp3") {
        return Err("not an audio file".into());
    }
    std::fs::read(&path).map(Response::new).map_err(err)
}

#[tauri::command]
pub fn reveal(app: AppHandle, path: PathBuf) -> CmdResult<()> {
    app.opener().reveal_item_in_dir(&path).map_err(err)
}

#[tauri::command]
pub fn open_folder(app: AppHandle, st: State<AppState>, which: String) -> CmdResult<()> {
    let s = settings(&st);
    let p = match which.as_str() {
        "models" => s.models_dir,
        "output" => s.output_dir,
        "projects" => s.projects_dir(),
        "logs" => st.log_dir.clone(),
        "data" => data_root(),
        _ => return Err("unknown folder".into()),
    };
    std::fs::create_dir_all(&p).map_err(err)?;
    app.opener().open_path(p.to_string_lossy(), None::<&str>).map_err(err)
}

/// Copy a render to a user-chosen destination; converts to MP3 when the destination ends
/// with .mp3 (requires FFmpeg).
#[tauri::command]
pub async fn export_audio(st: State<'_, AppState>, src: PathBuf, dest: PathBuf) -> CmdResult<PathBuf> {
    let s = settings(&st);
    tauri::async_runtime::spawn_blocking(move || -> anyhow::Result<PathBuf> {
        let is_mp3 = dest.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mp3"));
        if is_mp3 && !src.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("mp3")) {
            vs_core::export::wav_to_mp3(&src, &dest, s.ffmpeg_path.as_deref())
        } else {
            let bytes = std::fs::read(&src)?;
            fsutil::atomic_write(&dest, &bytes)?;
            Ok(dest)
        }
    })
    .await
    .map_err(err)?
    .map_err(aerr)
}

#[tauri::command]
pub async fn reference_load(st: State<'_, AppState>, path: PathBuf) -> CmdResult<profiles::ReferencePreview> {
    let p2 = path.clone();
    let r = tauri::async_runtime::spawn_blocking(move || profiles::load_reference(&p2)).await.map_err(err)?.map_err(aerr)?;
    let pv = profiles::preview(&r);
    *st.pending_ref.lock().unwrap() = Some((path, r));
    Ok(pv)
}

/// WAV bytes of the selected region (preview playback) plus its quality report.
#[tauri::command]
pub async fn reference_clip(st: State<'_, AppState>, start: f64, end: f64) -> CmdResult<Value> {
    let g = st.pending_ref.lock().unwrap();
    let (_, r) = g.as_ref().ok_or("no reference loaded")?;
    let clip = profiles::slice(&r.audio, start, end);
    let q = audio::analyze_quality(&clip, r.source_sr);
    let tmp = settings(&st).cache_dir().join("tmp").join("ref-preview.wav");
    audio::write_wav(&tmp, &clip).map_err(aerr)?;
    let bytes = std::fs::read(&tmp).map_err(err)?;
    Ok(json!({"quality": q, "wav_b64": b64(&bytes)}))
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len() * 4 / 3 + 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

#[tauri::command]
pub async fn profile_save(st: State<'_, AppState>, name: String, start: f64, end: f64, consent: bool) -> CmdResult<profiles::VoiceProfile> {
    let g = st.pending_ref.lock().unwrap();
    let (src, r) = g.as_ref().ok_or("no reference loaded")?;
    ProfileStore::new(settings(&st).profiles_dir()).save(&name, src, r, start, end, consent).map_err(aerr)
}

#[tauri::command]
pub fn profile_rename(st: State<AppState>, id: String, name: String) -> CmdResult<()> {
    ProfileStore::new(settings(&st).profiles_dir()).rename(&id, &name).map(|_| ()).map_err(aerr)
}

#[tauri::command]
pub fn profile_delete(st: State<AppState>, id: String) -> CmdResult<()> {
    ProfileStore::new(settings(&st).profiles_dir()).delete(&id).map_err(aerr)
}

#[tauri::command]
pub async fn diagnostics(st: State<'_, AppState>) -> CmdResult<Value> {
    let s = settings(&st);
    let (loaded, pid, busy) = {
        let sh = st.shared.lock().unwrap();
        (sh.loaded.clone(), sh.worker_pid, sh.busy.clone())
    };
    let log_dir = st.log_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let store = Store::new(&s.models_dir);
        let procs = sysmon::sample(&pid.into_iter().collect::<Vec<_>>(), std::time::Duration::from_millis(1000));
        let rt = PyRuntime::chatterbox(&s.runtimes_dir());
        let recent_log = std::fs::read_dir(&log_dir)
            .ok()
            .and_then(|rd| rd.flatten().max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok()))
            .and_then(|e| std::fs::read_to_string(e.path()).ok())
            .map(|t| t.lines().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        Ok(json!({
            "app_version": vs_core::APP_VERSION,
            "platform": catalog::platform_id(),
            "system": sysmon::system_summary(),
            "processes": procs,
            "loaded_engines": loaded,
            "busy": busy,
            "ort_library": engine::kokoro::find_ort(&store),
            "chatterbox_runtime": rt.info(),
            "nvidia_gpu": pyenv::has_nvidia_gpu(),
            "gpu_memory_used_mb": sysmon::nvidia_used_mb(),
            "ffmpeg": vs_core::export::find_ffmpeg(s.ffmpeg_path.as_deref()),
            "mp3_supported": vs_core::export::mp3_supported(s.ffmpeg_path.as_deref()),
            "paths": {"data": data_root(), "models": s.models_dir, "output": s.output_dir, "cache": s.cache_dir(), "logs": log_dir},
            "cache_size_mb": fsutil::dir_size(&s.cache_dir()) as f64 / 1e6,
            "models_size_mb": fsutil::dir_size(&s.models_dir) as f64 / 1e6,
            "free_space_gb": store.free_space().map(|b| b as f64 / 1e9),
            "recent_log": recent_log,
        }))
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn clear_cache(st: State<'_, AppState>) -> CmdResult<f64> {
    let s = settings(&st);
    let freed = render::prune_cache(&s.cache_dir().join("segments"), 0).map_err(aerr)?;
    Ok(freed as f64 / 1e6)
}

/// Read a user-chosen script file (.txt/.md, up to 5 MB).
#[tauri::command]
pub fn read_script_file(path: PathBuf) -> CmdResult<String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if !matches!(ext.as_str(), "txt" | "md") {
        return Err("Only .txt and .md scripts can be opened".into());
    }
    let len = std::fs::metadata(&path).map_err(err)?.len();
    if len > 5_000_000 {
        return Err("Script file is larger than 5 MB".into());
    }
    let bytes = std::fs::read(&path).map_err(err)?;
    let text = String::from_utf8(bytes).map_err(|_| "Script must be UTF-8 text".to_string())?;
    Ok(text.trim_start_matches('\u{feff}').to_string())
}
