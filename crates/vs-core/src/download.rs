//! Resumable, verified downloads and archive extraction.

use crate::fsutil;
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst)
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub bytes_per_sec: f64,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        // No limit on the whole body (multi-GB files); the response must start in time.
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .user_agent(concat!("VoiceStudio/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Download `url` to `dest`, resuming from `dest.part` when the server supports ranges.
/// The SHA-256 of the complete file must equal `sha256` (lowercase hex) or the file is
/// discarded. Returns Ok only once `dest` exists and is verified.
pub fn download(
    url: &str,
    dest: &Path,
    expected_size: u64,
    sha256: &str,
    cancel: &Cancel,
    mut on_progress: impl FnMut(Progress),
) -> Result<()> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    let part = dest.with_extension(format!(
        "{}.part",
        dest.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default()
    ));
    let mut attempt = 0;
    loop {
        attempt += 1;
        match try_download(url, &part, expected_size, cancel, &mut on_progress) {
            Ok(()) => break,
            Err(e) if cancel.is_cancelled() => return Err(e),
            Err(e) if attempt < 8 => {
                tracing::warn!("download attempt {attempt} failed: {e:#}; retrying");
                std::thread::sleep(Duration::from_secs(attempt * 2));
            }
            Err(e) => return Err(e),
        }
    }
    // Files without a published hash (small configs pinned by revision) are size-checked only.
    let got = if sha256.is_empty() { String::new() } else { fsutil::sha256_file(&part)? };
    if !sha256.is_empty() && !got.eq_ignore_ascii_case(sha256) {
        let _ = fs::remove_file(&part);
        bail!("checksum mismatch for {url}: expected {sha256}, got {got}");
    }
    fsutil::replace_file(&part, dest)
}

fn try_download(url: &str, part: &Path, expected: u64, cancel: &Cancel, on_progress: &mut impl FnMut(Progress)) -> Result<()> {
    let have = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if expected > 0 && have == expected {
        return Ok(());
    }
    if expected > 0 && have > expected {
        fs::remove_file(part)?;
    }
    let have = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    let mut req = agent().get(url);
    if have > 0 {
        req = req.header("Range", &format!("bytes={have}-"));
    }
    let resp = req.call().with_context(|| format!("GET {url}"))?;
    let status = resp.status().as_u16();
    let (mut file, mut done) = if status == 206 {
        let mut f = OpenOptions::new().append(true).open(part)?;
        f.seek(std::io::SeekFrom::End(0))?;
        (f, have)
    } else if status == 200 {
        (File::create(part)?, 0)
    } else {
        bail!("HTTP {status} for {url}");
    };
    let total = if expected > 0 {
        expected
    } else {
        resp.headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|l| l + done)
            .unwrap_or(0)
    };
    let mut body = resp.into_body();
    let mut reader = body.as_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let start = Instant::now();
    let start_done = done;
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    loop {
        if cancel.is_cancelled() {
            file.flush()?;
            bail!("cancelled");
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        if last_emit.elapsed() > Duration::from_millis(200) {
            let secs = start.elapsed().as_secs_f64().max(0.001);
            on_progress(Progress { done, total, bytes_per_sec: (done - start_done) as f64 / secs });
            last_emit = Instant::now();
        }
    }
    file.sync_all()?;
    on_progress(Progress { done, total, bytes_per_sec: 0.0 });
    if expected > 0 && done != expected {
        bail!("incomplete download: {done} of {expected} bytes");
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ExtractRule {
    /// Exact archive member path …
    #[serde(default)]
    pub member: Option<String>,
    /// … or any member whose path ends with this (e.g. "uv.exe").
    #[serde(default)]
    pub member_suffix: Option<String>,
    /// Output file name relative to the package directory.
    pub to: String,
}

/// Extract selected members from a .zip/.whl or .tgz/.tar.gz archive into `out_dir`.
pub fn extract(archive: &Path, out_dir: &Path, rules: &[ExtractRule]) -> Result<()> {
    let name = archive.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    let matches = |path: &str| -> Option<&ExtractRule> {
        let p = path.replace('\\', "/");
        rules.iter().find(|r| {
            r.member.as_deref() == Some(p.as_str())
                || r.member_suffix.as_deref().is_some_and(|s| p == s || p.ends_with(&format!("/{s}")))
        })
    };
    let mut found = 0;
    if name.ends_with(".zip") || name.ends_with(".whl") {
        let mut z = zip::ZipArchive::new(File::open(archive)?).context("open zip")?;
        for i in 0..z.len() {
            let mut f = z.by_index(i)?;
            if !f.is_file() {
                continue;
            }
            let fname = f.name().map(|n| n.to_string()).unwrap_or_default();
            if let Some(rule) = matches(&fname) {
                let dest = fsutil::safe_join(out_dir, &rule.to)?;
                write_stream(&mut f, &dest)?;
                found += 1;
            }
        }
    } else if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
        let gz = flate2::read::GzDecoder::new(File::open(archive)?);
        let mut t = tar::Archive::new(gz);
        for entry in t.entries()? {
            let mut e = entry?;
            let path = e.path()?.to_string_lossy().to_string();
            let path = path.trim_start_matches("./").to_string();
            if e.header().entry_type().is_symlink() {
                continue;
            }
            if let Some(rule) = matches(&path) {
                let dest = fsutil::safe_join(out_dir, &rule.to)?;
                write_stream(&mut e, &dest)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
                }
                found += 1;
            }
        }
    } else {
        bail!("unsupported archive type: {name}");
    }
    if found < rules.len() {
        bail!("archive {name} is missing expected files ({found}/{} found)", rules.len());
    }
    Ok(())
}

fn write_stream(r: &mut impl Read, dest: &Path) -> Result<()> {
    let dir = dest.parent().context("no parent")?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    {
        let mut out = File::create(&tmp)?;
        std::io::copy(r, &mut out)?;
        out.sync_all()?;
    }
    fsutil::replace_file(&tmp, dest)
}

/// Incremental SHA-256 for streams.
pub fn sha256_reader(mut r: impl Read) -> Result<String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// Minimal HTTP server supporting Range, which drops the connection after `cut` bytes on
    /// the first request to simulate an interrupted download.
    fn serve(data: Vec<u8>, cut: usize) -> (String, std::thread::JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            for (i, s) in l.incoming().take(3).enumerate() {
                let mut s = s.unwrap();
                let mut reader = BufReader::new(s.try_clone().unwrap());
                let mut range_start = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range_start = v.trim().trim_end_matches('-').parse().unwrap();
                    }
                }
                let body = &data[range_start..];
                let head = if range_start > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                        body.len(), range_start, data.len() - 1, data.len()
                    )
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                };
                s.write_all(head.as_bytes()).unwrap();
                if i == 0 {
                    let _ = s.write_all(&body[..cut]);
                    let _ = s.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                let _ = s.write_all(body);
            }
        });
        (format!("http://{addr}/file.bin"), h)
    }

    #[test]
    fn resumes_after_interruption_and_verifies() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let sha = fsutil::sha256_bytes(&data);
        let (url, _h) = serve(data.clone(), 100_000);
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("file.bin");
        let mut max_seen = 0;
        download(&url, &dest, data.len() as u64, &sha, &Cancel::new(), |p| max_seen = max_seen.max(p.done)).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        assert_eq!(max_seen, data.len() as u64);
    }

    #[test]
    fn checksum_mismatch_rejected() {
        let data = vec![7u8; 5000];
        let (url, _h) = serve(data.clone(), 5000);
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("file.bin");
        let err = download(&url, &dest, 5000, &"0".repeat(64), &Cancel::new(), |_| {}).unwrap_err();
        assert!(err.to_string().contains("checksum"), "{err}");
        assert!(!dest.exists());
    }

    #[test]
    fn extract_zip_members() {
        let d = tempfile::tempdir().unwrap();
        let zp = d.path().join("a.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&zp).unwrap());
            let o: zip::write::SimpleFileOptions = Default::default();
            w.start_file("pkg/capi/lib.dll", o).unwrap();
            w.write_all(b"dll").unwrap();
            w.start_file("other.txt", o).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let out = d.path().join("out");
        extract(&zp, &out, &[ExtractRule { member: Some("pkg/capi/lib.dll".into()), member_suffix: None, to: "lib.dll".into() }]).unwrap();
        assert_eq!(fs::read(out.join("lib.dll")).unwrap(), b"dll");
        assert!(!out.join("other.txt").exists());
        let bad = [ExtractRule { member: None, member_suffix: Some("missing".into()), to: "m".into() }];
        assert!(extract(&zp, &out, &bad).is_err());
        let evil = [ExtractRule { member: Some("other.txt".into()), member_suffix: None, to: "../evil".into() }];
        assert!(extract(&zp, &out, &evil).is_err());
    }
}
