//! Resource sampling for diagnostics and benchmarks (process RSS/CPU, GPU memory).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProcStats {
    pub pid: u32,
    pub rss_mb: f64,
    pub cpu_percent: f32,
}

/// Snapshot memory/CPU of this process (and optionally child workers). CPU% needs two
/// samples, so this sleeps for `window`.
pub fn sample(extra_pids: &[u32], window: Duration) -> Vec<ProcStats> {
    let mut sys = System::new();
    let mut pids: Vec<Pid> = vec![Pid::from_u32(std::process::id())];
    pids.extend(extra_pids.iter().map(|p| Pid::from_u32(*p)));
    let kind = ProcessRefreshKind::nothing().with_memory().with_cpu();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, kind);
    std::thread::sleep(window);
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, kind);
    pids.iter()
        .filter_map(|p| sys.process(*p))
        .map(|p| ProcStats { pid: p.pid().as_u32(), rss_mb: p.memory() as f64 / 1e6, cpu_percent: p.cpu_usage() })
        .collect()
}

/// GPU memory used on NVIDIA GPUs (MB), via nvidia-smi. None if unavailable.
pub fn nvidia_used_mb() -> Option<f64> {
    let out = crate::pyenv::no_window(
        std::process::Command::new("nvidia-smi").args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"]),
    )
    .stderr(std::process::Stdio::null())
    .output()
    .ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok()
}

pub fn gpu_names() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(o) = crate::pyenv::no_window(
        std::process::Command::new("nvidia-smi").args(["--query-gpu=name,memory.total", "--format=csv,noheader"]),
    )
    .stderr(std::process::Stdio::null())
    .output()
    {
        v.extend(String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.trim().is_empty()).map(|l| format!("NVIDIA {}", l.trim())));
    }
    #[cfg(target_os = "macos")]
    if let Ok(o) = std::process::Command::new("sysctl").args(["-n", "machdep.cpu.brand_string"]).output() {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if s.contains("Apple") {
            v.push(format!("{s} (integrated GPU, Metal)"));
        }
    }
    v
}

/// Samples peak RSS (this process + child workers) and GPU memory in the background.
pub struct PeakMonitor {
    stop: Arc<AtomicBool>,
    peaks: Arc<Mutex<(f64, f64)>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PeakMonitor {
    pub fn start(worker: Option<u32>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peaks = Arc::new(Mutex::new((0.0f64, 0.0f64)));
        let (s2, p2) = (stop.clone(), peaks.clone());
        let handle = std::thread::spawn(move || {
            let mut sys = System::new();
            let me = Pid::from_u32(std::process::id());
            let mut tick = 0u32;
            while !s2.load(Ordering::Relaxed) {
                sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_memory());
                // This process plus all descendants (the Chatterbox worker is a grandchild:
                // uv's venv launcher starts the real interpreter).
                let mut tree = vec![me];
                let mut i = 0;
                while i < tree.len() {
                    let parent = tree[i];
                    tree.extend(sys.processes().values().filter(|p| p.parent() == Some(parent)).map(|p| p.pid()));
                    i += 1;
                    if tree.len() > 64 {
                        break;
                    }
                }
                let _ = worker;
                let rss: u64 = tree.iter().filter_map(|p| sys.process(*p)).map(|p| p.memory()).sum();
                let vram = if tick % 10 == 0 { nvidia_used_mb().unwrap_or(0.0) } else { 0.0 };
                {
                    let mut pk = p2.lock().unwrap();
                    pk.0 = pk.0.max(rss as f64 / 1e6);
                    pk.1 = pk.1.max(vram);
                }
                tick += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        Self { stop, peaks, handle: Some(handle) }
    }

    /// Returns (peak RSS MB, peak total GPU memory used MB).
    pub fn stop(mut self) -> (Option<f64>, Option<f64>) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let p = *self.peaks.lock().unwrap();
        ((p.0 > 0.0).then_some(p.0), (p.1 > 0.0).then_some(p.1))
    }
}

pub fn system_summary() -> serde_json::Value {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let cpu = sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default();
    serde_json::json!({
        "os": System::long_os_version().unwrap_or_default(),
        "arch": std::env::consts::ARCH,
        "cpu": cpu,
        "logical_cpus": sys.cpus().len(),
        "physical_cpus": System::physical_core_count(),
        "total_ram_gb": sys.total_memory() as f64 / 1e9,
        "available_ram_gb": sys.available_memory() as f64 / 1e9,
        "gpus": gpu_names(),
    })
}
