//! Isolated Python runtime for optional PyTorch engines (Chatterbox), installed on demand
//! with uv into the app's runtimes folder. Nothing touches the system Python.

use crate::download::Cancel;
use crate::fsutil;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const WORKER_PY: &str = include_str!("../../../worker/chatterbox_worker.py");
pub const CHATTERBOX_VERSION: &str = "0.1.7";
pub const TORCH_VERSION: &str = "2.6.0";
pub const PYTHON_VERSION: &str = "3.11";
pub const SETUPTOOLS_PIN: &str = "setuptools<81";
const RUNTIME_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub schema_version: u32,
    pub python: String,
    pub torch: String,
    pub cuda: bool,
    pub mps: bool,
    pub cuda_device: Option<String>,
    pub chatterbox: String,
    pub installed_at: chrono::DateTime<chrono::Utc>,
}

/// Whether the Chatterbox runtime can exist on this platform. PyTorch 2.6 (pinned by
/// chatterbox-tts) publishes no Intel-macOS wheels, so Intel Macs are unsupported.
pub fn chatterbox_unsupported_reason() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("Chatterbox is not available on Intel Macs: PyTorch 2.6, which it requires, has no Intel-macOS build. Kokoro works normally; voice cloning needs an Apple Silicon Mac, Windows or Linux.")
    } else {
        None
    }
}

pub struct PyRuntime {
    pub dir: PathBuf,
}

/// Hide console windows for child processes on Windows.
pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

impl PyRuntime {
    pub fn chatterbox(runtimes_dir: &Path) -> Self {
        Self { dir: runtimes_dir.join("chatterbox") }
    }

    pub fn python(&self) -> PathBuf {
        if cfg!(windows) {
            self.dir.join("venv").join("Scripts").join("python.exe")
        } else {
            self.dir.join("venv").join("bin").join("python")
        }
    }

    fn info_path(&self) -> PathBuf {
        self.dir.join("runtime.json")
    }

    pub fn info(&self) -> Option<RuntimeInfo> {
        fsutil::read_json(&self.info_path()).ok().filter(|_: &RuntimeInfo| self.python().exists())
    }

    pub fn is_installed(&self) -> bool {
        self.info().is_some()
    }

    /// Write the embedded worker script next to the venv (refreshed when it changes).
    pub fn worker_script(&self) -> Result<PathBuf> {
        let p = self.dir.join("chatterbox_worker.py");
        let current = std::fs::read_to_string(&p).unwrap_or_default();
        if current != WORKER_PY {
            fsutil::atomic_write(&p, WORKER_PY.as_bytes())?;
        }
        Ok(p)
    }

    /// Run an import check inside the venv; on success records runtime.json.
    pub fn probe(&self) -> Result<RuntimeInfo> {
        let py = self.python();
        if !py.exists() {
            bail!("Python runtime not installed");
        }
        let code = "import json,sys,torch,importlib.metadata as m;import chatterbox.tts,perth;assert perth.PerthImplicitWatermarker,'watermarker unavailable';\
print(json.dumps({'python':sys.version.split()[0],'torch':torch.__version__,'cuda':torch.cuda.is_available(),\
'mps':bool(getattr(torch.backends,'mps',None) and torch.backends.mps.is_available()),\
'cuda_device':torch.cuda.get_device_name(0) if torch.cuda.is_available() else None,\
'chatterbox':m.version('chatterbox-tts')}))";
        let out = no_window(Command::new(&py).arg("-c").arg(code))
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::null())
            .output()
            .context("run python")?;
        if !out.status.success() {
            bail!("runtime check failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or(""));
        }
        let line = String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or("").to_string();
        let v: serde_json::Value = serde_json::from_str(&line).context("parse probe output")?;
        let info = RuntimeInfo {
            schema_version: RUNTIME_SCHEMA,
            python: v["python"].as_str().unwrap_or("").into(),
            torch: v["torch"].as_str().unwrap_or("").into(),
            cuda: v["cuda"].as_bool().unwrap_or(false),
            mps: v["mps"].as_bool().unwrap_or(false),
            cuda_device: v["cuda_device"].as_str().map(String::from),
            chatterbox: v["chatterbox"].as_str().unwrap_or("").into(),
            installed_at: chrono::Utc::now(),
        };
        fsutil::atomic_write_json(&self.info_path(), &info)?;
        Ok(info)
    }

    /// Which PyTorch wheel index to use on this machine.
    pub fn torch_index() -> Option<&'static str> {
        if cfg!(target_os = "macos") {
            return None; // PyPI wheels include MPS on Apple Silicon, CPU on Intel.
        }
        if has_nvidia_gpu() {
            Some("https://download.pytorch.org/whl/cu124")
        } else {
            Some("https://download.pytorch.org/whl/cpu")
        }
    }

    /// Create the venv and install pinned packages. Streams uv output to `log`.
    pub fn install(&self, uv: &Path, cancel: &Cancel, log: &mut dyn FnMut(String)) -> Result<RuntimeInfo> {
        if let Some(r) = chatterbox_unsupported_reason() {
            bail!("{r}");
        }
        std::fs::create_dir_all(&self.dir)?;
        let venv = self.dir.join("venv");
        let cache = self.dir.join("uv-cache");
        let py_dir = self.dir.join("python");
        let run = |args: &[&str], log: &mut dyn FnMut(String)| -> Result<()> {
            log(format!("uv {}", args.join(" ")));
            let mut child = no_window(Command::new(uv).args(args))
                .env("UV_CACHE_DIR", &cache)
                .env("UV_PYTHON_INSTALL_DIR", &py_dir)
                .env("UV_NO_CONFIG", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("start {}", uv.display()))?;
            let stderr = child.stderr.take().unwrap();
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            let tx2 = tx.clone();
            let stdout = child.stdout.take().unwrap();
            std::thread::spawn(move || BufReader::new(stderr).lines().map_while(Result::ok).for_each(|l| { let _ = tx.send(l); }));
            std::thread::spawn(move || BufReader::new(stdout).lines().map_while(Result::ok).for_each(|l| { let _ = tx2.send(l); }));
            loop {
                while let Ok(l) = rx.try_recv() {
                    log(l);
                }
                if cancel.is_cancelled() {
                    let _ = child.kill();
                    bail!("cancelled");
                }
                if let Some(st) = child.try_wait()? {
                    while let Ok(l) = rx.try_recv() {
                        log(l);
                    }
                    if !st.success() {
                        bail!("uv failed ({st})");
                    }
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        };
        let venv_s = venv.to_string_lossy().to_string();
        if !self.python().exists() {
            run(&["venv", "--python", PYTHON_VERSION, "--python-preference", "only-managed", &venv_s], log)?;
        }
        let py = self.python().to_string_lossy().to_string();
        let torch = format!("torch=={TORCH_VERSION}");
        let torchaudio = format!("torchaudio=={TORCH_VERSION}");
        let mut args = vec!["pip", "install", "--python", &py, &torch, &torchaudio];
        if let Some(idx) = Self::torch_index() {
            args.push("--index-url");
            args.push(idx);
        }
        run(&args, log)?;
        let cb = format!("chatterbox-tts=={CHATTERBOX_VERSION}");
        // resemble-perth (the output watermarker) still imports pkg_resources, which uv venvs
        // do not ship and setuptools>=81 removed.
        run(&["pip", "install", "--python", &py, &cb, SETUPTOOLS_PIN], log)?;
        log("Verifying runtime…".into());
        let info = self.probe()?;
        log(format!("Runtime ready: Python {}, torch {}, CUDA {}", info.python, info.torch, info.cuda));
        Ok(info)
    }

    /// Install a generic engine runtime from its spec. The uv cache and managed Pythons are
    /// shared by all runtimes (siblings of this folder), so on one drive PyTorch and other
    /// large wheels are stored once and hard-linked into each venv.
    pub fn install_spec(
        &self,
        uv: &Path,
        spec: &crate::engine::python::RuntimeSpec,
        label: &str,
        cancel: &Cancel,
        log: &mut dyn FnMut(String),
    ) -> Result<RuntimeInfo> {
        std::fs::create_dir_all(&self.dir)?;
        let root = self.dir.parent().map(Path::to_path_buf).unwrap_or_else(|| self.dir.clone());
        let cache = root.join("uv-cache");
        let py_dir = root.join("python");
        let venv = self.dir.join("venv");
        let run = |args: &[String], log: &mut dyn FnMut(String)| -> Result<()> {
            log(format!("uv {}", args.join(" ")));
            let mut child = no_window(Command::new(uv).args(args))
                .env("UV_CACHE_DIR", &cache)
                .env("UV_PYTHON_INSTALL_DIR", &py_dir)
                .env("UV_NO_CONFIG", "1")
                .env("UV_LINK_MODE", "hardlink")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("start {}", uv.display()))?;
            let stderr = child.stderr.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            let tx2 = tx.clone();
            std::thread::spawn(move || BufReader::new(stderr).lines().map_while(Result::ok).for_each(|l| { let _ = tx.send(l); }));
            std::thread::spawn(move || BufReader::new(stdout).lines().map_while(Result::ok).for_each(|l| { let _ = tx2.send(l); }));
            let mut tail: Vec<String> = Vec::new();
            loop {
                while let Ok(l) = rx.try_recv() {
                    tail.push(l.clone());
                    if tail.len() > 12 {
                        tail.remove(0);
                    }
                    log(l);
                }
                if cancel.is_cancelled() {
                    let _ = child.kill();
                    bail!("cancelled");
                }
                if let Some(st) = child.try_wait()? {
                    while let Ok(l) = rx.try_recv() {
                        tail.push(l.clone());
                        log(l);
                    }
                    if !st.success() {
                        bail!("uv failed ({st}):\n{}", tail.join("\n"));
                    }
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        };
        let s = |x: &str| x.to_string();
        if !self.python().exists() {
            run(&[s("venv"), s("--python"), spec.python.clone(), s("--python-preference"), s("only-managed"), venv.to_string_lossy().to_string()], log)?;
        }
        let py = self.python().to_string_lossy().to_string();
        if let Some(t) = &spec.torch {
            // torchaudio is always pinned to the same release: many audio packages pull it in
            // and a newer build fails to load against an older torch (WinError 127).
            let mut args = vec![s("pip"), s("install"), s("--python"), py.clone(), format!("torch=={t}"), format!("torchaudio=={t}")];
            if let Some(idx) = Self::torch_index() {
                args.push(s("--index-url"));
                args.push(s(idx));
            }
            run(&args, log)?;
        }
        let mut args = vec![s("pip"), s("install"), s("--python"), py.clone()];
        args.extend(spec.packages.iter().cloned());
        args.extend(spec.pip_args.iter().cloned());
        if let Some(t) = &spec.torch {
            // Keep the machine-specific torch build that was just installed.
            args.push(format!("torch=={t}"));
            args.push(format!("torchaudio=={t}"));
            if let Some(idx) = Self::torch_index() {
                args.push(s("--extra-index-url"));
                args.push(s(idx));
                args.push(s("--index-strategy"));
                args.push(s("unsafe-best-match"));
            }
        }
        run(&args, log)?;
        if let Some(code) = &spec.post_install {
            log("Applying post-install fixes…".into());
            let out = no_window(Command::new(&py).arg("-c").arg(code))
                .env("PYTHONIOENCODING", "utf-8")
                .stdin(Stdio::null())
                .output()
                .context("run post-install step")?;
            if !out.status.success() {
                bail!("{label} post-install step failed: {}", String::from_utf8_lossy(&out.stderr).lines().rev().take(3).collect::<Vec<_>>().join(" | "));
            }
        }
        log("Verifying runtime…".into());
        let info = self.probe_generic(&spec.probe, label)?;
        log(format!("Runtime ready: Python {}{}", info.python, if info.torch.is_empty() { String::new() } else { format!(", torch {}, CUDA {}", info.torch, info.cuda) }));
        Ok(info)
    }

    /// Import check for a generic runtime; records runtime.json on success.
    pub fn probe_generic(&self, probe: &str, label: &str) -> Result<RuntimeInfo> {
        let py = self.python();
        let code = format!(
            "import json,sys\n{probe}\ntry:\n import torch;t=torch.__version__;c=torch.cuda.is_available();m=bool(getattr(torch.backends,'mps',None) and torch.backends.mps.is_available());d=torch.cuda.get_device_name(0) if c else None\nexcept Exception:\n t='';c=False;m=False;d=None\nprint(json.dumps({{'python':sys.version.split()[0],'torch':t,'cuda':c,'mps':m,'cuda_device':d}}))"
        );
        let out = no_window(Command::new(&py).arg("-c").arg(code))
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::null())
            .output()
            .context("run python")?;
        if !out.status.success() {
            bail!("{label} runtime check failed: {}", String::from_utf8_lossy(&out.stderr).lines().rev().take(3).collect::<Vec<_>>().join(" | "));
        }
        let line = String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or("").to_string();
        let v: serde_json::Value = serde_json::from_str(&line).context("parse probe output")?;
        let info = RuntimeInfo {
            schema_version: RUNTIME_SCHEMA,
            python: v["python"].as_str().unwrap_or("").into(),
            torch: v["torch"].as_str().unwrap_or("").into(),
            cuda: v["cuda"].as_bool().unwrap_or(false),
            mps: v["mps"].as_bool().unwrap_or(false),
            cuda_device: v["cuda_device"].as_str().map(String::from),
            chatterbox: label.into(),
            installed_at: chrono::Utc::now(),
        };
        fsutil::atomic_write_json(&self.info_path(), &info)?;
        Ok(info)
    }

    pub fn remove(&self) -> Result<()> {
        if self.dir.exists() {
            std::fs::remove_dir_all(&self.dir).with_context(|| format!("remove {}", self.dir.display()))?;
        }
        Ok(())
    }
}

pub fn has_nvidia_gpu() -> bool {
    no_window(Command::new("nvidia-smi").arg("-L"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("GPU"))
        .unwrap_or(false)
}

/// Find uv: explicit env var, the installed package, then PATH.
pub fn find_uv(store: &crate::catalog::Store) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VOICE_STUDIO_UV") {
        return Some(PathBuf::from(p));
    }
    let exe = if cfg!(windows) { "uv.exe" } else { "uv" };
    let id = crate::catalog::uv_package_id();
    if store.is_installed(&id) {
        let p = store.file_path(&id, exe);
        if p.exists() {
            return Some(p);
        }
    }
    which(exe)
}

pub fn which(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(exe)).find(|p| p.is_file())
}
