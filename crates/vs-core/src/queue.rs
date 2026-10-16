//! Persistent batch queue. Survives restarts: a job that was running when the app quit or
//! crashed is put back to pending and resumed (its finished segments come from the cache).

use crate::fsutil;
use crate::render::RenderOptions;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const QUEUE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub title: String,
    pub script: String,
    pub options: RenderOptions,
    pub status: JobStatus,
    pub attempts: u32,
    pub error: Option<String>,
    pub project_dir: Option<PathBuf>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    pub schema_version: u32,
    pub jobs: Vec<Job>,
    #[serde(skip)]
    path: PathBuf,
}

impl Queue {
    pub fn load(path: &Path) -> Self {
        let mut q: Queue = fsutil::read_json(path).unwrap_or(Queue { schema_version: QUEUE_SCHEMA, jobs: vec![], path: PathBuf::new() });
        q.path = path.to_path_buf();
        for j in q.jobs.iter_mut() {
            if j.status == JobStatus::Running {
                j.status = JobStatus::Pending; // interrupted: resume
            }
        }
        q
    }

    pub fn save(&self) -> Result<()> {
        fsutil::atomic_write_json(&self.path, self)
    }

    pub fn add(&mut self, title: &str, script: &str, options: RenderOptions) -> Result<String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let title = if title.trim().is_empty() { crate::render::auto_title(script) } else { title.trim().to_string() };
        self.jobs.push(Job {
            id: id.clone(),
            title,
            script: script.to_string(),
            options,
            status: JobStatus::Pending,
            attempts: 0,
            error: None,
            project_dir: None,
            created_at: chrono::Utc::now(),
            finished_at: None,
        });
        self.save()?;
        Ok(id)
    }

    pub fn next_pending(&self) -> Option<Job> {
        self.jobs.iter().find(|j| j.status == JobStatus::Pending).cloned()
    }

    pub fn set_status(&mut self, id: &str, status: JobStatus, error: Option<String>, project_dir: Option<PathBuf>) -> Result<()> {
        if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
            if status == JobStatus::Running {
                j.attempts += 1;
            }
            j.status = status;
            j.error = error;
            if project_dir.is_some() {
                j.project_dir = project_dir;
            }
            if matches!(status, JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled) {
                j.finished_at = Some(chrono::Utc::now());
            }
        }
        self.save()
    }

    pub fn retry(&mut self, id: &str) -> Result<()> {
        if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
            if j.status != JobStatus::Running {
                j.status = JobStatus::Pending;
                j.error = None;
            }
        }
        self.save()
    }

    pub fn retry_failed(&mut self) -> Result<()> {
        for j in self.jobs.iter_mut().filter(|j| matches!(j.status, JobStatus::Failed | JobStatus::Cancelled)) {
            j.status = JobStatus::Pending;
            j.error = None;
        }
        self.save()
    }

    pub fn cancel_pending(&mut self, id: Option<&str>) -> Result<()> {
        for j in self.jobs.iter_mut().filter(|j| j.status == JobStatus::Pending && id.is_none_or(|i| i == j.id)) {
            j.status = JobStatus::Cancelled;
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.jobs.retain(|j| j.id != id || j.status == JobStatus::Running);
        self.save()
    }

    pub fn clear_finished(&mut self) -> Result<()> {
        self.jobs.retain(|j| j.status != JobStatus::Done);
        self.save()
    }
}

/// Split a multi-script document into scripts. Scripts are separated by a line containing
/// only `===`; an optional first line `# Title` names the script.
pub fn split_batch(doc: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    fn flush(cur: &mut Vec<&str>, out: &mut Vec<(String, String)>) {
        let mut lines: Vec<&str> = std::mem::take(cur);
        while lines.first().is_some_and(|l| l.trim().is_empty()) {
            lines.remove(0);
        }
        let mut title = String::new();
        if let Some(first) = lines.first() {
            if let Some(t) = first.trim().strip_prefix("# ") {
                title = t.trim().to_string();
                lines.remove(0);
            }
        }
        let body = lines.join("\n").trim().to_string();
        if !body.is_empty() {
            out.push((title, body));
        }
    }
    for line in doc.lines() {
        if line.trim() == "===" {
            flush(&mut cur, &mut out);
        } else {
            cur.push(line);
        }
    }
    flush(&mut cur, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CloneMode;

    fn opts() -> RenderOptions {
        RenderOptions {
            title: String::new(),
            engine: "kokoro".into(),
            voice: "af_heart".into(),
            emotion_control: true,
            voice_clone: false,
            profile_id: None,
            clone_mode: CloneMode::VoiceOnly,
            seed: None,
            speed: 1.0,
            export_mp3: None,
            bypass_cache: false,
        }
    }

    #[test]
    fn queue_persists_and_resumes_running_jobs() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("queue.json");
        let mut q = Queue::load(&p);
        let a = q.add("A", "one", opts()).unwrap();
        let b = q.add("", "two words here", opts()).unwrap();
        q.set_status(&a, JobStatus::Running, None, None).unwrap();
        // Simulated crash: reload from disk.
        let mut q2 = Queue::load(&p);
        assert_eq!(q2.jobs.len(), 2);
        assert_eq!(q2.jobs[0].status, JobStatus::Pending, "running job resumes");
        assert_eq!(q2.jobs[0].attempts, 1);
        assert_eq!(q2.jobs[1].title, "two words here");
        q2.set_status(&a, JobStatus::Failed, Some("boom".into()), None).unwrap();
        assert_eq!(q2.next_pending().unwrap().id, b);
        q2.retry(&a).unwrap();
        assert_eq!(q2.next_pending().unwrap().id, a);
        q2.cancel_pending(None).unwrap();
        assert!(q2.next_pending().is_none());
        q2.retry_failed().unwrap();
        assert_eq!(q2.jobs.iter().filter(|j| j.status == JobStatus::Pending).count(), 2);
    }

    #[test]
    fn batch_split() {
        let doc = "# First\nHello there.\n===\n\n# Second\n\"Wow\"\n===\n\n===\nUntitled body";
        let s = split_batch(doc);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], ("First".into(), "Hello there.".into()));
        assert_eq!(s[1].0, "Second");
        assert_eq!(s[2], ("".into(), "Untitled body".into()));
    }
}
