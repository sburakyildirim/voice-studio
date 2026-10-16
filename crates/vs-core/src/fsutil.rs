//! Filesystem helpers: atomic writes, safe path handling, checksums.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// Write bytes atomically: write to a sibling temp file, fsync, then rename over the target.
/// A crash mid-write leaves either the old file or the new one, never a torn file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("atomic_write: path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("create dir {}", dir.display()))?;
    let name = path.file_name().context("atomic_write: no file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{}.{}.tmp", name, uuid::Uuid::new_v4().simple()));
    {
        let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    replace_file(&tmp, path)
}

/// Serialize as pretty JSON and write atomically.
pub fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    atomic_write(path, &bytes)
}

/// Rename `from` over `to`, retrying briefly on Windows where AV scanners or a playing
/// audio element can hold the target open for a moment.
pub fn replace_file(from: &Path, to: &Path) -> Result<()> {
    let mut last = None;
    for attempt in 0..10 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(30 * (attempt + 1)));
            }
        }
    }
    let _ = fs::remove_file(from);
    Err(last.unwrap()).with_context(|| format!("rename {} -> {}", from.display(), to.display()))
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// Turn arbitrary user text into a safe single path component (no separators, no reserved
/// Windows names, no leading dots, bounded length).
pub fn sanitize_file_name(input: &str) -> String {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let mut out: String = input
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    out = out.trim().trim_matches('.').trim().to_string();
    // Collapse runs of whitespace/underscores.
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_sep = false;
    for c in out.chars() {
        let sep = c.is_whitespace() || c == '_';
        if sep {
            if !prev_sep {
                collapsed.push(if c == '_' { '_' } else { ' ' });
            }
        } else {
            collapsed.push(c);
        }
        prev_sep = sep;
    }
    let mut out: String = collapsed.chars().take(80).collect();
    out = out.trim().to_string();
    if out.is_empty() {
        out = "untitled".into();
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        out = format!("_{out}");
    }
    out
}

/// Join a user-controlled relative path onto `root`, refusing anything that would escape it
/// (absolute paths, `..`, drive prefixes).
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    let mut out = root.to_path_buf();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            _ => bail!("unsafe path component in {rel:?}"),
        }
    }
    if out == root {
        bail!("empty relative path");
    }
    Ok(out)
}

/// True if `child` is located inside `root` after canonicalization of the existing parts.
pub fn is_within(root: &Path, child: &Path) -> bool {
    let (Ok(r), Ok(c)) = (dunce_canon(root), dunce_canon(child)) else {
        return false;
    };
    c.starts_with(r)
}

fn dunce_canon(p: &Path) -> std::io::Result<PathBuf> {
    let c = fs::canonicalize(p)?;
    // Strip the Windows verbatim prefix so starts_with comparisons work on both sides.
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            if !rest.starts_with("UNC\\") {
                return Ok(PathBuf::from(rest));
            }
        }
    }
    Ok(c)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Total size of a directory tree in bytes (best effort).
pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = fs::read_dir(path) {
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => total += dir_size(&e.path()),
                Ok(_) => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                Err(_) => {}
            }
        }
    }
    total
}

/// Pick a non-existing path by appending " (2)", " (3)", ... before the extension.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("."));
    for i in 2.. {
        let cand = dir.join(format!("{stem} ({i}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_separators_and_reserved() {
        assert_eq!(sanitize_file_name("a/b\\c:d"), "a_b_c_d");
        assert_eq!(sanitize_file_name("  ..hidden.. "), "hidden");
        assert_eq!(sanitize_file_name("CON"), "_CON");
        assert_eq!(sanitize_file_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_file_name(""), "untitled");
        assert_eq!(sanitize_file_name("Şarkı — ünlü"), "Şarkı — ünlü");
        assert!(sanitize_file_name(&"x".repeat(500)).len() <= 80);
    }

    #[test]
    fn safe_join_rejects_escapes() {
        let root = Path::new("root");
        assert!(safe_join(root, "../x").is_err());
        assert!(safe_join(root, "a/../../x").is_err());
        assert!(safe_join(root, "").is_err());
        #[cfg(windows)]
        assert!(safe_join(root, "C:\\x").is_err());
        #[cfg(unix)]
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert_eq!(safe_join(root, "a/b.wav").unwrap(), root.join("a").join("b.wav"));
    }

    #[test]
    fn atomic_write_replaces() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let leftovers: Vec<_> = fs::read_dir(d.path()).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no temp files left behind");
    }

    #[test]
    fn unique_path_increments() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        assert_eq!(unique_path(&p), p);
        fs::write(&p, b"").unwrap();
        assert_eq!(unique_path(&p), d.path().join("a (2).wav"));
    }
}
