//! Voice Studio desktop shell: Tauri commands + one background render worker.
//!
//! Threading model
//! * The UI thread never touches models.
//! * One worker thread owns the `Studio` (engines are loaded lazily on the first MAKE and
//!   unloaded after the idle timeout), processes MAKE requests and the batch queue one job
//!   at a time, and sleeps on a channel otherwise (no polling, ~0% CPU at idle).
//! * Downloads/runtime installs run on their own short-lived threads.

mod commands;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use vs_core::download::Cancel;
use vs_core::queue::{JobStatus, Queue};
use vs_core::render::{RenderOptions, Studio};
use vs_core::settings::Settings;

pub enum WorkerMsg {
    Make { id: String, script: String, options: RenderOptions },
    RunQueue,
    UpdateSettings(Settings),
    Unload,
}

#[derive(Default)]
pub struct Shared {
    pub busy: Option<String>,
    pub batch_running: bool,
    pub loaded: Vec<(String, String)>,
    pub worker_pid: Option<u32>,
}

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub tx: Mutex<Sender<WorkerMsg>>,
    pub cancel: Cancel,
    pub shared: Arc<Mutex<Shared>>,
    pub queue: Arc<Mutex<Queue>>,
    pub downloads: Mutex<std::collections::HashMap<String, Cancel>>,
    pub pending_ref: Mutex<Option<(PathBuf, vs_core::profiles::PendingReference)>>,
    pub log_dir: PathBuf,
}

#[derive(Clone, serde::Serialize)]
struct JobEvent {
    id: String,
    kind: &'static str,
    project_dir: Option<PathBuf>,
    error: Option<String>,
    missing_packages: Vec<String>,
}

fn missing_packages(e: &anyhow::Error) -> Vec<String> {
    let s = format!("{e:#}");
    s.split("MISSING_PACKAGES:").nth(1).map(|r| r.split(|c: char| c == ',' || c.is_whitespace()).filter(|x| !x.is_empty()).map(String::from).collect()).unwrap_or_default()
}

fn friendly(e: &anyhow::Error) -> String {
    let m = missing_packages(e);
    if !m.is_empty() {
        return format!("Required downloads are missing: {}. Open Options › Models to install them.", m.join(", "));
    }
    format!("{e:#}")
}

fn worker_loop(app: AppHandle, rx: Receiver<WorkerMsg>, settings: Settings, cancel: Cancel, shared: Arc<Mutex<Shared>>, queue: Arc<Mutex<Queue>>) {
    let mut studio = Studio::new(settings);
    let sync = |studio: &Studio, shared: &Arc<Mutex<Shared>>, app: &AppHandle| {
        let mut s = shared.lock().unwrap();
        s.loaded = studio.loaded_engines();
        s.worker_pid = studio.chatterbox_worker_pid();
        let _ = app.emit("engine-state", &s.loaded);
    };
    let run_one = |studio: &mut Studio, id: &str, script: &str, opts: &RenderOptions, batch: bool| -> Result<PathBuf, anyhow::Error> {
        cancel.reset();
        shared.lock().unwrap().busy = Some(id.to_string());
        let _ = app.emit("job-event", JobEvent { id: id.into(), kind: "started", project_dir: None, error: None, missing_packages: vec![] });
        let app2 = app.clone();
        let jid = id.to_string();
        let mut progress = move |e: vs_core::render::ProgressEvent| {
            let _ = app2.emit("render-progress", serde_json::json!({ "id": jid, "batch": batch, "event": e }));
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| studio.render(script, opts, &cancel, &mut progress)));
        shared.lock().unwrap().busy = None;
        match r {
            Ok(r) => r.map(|r| r.project_dir),
            Err(_) => {
                // An engine panicked: drop everything so the next job starts clean.
                studio.unload_all();
                Err(anyhow::anyhow!("internal error during rendering (engine crashed); models were unloaded"))
            }
        }
    };
    loop {
        let msg = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match msg {
            None => {
                if studio.unload_if_idle() {
                    tracing::info!("models unloaded after idle timeout");
                    sync(&studio, &shared, &app);
                }
            }
            Some(WorkerMsg::UpdateSettings(s)) => {
                studio.unload_all();
                studio.settings = s;
                sync(&studio, &shared, &app);
            }
            Some(WorkerMsg::Unload) => {
                studio.unload_all();
                sync(&studio, &shared, &app);
            }
            Some(WorkerMsg::Make { id, script, options }) => {
                let res = run_one(&mut studio, &id, &script, &options, false);
                sync(&studio, &shared, &app);
                let ev = match res {
                    Ok(dir) => JobEvent { id, kind: "done", project_dir: Some(dir), error: None, missing_packages: vec![] },
                    Err(e) => {
                        let cancelled = cancel.is_cancelled();
                        tracing::warn!("render failed: {e:#}");
                        JobEvent { id, kind: if cancelled { "cancelled" } else { "failed" }, project_dir: None, error: Some(friendly(&e)), missing_packages: missing_packages(&e) }
                    }
                };
                let _ = app.emit("job-event", ev);
            }
            Some(WorkerMsg::RunQueue) => {
                shared.lock().unwrap().batch_running = true;
                let _ = app.emit("queue-changed", ());
                loop {
                    if !shared.lock().unwrap().batch_running {
                        break;
                    }
                    let job = queue.lock().unwrap().next_pending();
                    let Some(job) = job else { break };
                    let _ = queue.lock().unwrap().set_status(&job.id, JobStatus::Running, None, None);
                    let _ = app.emit("queue-changed", ());
                    let res = run_one(&mut studio, &job.id, &job.script, &job.options, true);
                    sync(&studio, &shared, &app);
                    {
                        let mut q = queue.lock().unwrap();
                        match &res {
                            Ok(dir) => {
                                let _ = q.set_status(&job.id, JobStatus::Done, None, Some(dir.clone()));
                            }
                            Err(e) if cancel.is_cancelled() => {
                                let _ = q.set_status(&job.id, JobStatus::Cancelled, Some("cancelled".into()), None);
                            }
                            Err(e) => {
                                let _ = q.set_status(&job.id, JobStatus::Failed, Some(friendly(e)), None);
                            }
                        }
                    }
                    let _ = app.emit("job-event", JobEvent {
                        id: job.id.clone(),
                        kind: if res.is_ok() { "done" } else if cancel.is_cancelled() { "cancelled" } else { "failed" },
                        project_dir: res.as_ref().ok().cloned(),
                        error: res.as_ref().err().map(friendly),
                        missing_packages: res.as_ref().err().map(missing_packages).unwrap_or_default(),
                    });
                    let _ = app.emit("queue-changed", ());
                    if cancel.is_cancelled() {
                        // Cancel stops the running job and pauses the queue.
                        shared.lock().unwrap().batch_running = false;
                    }
                }
                shared.lock().unwrap().batch_running = false;
                let _ = app.emit("queue-changed", ());
            }
        }
    }
    studio.unload_all();
}

fn init_logging(dir: &std::path::Path) -> tracing_appender::non_blocking::WorkerGuard {
    let _ = std::fs::create_dir_all(dir);
    let file = tracing_appender::rolling::daily(dir, "voice-studio.log");
    let (nb, guard) = tracing_appender::non_blocking(file);
    let filter = tracing_subscriber::EnvFilter::try_from_env("VS_LOG").unwrap_or_else(|_| "info,ort=warn".into());
    let _ = tracing_subscriber::fmt().with_writer(nb).with_ansi(false).with_env_filter(filter).try_init();
    guard
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let settings = Settings::load();
    let log_dir = settings.logs_dir();
    let guard = init_logging(&log_dir);
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}");
    }));
    tracing::info!("Voice Studio {} starting on {}", vs_core::APP_VERSION, vs_core::catalog::platform_id());
    let queue = Arc::new(Mutex::new(Queue::load(&settings.queue_path())));
    let shared = Arc::new(Mutex::new(Shared::default()));
    let cancel = Cancel::new();
    let (tx, rx) = mpsc::channel();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            settings: Mutex::new(settings.clone()),
            tx: Mutex::new(tx),
            cancel: cancel.clone(),
            shared: shared.clone(),
            queue: queue.clone(),
            downloads: Mutex::new(Default::default()),
            pending_ref: Mutex::new(None),
            log_dir,
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            let s = settings.clone();
            std::thread::Builder::new()
                .name("render-worker".into())
                .spawn(move || worker_loop(handle, rx, s, cancel, shared, queue))?;
            // Keep the log guard alive for the app lifetime.
            app.manage(Mutex::new(Some(guard)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_settings,
            commands::save_settings,
            commands::move_models_dir,
            commands::parse_script,
            commands::catalog,
            commands::install_package,
            commands::cancel_download,
            commands::remove_package,
            commands::verify_package,
            commands::import_package,
            commands::runtime_status,
            commands::install_runtime,
            commands::remove_runtime,
            commands::voices,
            commands::engine_status,
            commands::make,
            commands::cancel_render,
            commands::unload_models,
            commands::queue_list,
            commands::queue_add,
            commands::queue_start,
            commands::queue_pause,
            commands::queue_retry,
            commands::queue_cancel,
            commands::queue_remove,
            commands::queue_clear_finished,
            commands::projects,
            commands::project_get,
            commands::project_rename,
            commands::project_delete,
            commands::read_audio,
            commands::reveal,
            commands::open_folder,
            commands::export_audio,
            commands::reference_load,
            commands::reference_clip,
            commands::profile_save,
            commands::profile_rename,
            commands::profile_delete,
            commands::diagnostics,
            commands::clear_cache,
            commands::read_script_file,
            commands::engines,
            commands::install_engine,
            commands::remove_engine,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Voice Studio")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Stop any running job and let the worker drop engines (kills the Python
                // worker; the Job Object is a second line of defence on Windows).
                let st = app.state::<AppState>();
                st.cancel.cancel();
                let _ = st.tx.lock().unwrap().send(WorkerMsg::Unload);
                std::thread::sleep(Duration::from_millis(300));
            }
        });
}
