//! Chatterbox via an isolated Python worker process (crash-isolated, killable, never
//! orphaned: on Windows it is bound to a kill-on-close Job Object; on all platforms it exits
//! when its stdin closes or the parent PID disappears).

use super::{runs_text, Capabilities, CloneMode, Engine, Fidelity, SegmentRequest};
use crate::audio::{self, AudioBuf};
use crate::catalog::Store;
use crate::download::Cancel;
use crate::pyenv::{self, PyRuntime};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SAMPLE_RATE: u32 = 24000;

pub struct ChatterboxPaths {
    pub ckpt_dir: PathBuf,
    pub runtime: PathBuf,
    pub tmp_dir: PathBuf,
    pub model_sha: String,
    /// Worker stderr is appended here (diagnostics).
    pub log_file: Option<PathBuf>,
}

impl ChatterboxPaths {
    pub fn resolve(store: &Store, runtimes_dir: &std::path::Path, tmp_dir: PathBuf) -> Result<Self> {
        if let Some(r) = pyenv::chatterbox_unsupported_reason() {
            bail!("{r}");
        }
        let mut missing = Vec::new();
        if !store.is_installed("chatterbox-en") {
            missing.push("chatterbox-en".to_string());
        }
        let rt = PyRuntime::chatterbox(runtimes_dir);
        if !rt.is_installed() {
            missing.push("chatterbox-runtime".to_string());
        }
        if !missing.is_empty() {
            bail!("MISSING_PACKAGES:{}", missing.join(","));
        }
        let model_sha = store
            .manifest("chatterbox-en")
            .map(|m| m.sources.iter().map(|s| &s[..8]).collect::<Vec<_>>().join(""))
            .unwrap_or_default();
        let log_file = Some(crate::settings::data_root().join("logs").join("chatterbox-worker.log"));
        Ok(Self { ckpt_dir: store.dir("chatterbox-en"), runtime: rt.dir, tmp_dir, model_sha, log_file })
    }
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    next_id: u64,
    #[cfg(windows)]
    _job: Option<win_job::Job>,
}

impl Worker {
    fn spawn(paths: &ChatterboxPaths) -> Result<Self> {
        let rt = PyRuntime { dir: paths.runtime.clone() };
        let script = rt.worker_script()?;
        let mut cmd = Command::new(rt.python());
        pyenv::no_window(&mut cmd)
            .arg("-u")
            .arg(&script)
            .current_dir(&paths.runtime)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUNBUFFERED", "1")
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .env("HF_HUB_DISABLE_TELEMETRY", "1")
            .env("TOKENIZERS_PARALLELISM", "false")
            // Apple Silicon: run the few ops MPS lacks on CPU instead of failing.
            .env("PYTORCH_ENABLE_MPS_FALLBACK", "1")
            // librosa's numba kernels are JIT-compiled on first import; keep the cache in a
            // stable writable place so it survives and is reused across runs.
            .env("NUMBA_CACHE_DIR", paths.runtime.join("numba-cache"))
            .env("VS_PARENT_PID", std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("start Chatterbox worker")?;
        #[cfg(windows)]
        let job = win_job::Job::assign(&child).map_err(|e| tracing::warn!("job object: {e}")).ok();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let t2 = tail.clone();
        let mut log = paths.log_file.as_ref().and_then(|p| {
            let _ = std::fs::create_dir_all(p.parent()?);
            std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
        });
        if let Some(f) = log.as_mut() {
            let _ = writeln!(f, "---- worker started {} ----", chrono::Utc::now().to_rfc3339());
        }
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                tracing::debug!(target: "chatterbox", "{line}");
                if let Some(f) = log.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
                let mut t = t2.lock().unwrap();
                t.push_back(line);
                if t.len() > 40 {
                    t.pop_front();
                }
            }
        });
        let mut w = Self {
            child,
            stdin,
            rx,
            stderr_tail: tail,
            next_id: 1,
            #[cfg(windows)]
            _job: job,
        };
        // Wait for the ready event (Python + torch import can take a while on cold disks).
        let ready = w.wait_line(Duration::from_secs(120), &Cancel::new())?;
        let v: Value = serde_json::from_str(&ready).context("worker handshake")?;
        if v["event"] != "ready" {
            bail!("unexpected worker handshake: {ready}");
        }
        Ok(w)
    }

    fn tail(&self) -> String {
        let t = self.raw_tail();
        if let Some(hint) = memory_hint(&t) {
            return format!("{hint}
{t}");
        }
        t
    }

    fn raw_tail(&self) -> String {
        self.stderr_tail.lock().unwrap().iter().rev().take(6).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    fn wait_line(&mut self, timeout: Duration, cancel: &Cancel) -> Result<String> {
        let start = Instant::now();
        loop {
            if cancel.is_cancelled() {
                bail!("cancelled");
            }
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(l) => return Ok(l),
                Err(RecvTimeoutError::Timeout) => {
                    if let Ok(Some(st)) = self.child.try_wait() {
                        bail!("Chatterbox worker exited ({st}). Last output:\n{}", self.tail());
                    }
                    if start.elapsed() > timeout {
                        bail!("Chatterbox worker timed out");
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(Duration::from_millis(200));
                    bail!("Chatterbox worker crashed. Last output:\n{}", self.tail());
                }
            }
        }
    }

    fn call(&mut self, mut req: Value, timeout: Duration, cancel: &Cancel) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        req["id"] = json!(id);
        writeln!(self.stdin, "{req}").context("write to worker")?;
        self.stdin.flush()?;
        loop {
            let line = self.wait_line(timeout, cancel)?;
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v["id"] != json!(id) {
                continue;
            }
            if v["ok"] == json!(true) {
                return Ok(v);
            }
            let mut msg = v["error"].as_str().unwrap_or("unknown error").to_string();
            if let Some(h) = memory_hint(&msg) {
                msg = format!("{h} ({msg})");
            }
            tracing::warn!("worker error: {msg}\n{}", v["trace"].as_str().unwrap_or(""));
            return Err(anyhow!("Chatterbox: {msg}"));
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "{}", json!({"cmd": "quit", "id": 0}));
        let _ = self.stdin.flush();
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.kill();
    }
}

pub struct Chatterbox {
    paths: ChatterboxPaths,
    worker: Option<Worker>,
    device: String,
    pub last_peak_vram: Option<u64>,
}

impl Chatterbox {
    pub fn new(paths: ChatterboxPaths) -> Self {
        Self { paths, worker: None, device: "unloaded".into(), last_peak_vram: None }
    }

    pub fn worker_pid(&self) -> Option<u32> {
        self.worker.as_ref().map(|w| w.child.id())
    }
}

impl Engine for Chatterbox {
    fn id(&self) -> &'static str {
        "chatterbox"
    }

    fn capabilities(&self) -> Capabilities {
        caps()
    }

    fn is_loaded(&self) -> bool {
        self.worker.is_some()
    }

    fn load(&mut self, device: &str, log: &mut dyn FnMut(String)) -> Result<()> {
        if self.worker.is_some() {
            return Ok(());
        }
        log("Starting isolated Chatterbox worker…".into());
        let mut w = Worker::spawn(&self.paths)?;
        log("Loading Chatterbox weights (first load can take 10–40 s)…".into());
        let r = w.call(
            json!({"cmd": "load", "ckpt_dir": self.paths.ckpt_dir, "device": device}),
            Duration::from_secs(600),
            &Cancel::new(),
        )?;
        self.device = match r["device"].as_str().unwrap_or("cpu") {
            "cuda" => format!("gpu:cuda ({})", r["gpu_name"].as_str().unwrap_or("NVIDIA")),
            "mps" => "gpu:mps".into(),
            d => d.to_string(),
        };
        log(format!("Chatterbox ready on {} in {:.1}s", self.device, r["load_secs"].as_f64().unwrap_or(0.0)));
        self.worker = Some(w);
        Ok(())
    }

    fn unload(&mut self) {
        // Dropping the worker ends the process and frees all RAM/VRAM.
        self.worker = None;
        self.device = "unloaded".into();
    }

    fn device(&self) -> String {
        self.device.clone()
    }

    fn model_fingerprint(&self) -> String {
        format!("chatterbox-en-{}:{}", pyenv::CHATTERBOX_VERSION, self.paths.model_sha)
    }

    fn synthesize(&mut self, req: &SegmentRequest, cancel: &Cancel) -> Result<AudioBuf> {
        let w = self.worker.as_mut().context("Chatterbox not loaded")?;
        std::fs::create_dir_all(&self.paths.tmp_dir)?;
        let out = self.paths.tmp_dir.join(format!("cb-{}.wav", uuid::Uuid::new_v4().simple()));
        let text = runs_text(req.runs, None);
        let r = w.call(
            json!({
                "cmd": "synth",
                "text": text,
                "ref": req.reference,
                "ckpt_dir": self.paths.ckpt_dir,
                "exaggeration": req.style.exaggeration,
                "cfg_weight": req.style.cfg_weight,
                "temperature": req.style.temperature,
                "seed": req.seed,
                "out": out,
            }),
            Duration::from_secs(900),
            cancel,
        );
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                // Cancel or crash: kill the process so no work continues in the background.
                if let Some(mut w) = self.worker.take() {
                    w.kill();
                }
                self.device = "unloaded".into();
                let _ = std::fs::remove_file(&out);
                return Err(e);
            }
        };
        self.last_peak_vram = r["peak_vram"].as_u64();
        if std::env::var_os("VS_TIMING").is_some() {
            eprintln!("
[chatterbox] {:.2}s audio in {:.2}s ({} chars)", r["secs"].as_f64().unwrap_or(0.0), r["gen_secs"].as_f64().unwrap_or(0.0), text.len());
        }
        let buf = audio::read_wav(&out);
        let _ = std::fs::remove_file(&out);
        buf
    }
}

/// Recognize out-of-memory failures (Windows commit limit, CUDA OOM, Linux OOM) so the user
/// gets an actionable message instead of a Python traceback.
pub fn memory_hint(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    if t.contains("os error 1455") || t.contains("paging file") || t.contains("disk belleği") {
        Some("Not enough memory to load Chatterbox (system commit limit reached). Close memory-heavy apps or enlarge the page file, then try again.")
    } else if t.contains("cuda out of memory") || t.contains("outofmemoryerror") {
        Some("The GPU ran out of memory. Close other GPU apps, or set Device to CPU in Options › Settings.")
    } else if t.contains("cannot allocate memory") || t.contains("memoryerror") {
        Some("Not enough RAM to run Chatterbox. Close other apps and try again.")
    } else {
        None
    }
}

pub fn caps() -> Capabilities {
    Capabilities {
        engine: "chatterbox".into(),
        cloning: true,
        clone_modes: vec![CloneMode::VoiceOnly, CloneMode::VoiceStyle],
        emotion: Fidelity::Genuine,
        emphasis: Fidelity::Unsupported,
        sample_rate: SAMPLE_RATE,
        notes: vec![
            "Zero-shot cloning from a 10–60 s reference. Similarity is not guaranteed and varies by reference quality.".into(),
            "Excited / calm use Chatterbox's real emotion-intensity control (exaggeration).".into(),
            "Mystery has no dedicated control: approximated with low intensity and longer pauses.".into(),
            "Identity and style are not separable in Chatterbox; Voice + Style keeps the reference's delivery and ignores markup intensity.".into(),
            "Emphasis (*text*) is not supported and is spoken normally.".into(),
            "Output carries Resemble AI's Perth imperceptible watermark.".into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn memory_hints() {
        assert!(super::memory_hint("OSError: ... (os error 1455)").is_some());
        assert!(super::memory_hint("torch.OutOfMemoryError: CUDA out of memory").is_some());
        assert!(super::memory_hint("ValueError: bad").is_none());
    }
}

#[cfg(windows)]
mod win_job {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::*;

    pub struct Job(HANDLE);
    unsafe impl Send for Job {}

    impl Job {
        /// Put the child in a job that is killed when the last handle (ours) closes, i.e.
        /// when Voice Studio exits for any reason, including a crash.
        pub fn assign(child: &std::process::Child) -> Result<Job, String> {
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return Err("CreateJobObjectW failed".into());
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    CloseHandle(h);
                    return Err("SetInformationJobObject failed".into());
                }
                if AssignProcessToJobObject(h, child.as_raw_handle() as HANDLE) == 0 {
                    CloseHandle(h);
                    return Err("AssignProcessToJobObject failed".into());
                }
                Ok(Job(h))
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
