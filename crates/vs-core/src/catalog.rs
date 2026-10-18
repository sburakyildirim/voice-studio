//! Model catalog (evaluated models) and installable packages (models, voices, runtimes).

use crate::download::{self, Cancel, ExtractRule, Progress};
use crate::fsutil;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PACKAGES_JSON: &str = include_str!("../../../catalog/packages.json");
pub const MODELS_JSON: &str = include_str!("../../../catalog/models.json");
const MANIFEST: &str = "package.json";
pub const MANIFEST_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSpec {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub dest: String,
    #[serde(default)]
    pub extract: Vec<ExtractRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub engine: String,
    #[serde(default)]
    pub note: Option<String>,
    pub license: String,
    #[serde(default)]
    pub platforms: Vec<String>,
    pub files: Vec<FileSpec>,
}

impl Package {
    pub fn download_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
    pub fn supports_this_platform(&self) -> bool {
        self.platforms.is_empty() || self.platforms.iter().any(|p| p == &platform_id())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackagesFile {
    schema_version: u32,
    packages: Vec<Package>,
}

pub fn platform_id() -> String {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        _ => "linux",
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

pub fn packages() -> Vec<Package> {
    let f: PackagesFile = serde_json::from_str(PACKAGES_JSON).expect("embedded packages.json is valid");
    f.packages
}

pub fn package(id: &str) -> Option<Package> {
    packages().into_iter().find(|p| p.id == id)
}

pub fn models_catalog() -> serde_json::Value {
    serde_json::from_str(MODELS_JSON).expect("embedded models.json is valid")
}

/// The ONNX Runtime package preferred on this platform (first match wins).
pub fn ort_package_candidates() -> Vec<String> {
    let mut v: Vec<String> = packages()
        .into_iter()
        .filter(|p| p.engine == "onnxruntime" && p.supports_this_platform())
        .map(|p| p.id)
        .collect();
    v.sort_by_key(|id| if id.contains("directml") { 0 } else { 1 });
    v
}

pub fn uv_package_id() -> String {
    format!("uv-{}", platform_id())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub id: String,
    pub installed_at: chrono::DateTime<chrono::Utc>,
    /// Source file hashes (as published upstream).
    pub sources: Vec<String>,
    pub files: Vec<InstalledFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageStatus {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub engine: String,
    pub license: String,
    pub note: Option<String>,
    pub installed: bool,
    pub partial_bytes: u64,
    pub download_size: u64,
    pub disk_size: u64,
    pub path: PathBuf,
    pub supported: bool,
}

pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join(fsutil::sanitize_file_name(id))
    }

    pub fn manifest(&self, id: &str) -> Option<Manifest> {
        fsutil::read_json(&self.dir(id).join(MANIFEST)).ok()
    }

    /// Installed = manifest present and every listed file exists with the recorded size.
    pub fn is_installed(&self, id: &str) -> bool {
        let Some(m) = self.manifest(id) else { return false };
        let dir = self.dir(id);
        m.files.iter().all(|f| fs_len(&dir.join(&f.path)) == Some(f.size))
    }

    pub fn file_path(&self, id: &str, rel: &str) -> PathBuf {
        self.dir(id).join(rel)
    }

    pub fn status(&self, p: &Package) -> PackageStatus {
        let dir = self.dir(&p.id);
        let installed = self.is_installed(&p.id);
        let partial = if installed { 0 } else { partial_bytes(&dir) };
        PackageStatus {
            id: p.id.clone(),
            title: p.title.clone(),
            kind: p.kind.clone(),
            engine: p.engine.clone(),
            license: p.license.clone(),
            note: p.note.clone(),
            installed,
            partial_bytes: partial,
            download_size: p.download_size(),
            disk_size: if dir.exists() { fsutil::dir_size(&dir) } else { 0 },
            path: dir,
            supported: p.supports_this_platform(),
        }
    }

    /// Download, verify, extract and record a package. Resumes partial downloads.
    pub fn install(&self, p: &Package, cancel: &Cancel, mut on_progress: impl FnMut(&str, Progress)) -> Result<()> {
        if !p.supports_this_platform() {
            bail!("{} is not available for {}", p.id, platform_id());
        }
        let dir = self.dir(&p.id);
        let dl = dir.join(".downloads");
        std::fs::create_dir_all(&dl)?;
        let total: u64 = p.download_size();
        let mut base = 0u64;
        let mut installed = Vec::new();
        for f in &p.files {
            let fname = fsutil::sanitize_file_name(&f.dest);
            let tmp = dl.join(&fname);
            let final_path = fsutil::safe_join(&dir, &f.dest)?;
            let already = f.extract.is_empty()
                && fs_len(&final_path) == Some(f.size)
                && (f.sha256.is_empty() || fsutil::sha256_file(&final_path)? == f.sha256);
            if !already {
                download::download(&f.url, &tmp, f.size, &f.sha256, cancel, |pr| {
                    on_progress(&f.dest, Progress { done: base + pr.done, total, bytes_per_sec: pr.bytes_per_sec })
                })?;
                if f.extract.is_empty() {
                    if let Some(parent) = final_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    fsutil::replace_file(&tmp, &final_path)?;
                } else {
                    download::extract(&tmp, &dir, &f.extract)?;
                    let _ = std::fs::remove_file(&tmp);
                }
            }
            if f.extract.is_empty() {
                installed.push(InstalledFile { path: f.dest.clone(), sha256: f.sha256.clone(), size: f.size });
            } else {
                for r in &f.extract {
                    let path = dir.join(&r.to);
                    installed.push(InstalledFile {
                        path: r.to.clone(),
                        sha256: fsutil::sha256_file(&path)?,
                        size: fs_len(&path).unwrap_or(0),
                    });
                }
            }
            base += f.size;
        }
        let _ = std::fs::remove_dir_all(&dl);
        let m = Manifest {
            schema_version: MANIFEST_SCHEMA,
            id: p.id.clone(),
            installed_at: chrono::Utc::now(),
            sources: p.files.iter().map(|f| f.sha256.clone()).collect(),
            files: installed,
        };
        fsutil::atomic_write_json(&dir.join(MANIFEST), &m)
    }

    /// Full integrity check (re-hash every file). Returns the list of bad files.
    pub fn verify(&self, id: &str) -> Result<Vec<String>> {
        let m = self.manifest(id).context("package not installed")?;
        let dir = self.dir(id);
        let mut bad = Vec::new();
        for f in &m.files {
            let p = dir.join(&f.path);
            if f.sha256.is_empty() {
                if fs_len(&p) != Some(f.size) {
                    bad.push(f.path.clone());
                }
                continue;
            }
            match fsutil::sha256_file(&p) {
                Ok(h) if h == f.sha256 => {}
                _ => bad.push(f.path.clone()),
            }
        }
        Ok(bad)
    }

    /// Remove a package directory. Refuses to touch anything outside the store root.
    pub fn remove(&self, id: &str) -> Result<()> {
        let dir = self.dir(id);
        if !dir.exists() {
            return Ok(());
        }
        if !fsutil::is_within(&self.root, &dir) || dir == self.root {
            bail!("refusing to remove {} (outside model folder)", dir.display());
        }
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))
    }

    /// Import files for a package from a local folder (or the package's own directory, for
    /// files placed there manually). Files are matched by SHA-256, so names don't matter.
    /// Only non-archive packages can be imported.
    pub fn import_from_dir(&self, p: &Package, src: &Path) -> Result<usize> {
        if p.files.iter().any(|f| !f.extract.is_empty()) {
            bail!("{} is an archive package; download it instead", p.id);
        }
        let dir = self.dir(&p.id);
        let mut by_size: std::collections::HashMap<u64, Vec<PathBuf>> = Default::default();
        for e in walk(src) {
            if let Some(l) = fs_len(&e) {
                by_size.entry(l).or_default().push(e);
            }
        }
        let mut found = 0;
        let mut files = Vec::new();
        for f in &p.files {
            let dest = fsutil::safe_join(&dir, &f.dest)?;
            let mut ok = false;
            for cand in by_size.get(&f.size).into_iter().flatten() {
                if fsutil::sha256_file(cand)? == f.sha256 {
                    if cand != &dest {
                        if let Some(parent) = dest.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::copy(cand, &dest)?;
                    }
                    ok = true;
                    break;
                }
            }
            if !ok {
                bail!("{}: no file matching {} ({} bytes, sha256 {}) in {}", p.id, f.dest, f.size, &f.sha256[..12], src.display());
            }
            found += 1;
            files.push(InstalledFile { path: f.dest.clone(), sha256: f.sha256.clone(), size: f.size });
        }
        let m = Manifest {
            schema_version: MANIFEST_SCHEMA,
            id: p.id.clone(),
            installed_at: chrono::Utc::now(),
            sources: p.files.iter().map(|f| f.sha256.clone()).collect(),
            files,
        };
        fsutil::atomic_write_json(&dir.join(MANIFEST), &m)?;
        Ok(found)
    }

    pub fn free_space(&self) -> Option<u64> {
        free_space(&self.root)
    }
}

fn fs_len(p: &Path) -> Option<u64> {
    std::fs::metadata(p).ok().filter(|m| m.is_file()).map(|m| m.len())
}

fn partial_bytes(dir: &Path) -> u64 {
    let dl = dir.join(".downloads");
    walk(&dl).iter().filter_map(|p| fs_len(p)).sum()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Free bytes on the disk that contains `path` (walks up to an existing ancestor).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    let canon = std::fs::canonicalize(&p).ok()?;
    let canon_s = canon.to_string_lossy().trim_start_matches(r"\\?\").to_lowercase();
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| canon_s.starts_with(&d.mount_point().to_string_lossy().to_lowercase()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_catalogs_parse() {
        let p = packages();
        assert!(p.iter().any(|p| p.id == "kokoro-82m-q8"));
        assert!(p.iter().all(|p| p.files.iter().all(|f| f.sha256.len() == 64 && f.size > 0)));
        let m = models_catalog();
        let n = m["models"].as_array().unwrap().len();
        assert!(n >= 20, "{n} models");
        // Every integrated model names an engine.
        for e in m["models"].as_array().unwrap() {
            if e["status"] == "integrated" {
                assert!(e["engine"].is_string());
            }
        }
    }
    #[test]
    fn ort_candidate_for_this_platform() {
        assert!(!ort_package_candidates().is_empty(), "{}", platform_id());
    }
    #[test]
    fn import_by_hash_and_remove() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::new(d.path().join("models"));
        let src = d.path().join("src");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("nested").join("whatever.bin"), b"hello").unwrap();
        let p = Package {
            id: "test-pkg".into(),
            title: "t".into(),
            kind: "model".into(),
            engine: "x".into(),
            note: None,
            license: "MIT".into(),
            platforms: vec![],
            files: vec![FileSpec { url: "http://invalid".into(), sha256: fsutil::sha256_bytes(b"hello"), size: 5, dest: "model.bin".into(), extract: vec![] }],
        };
        assert!(!store.is_installed("test-pkg"));
        assert_eq!(store.import_from_dir(&p, &src).unwrap(), 1);
        assert!(store.is_installed("test-pkg"));
        assert!(store.verify("test-pkg").unwrap().is_empty());
        std::fs::write(store.file_path("test-pkg", "model.bin"), b"HELLO").unwrap();
        assert_eq!(store.verify("test-pkg").unwrap(), vec!["model.bin".to_string()]);
        store.remove("test-pkg").unwrap();
        assert!(!store.dir("test-pkg").exists());
    }
}
