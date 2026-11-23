use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::Local;
use serde::Serialize;

use crate::upload::strip_verbatim;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: i64,
    pub path: String,
    pub filename: String,
    pub size: u64,
    pub upload_id: Option<String>,
    pub status: String,
    pub uploaded_chunks: u32,
    pub total_chunks: u32,
    pub server_progress: u32,
    pub error: Option<String>,
    pub video_id: Option<String>,
    pub slug: Option<String>,
    #[serde(default)]
    pub force_new: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_fingerprint: Option<String>,
    #[serde(default)]
    pub priority: bool,
    pub created_at: String,
    pub updated_at: String,
}

pub struct Queue {
    next_id: AtomicI64,
    jobs: Vec<Job>,
}

fn now() -> String {
    Local::now().to_rfc3339()
}

impl Queue {
    pub fn new() -> Self {
        Self {
            next_id: AtomicI64::new(1),
            jobs: Vec::new(),
        }
    }

    pub fn list_visible(&self) -> Vec<Job> {
        self.jobs
            .iter()
            .filter(|job| job.status != "dismissed")
            .cloned()
            .collect()
    }

    pub fn get(&self, id: i64) -> Option<Job> {
        self.jobs.iter().find(|job| job.id == id).cloned()
    }

    pub fn find_by_path(&self, path: &str) -> Option<Job> {
        self.jobs
            .iter()
            .filter(|job| job.status != "dismissed")
            .find(|job| job.path == path)
            .cloned()
    }

    pub fn next_active(&self) -> Option<Job> {
        fn status_rank(status: &str) -> u8 {
            match status {
                "uploading" => 0,
                "completing" => 1,
                "initializing" => 2,
                _ => 3,
            }
        }
        self.jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.status.as_str(),
                    "pending" | "initializing" | "uploading" | "completing"
                )
            })
            .min_by(|left, right| {
                left.priority
                    .cmp(&right.priority)
                    .reverse()
                    .then(status_rank(&left.status).cmp(&status_rank(&right.status)))
                    .then(left.id.cmp(&right.id))
            })
            .cloned()
    }

    pub fn has_priority_pending(&self) -> bool {
        self.jobs.iter().any(|job| {
            job.priority && matches!(job.status.as_str(), "pending" | "initializing" | "uploading")
        })
    }

    pub fn processing(&self) -> Vec<Job> {
        self.jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.status.as_str(),
                    "server_processing"
                        | "assembling"
                        | "uploaded"
                        | "converting"
                        | "processing"
                        | "moving"
                        | "restarting"
                ) || (job.status == "failed"
                    && job.upload_id.is_some()
                    && job.total_chunks > 0
                    && job.uploaded_chunks >= job.total_chunks
                    && job.error.as_deref() != Some("Cancelled"))
            })
            .cloned()
            .collect()
    }

    pub fn insert_pending(&mut self, path: &str, filename: &str, size: u64) -> i64 {
        let stamp = now();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.jobs.push(Job {
            id,
            path: path.into(),
            filename: filename.into(),
            size,
            upload_id: None,
            status: "pending".into(),
            uploaded_chunks: 0,
            total_chunks: 0,
            server_progress: 0,
            error: None,
            video_id: None,
            slug: None,
            force_new: false,
            source_fingerprint: None,
            priority: false,
            created_at: stamp.clone(),
            updated_at: stamp,
        });
        id
    }

    pub fn enqueue_resume(
        &mut self,
        path: &str,
        filename: &str,
        size: u64,
        upload_id: &str,
        total_chunks: u32,
        uploaded_chunks: u32,
        source_fingerprint: Option<String>,
    ) -> bool {
        if self.jobs.iter().any(|job| job.upload_id.as_deref() == Some(upload_id)) {
            return false;
        }
        if self.find_by_path(path).is_some() {
            return false;
        }
        let stamp = now();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.jobs.push(Job {
            id,
            path: path.into(),
            filename: filename.into(),
            size,
            upload_id: Some(upload_id.into()),
            status: "pending".into(),
            uploaded_chunks,
            total_chunks,
            server_progress: 0,
            error: None,
            video_id: None,
            slug: None,
            force_new: false,
            source_fingerprint,
            priority: true,
            created_at: stamp.clone(),
            updated_at: stamp,
        });
        true
    }

    pub fn enqueue_server_pipeline(
        &mut self,
        path: &str,
        filename: &str,
        size: u64,
        upload_id: &str,
        status: &str,
        total_chunks: u32,
        server_progress: u32,
        source_fingerprint: Option<String>,
        video_id: Option<String>,
        slug: Option<String>,
    ) -> bool {
        if self.jobs.iter().any(|job| job.upload_id.as_deref() == Some(upload_id)) {
            return false;
        }
        let stamp = now();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let ui_status = match status {
            "assembling" => "assembling",
            "uploaded" => "uploaded",
            "moving" => "moving",
            "restarting" => "restarting",
            "processing" => "processing",
            "trimming" | "thumbnailing" | "storyboarding" => "processing",
            _ => "converting",
        };
        self.jobs.push(Job {
            id,
            path: path.into(),
            filename: filename.into(),
            size,
            upload_id: Some(upload_id.into()),
            status: ui_status.into(),
            uploaded_chunks: total_chunks,
            total_chunks: total_chunks.max(1),
            server_progress,
            error: None,
            video_id,
            slug,
            force_new: false,
            source_fingerprint,
            priority: false,
            created_at: stamp.clone(),
            updated_at: stamp,
        });
        true
    }

    pub fn insert_missing_session(&mut self, filename: &str, size: u64, upload_id: &str) -> bool {
        if self.jobs.iter().any(|job| job.upload_id.as_deref() == Some(upload_id)) {
            return false;
        }
        let stamp = now();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.jobs.push(Job {
            id,
            path: format!("session:{upload_id}"),
            filename: filename.into(),
            size,
            upload_id: Some(upload_id.into()),
            status: "missing_file".into(),
            uploaded_chunks: 0,
            total_chunks: 0,
            server_progress: 0,
            error: Some("Local file not found for interrupted upload".into()),
            video_id: None,
            slug: None,
            force_new: false,
            source_fingerprint: None,
            priority: false,
            created_at: stamp.clone(),
            updated_at: stamp,
        });
        true
    }

    pub fn enqueue_new(&mut self, path: &str, filename: &str, size: u64) -> bool {
        self.enqueue_new_with_fp(path, filename, size, None)
    }

    pub fn enqueue_new_with_fp(
        &mut self,
        path: &str,
        filename: &str,
        size: u64,
        source_fingerprint: Option<String>,
    ) -> bool {
        if self.find_by_path(path).is_some() {
            return false;
        }
        let id = self.insert_pending(path, filename, size);
        if let Some(fp) = source_fingerprint {
            let mut job = self.get(id).expect("job just inserted");
            job.source_fingerprint = Some(fp);
            self.save(&job);
        }
        true
    }

    pub fn save(&mut self, job: &Job) {
        let Some(slot) = self.jobs.iter_mut().find(|item| item.id == job.id) else {
            return;
        };
        *slot = job.clone();
        slot.updated_at = now();
    }

    pub fn remove(&mut self, id: i64) {
        self.jobs.retain(|job| job.id != id);
    }

    pub fn retry_failed(&mut self) -> usize {
        let failed: Vec<i64> = self
            .jobs
            .iter()
            .filter(|job| job.status == "failed")
            .map(|job| job.id)
            .collect();
        let mut count = 0usize;
        for id in failed {
            let Some(mut job) = self.get(id) else {
                continue;
            };
            if Self::prepare_restart(&mut job).is_ok() {
                self.save(&job);
                count += 1;
            }
        }
        count
    }

    pub fn prepare_restart(job: &mut Job) -> Result<(), String> {
        let path = Path::new(&job.path);
        if !path.is_file() {
            return Err(format!("file not found: {}", job.path));
        }
        job.size = std::fs::metadata(path).map_err(|err| err.to_string())?.len();
        job.upload_id = None;
        job.uploaded_chunks = 0;
        job.total_chunks = 0;
        job.server_progress = 0;
        job.error = None;
        job.video_id = None;
        job.slug = None;
        job.status = "pending".into();
        job.force_new = true;
        job.source_fingerprint = None;
        Ok(())
    }
}

/// Быстрый ключ пути без `canonicalize` (для скана тысяч файлов).
pub fn path_key(path: &Path) -> String {
    strip_verbatim(path.to_string_lossy().as_ref())
}

pub fn normalize_path(path: &Path) -> String {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    strip_verbatim(&absolute.to_string_lossy())
}

pub fn filename_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "video.mp4".into())
}