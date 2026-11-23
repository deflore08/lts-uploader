use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;


use reqwest::multipart;
use serde::Deserialize;
use serde_json::Value;

pub const CHUNK_SIZE: u64 = 5 * 1024 * 1024;
pub const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024 * 1024;

#[derive(Debug)]
pub enum ApiError {
    Unauthorized(String),
    Retryable(String),
    Fatal(String),
    /// Файл уже есть в каталоге (409 duplicate / removed_duplicate).
    Duplicate(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized(msg)
            | ApiError::Retryable(msg)
            | ApiError::Fatal(msg)
            | ApiError::Duplicate(msg) => write!(f, "{msg}"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FingerprintSet {
    pub revision: u64,
    pub hashes: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct InitResult {
    pub upload_id: String,
    pub resumed: bool,
    pub missing_chunks: Vec<u32>,
    pub chunk_size: u64,
    pub total_chunks: u32,
    pub chunks_on_disk: Vec<u32>,
    pub http_status: u16,
}

impl std::error::Error for ApiError {}

#[derive(Debug, Clone)]
pub struct UserInfo {
    pub username: String,
    pub role: String,
}

#[derive(Debug)]
pub struct LoginResult {
    pub token: String,
    pub user: UserInfo,
}

#[derive(Debug, Clone)]
pub struct ChunkProgress {
    pub upload_id: String,
    pub phase: &'static str,
    pub uploaded_chunks: u32,
    pub total_chunks: u32,
    pub server_progress: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvKind {
    Running,
    Done,
    Failed,
    Missing,
}

#[derive(Debug, Clone)]
pub struct ConvUpdate {
    pub kind: ConvKind,
    pub progress: u32,
    pub video_id: Option<String>,
    pub slug: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub id: String,
    pub filename: String,
    pub original_name: Option<String>,
    pub size: u64,
    pub source_fingerprint: Option<String>,
    pub chunk_size: u64,
    pub total_chunks: u32,
    pub missing_chunks: Vec<u32>,
    pub progress: u32,
    pub needs_client: bool,
    pub status: String,
}

#[derive(Debug)]
pub(crate) struct UploadInfo {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) total_chunks: u32,
    pub(crate) chunk_size: u64,
    pub(crate) size: u64,
    pub(crate) missing_chunks: Vec<u32>,
    pub(crate) chunks_on_disk: Vec<u32>,
    pub(crate) error: Option<String>,
    pub(crate) assembly_progress: u32,
    pub(crate) file_hash: Option<String>,
    pub(crate) filename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UserBody {
    username: String,
    #[serde(default)]
    role: String,
}

pub fn http_client() -> Result<reqwest::Client, ApiError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    reqwest::Client::builder()
        .user_agent("lts-uploader/0.6")
        .connect_timeout(Duration::from_secs(15))
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|err| ApiError::Fatal(err.to_string()))
}

pub fn prefix_from_username(username: &str) -> String {
    let cleaned: String = username
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .collect();
    let cleaned = cleaned.to_ascii_lowercase();
    if cleaned.is_empty() {
        String::new()
    } else if cleaned.ends_with('_') {
        cleaned
    } else {
        format!("{cleaned}_")
    }
}

pub fn apply_prefix(filename: &str, prefix: &str) -> String {
    let prefix = prefix.trim();
    if prefix.is_empty()
        || filename
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    {
        filename.to_string()
    } else {
        format!("{prefix}{filename}")
    }
}

pub fn chunk_count(size: u64, chunk_size: u64) -> u32 {
    if size == 0 || chunk_size == 0 {
        0
    } else {
        ((size + chunk_size - 1) / chunk_size) as u32
    }
}

pub fn has_video_ext(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name.starts_with('.')
        || name.ends_with(".part")
        || name.ends_with(".tmp")
        || name.ends_with(".crdownload")
    {
        return false;
    }
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mp4" | "avi" | "mov" | "mkv" | "webm" | "flv" | "wmv"
    )
}

pub fn strip_verbatim(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text.to_string()
    }
}

pub fn effective_missing(total: u32, missing: &[u32], on_disk: &[u32]) -> Vec<u32> {
    if !missing.is_empty() {
        let mut items: Vec<u32> = missing.iter().copied().filter(|idx| *idx < total).collect();
        items.sort_unstable();
        items.dedup();
        return items;
    }
    if !on_disk.is_empty() || total == 0 {
        return Vec::new();
    }
    (0..total).collect()
}

pub async fn login(
    http: &reqwest::Client,
    base: &str,
    username: &str,
    password: &str,
) -> Result<LoginResult, ApiError> {
    let value = success_json(
        http.post(format!("{base}/api/auth/login"))
            .json(&serde_json::json!({
                "username": username,
                "password": password,
            })),
        Duration::from_secs(30),
    )
    .await?;
    let token = value
        .get("token")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    if token.is_empty() {
        return Err(ApiError::Fatal(format_server_error(200, &value.to_string())));
    }
    let user = parse_user(value.get("user"))?;
    Ok(LoginResult { token, user })
}

#[derive(Debug, Clone)]
pub struct DuplicateProbe {
    pub source_fingerprint: String,
    pub size_bytes: u64,
}

const CHECK_DUPLICATES_MAX: usize = 500;

/// `POST /api/uploads/check-duplicates` — локальные fp → сервер отдаёт дубли (спека §4.1).
pub async fn check_duplicates_on_server(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    items: &[DuplicateProbe],
) -> Result<HashSet<String>, ApiError> {
    if items.is_empty() {
        return Ok(HashSet::new());
    }
    if items.len() > CHECK_DUPLICATES_MAX {
        return Err(ApiError::Fatal(format!(
            "check-duplicates: больше {CHECK_DUPLICATES_MAX} элементов"
        )));
    }
    let fingerprints: Vec<String> = items
        .iter()
        .map(|item| item.source_fingerprint.trim().to_ascii_lowercase())
        .filter(|fp| fp.len() == 64)
        .collect();
    if fingerprints.is_empty() {
        return Ok(HashSet::new());
    }
    let response = http
        .post(format!("{base}/api/uploads/check-duplicates"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "fingerprints": fingerprints }))
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(classify_reqwest)?;
    if response.status().as_u16() == 409 {
        return Ok(items
            .iter()
            .map(|item| item.source_fingerprint.trim().to_ascii_lowercase())
            .collect());
    }
    if !response.status().is_success() {
        return Err(error_from_status(response).await);
    }
    let value = parse_json(response).await?;
    Ok(duplicate_fps_from_response(&value))
}

fn duplicate_fps_from_response(value: &Value) -> HashSet<String> {
    let mut out = HashSet::new();
    let insert_fp = |out: &mut HashSet<String>, raw: &str| {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.len() == 64 {
            out.insert(normalized);
        }
    };
    if let Some(arr) = value
        .get("duplicateFingerprints")
        .and_then(|v| v.as_array())
    {
        for item in arr {
            if let Some(fp) = item.as_str() {
                insert_fp(&mut out, fp);
            }
        }
    }
    for key in ["duplicates", "results", "entries", "present", "matches"] {
        let Some(arr) = value.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for item in arr {
            if let Some(fp) = item.as_str() {
                insert_fp(&mut out, fp);
                continue;
            }
            let Some(obj) = item.as_object() else {
                continue;
            };
            let dup = obj
                .get("duplicate")
                .and_then(|v| v.as_bool())
                .or_else(|| obj.get("isDuplicate").and_then(|v| v.as_bool()))
                .unwrap_or_else(|| key == "duplicates");
            if !dup {
                continue;
            }
            if let Some(fp) = obj
                .get("sourceFingerprint")
                .or(obj.get("fingerprint"))
                .or(obj.get("f"))
                .and_then(|v| v.as_str())
            {
                insert_fp(&mut out, fp);
            }
        }
    }
    out
}

pub async fn me(http: &reqwest::Client, base: &str, token: &str) -> Result<UserInfo, ApiError> {
    let value = success_json(
        http.get(format!("{base}/api/auth/me")).bearer_auth(token),
        Duration::from_secs(20),
    )
    .await?;
    parse_user(value.get("user"))
}

pub async fn upload_file<F>(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    path: &Path,
    remote_name: &str,
    chunk_size: u64,
    known_fingerprint: Option<&str>,
    force_new: bool,
    continue_upload_id: Option<&str>,
    mut on_progress: F,
) -> Result<String, ApiError>
where
    F: FnMut(ChunkProgress),
{
    if let Some(upload_id) = continue_upload_id.filter(|id| !id.is_empty()) {
        return continue_upload(
            http,
            base,
            token,
            path,
            upload_id,
            known_fingerprint,
            &mut on_progress,
        )
        .await;
    }
    let file_size = std::fs::metadata(path)
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ApiError::Fatal(format!("file not found: {}", path.display()))
            } else {
                ApiError::Retryable(format!("could not read the file: {err}"))
            }
        })?
        .len();
    if file_size == 0 {
        return Err(ApiError::Fatal("empty file".into()));
    }
    if file_size > MAX_FILE_SIZE {
        return Err(ApiError::Fatal("file is over 10 GB — the server will reject it".into()));
    }
    if chunk_size == 0 || chunk_size > 100 * 1024 * 1024 {
        return Err(ApiError::Fatal("invalid chunk size".into()));
    }

    let mut use_chunk = chunk_size;
    let mut total = chunk_count(file_size, use_chunk);
    if total == 0 {
        return Err(ApiError::Fatal("could not count chunks".into()));
    }

    let source_fp = match known_fingerprint {
        Some(value) => value.to_string(),
        None => {
            let path_owned = path.to_path_buf();
            tokio::task::spawn_blocking(move || {
                crate::fingerprint::source_fingerprint(&path_owned)
            })
            .await
            .map_err(|err| ApiError::Fatal(err.to_string()))?
            .map_err(ApiError::Fatal)?
        }
    };

    on_progress(ChunkProgress {
        upload_id: String::new(),
        phase: "initializing",
        uploaded_chunks: 0,
        total_chunks: total,
        server_progress: 0,
    });

    eprintln!("[lts] calling init for {remote_name} ({file_size} bytes, forceNew={force_new})");
    let init = init_upload(
        http,
        base,
        token,
        remote_name,
        total,
        file_size,
        use_chunk,
        &source_fp,
        force_new,
    )
    .await?;
    if init.chunk_size > 0 {
        use_chunk = init.chunk_size;
    }
    total = if init.total_chunks > 0 {
        init.total_chunks
    } else {
        chunk_count(file_size, use_chunk)
    };
    let missing = if init.resumed {
        effective_missing(total, &init.missing_chunks, &init.chunks_on_disk)
    } else {
        (0..total).collect()
    };
    eprintln!(
        "[lts] init {}: resumed={} missingChunks={} onDisk={} totalChunks={}",
        init.http_status,
        init.resumed,
        init.missing_chunks.len(),
        init.chunks_on_disk.len(),
        total
    );
    let upload_id = init.upload_id;

    let mut done = total.saturating_sub(missing.len() as u32);
    on_progress(ChunkProgress {
        upload_id: upload_id.clone(),
        phase: "uploading",
        uploaded_chunks: done,
        total_chunks: total,
        server_progress: 0,
    });

    for index in missing {
        send_chunk_retry(http, base, token, path, &upload_id, index, use_chunk, file_size).await?;
        done = done.saturating_add(1);
        on_progress(ChunkProgress {
            upload_id: upload_id.clone(),
            phase: "uploading",
            uploaded_chunks: done,
            total_chunks: total,
            server_progress: 0,
        });
    }

    on_progress(ChunkProgress {
        upload_id: upload_id.clone(),
        phase: "assembling",
        uploaded_chunks: total,
        total_chunks: total,
        server_progress: 0,
    });

    let poll_http = http.clone();
    let poll_base = base.to_string();
    let poll_token = token.to_string();
    let poll_id = upload_id.clone();
    let complete_result = {
        let mut complete_fut = std::pin::pin!(complete_upload(http, base, token, &poll_id));
        loop {
            tokio::select! {
                biased;
                result = complete_fut.as_mut() => break result,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    if let Ok(Some(info)) = fetch_upload(&poll_http, &poll_base, &poll_token, &poll_id).await {
                        on_progress(ChunkProgress {
                            upload_id: poll_id.clone(),
                            phase: phase_of(&info.status),
                            uploaded_chunks: total,
                            total_chunks: total,
                            server_progress: if info.status == "assembling" {
                                info.assembly_progress
                            } else {
                                0
                            },
                        });
                    }
                }
            }
        }
    };

    match complete_result {
        Ok(()) => {
            eprintln!("[lts] complete ok uploadId={upload_id}");
            Ok(upload_id)
        }
        Err(err) => match fetch_upload(http, base, token, &upload_id).await {
            Ok(Some(info))
                if matches!(
                    info.status.as_str(),
                    "assembling" | "uploaded" | "converting" | "completed"
                ) =>
            {
                Ok(upload_id)
            }
            Ok(Some(info)) if info.status == "failed" => Err(ApiError::Fatal(
                info.error.unwrap_or_else(|| err.to_string()),
            )),
            Ok(_) => Err(err),
            Err(ApiError::Retryable(_)) => Err(err),
            Err(other) => Err(other),
        },
    }
}

pub async fn poll_conversion(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    upload_id: &str,
) -> Result<ConvUpdate, ApiError> {
    let response = http
        .get(format!("{base}/api/uploads/{upload_id}/conversion-progress"))
        .bearer_auth(token)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(classify_reqwest)?;
    if response.status().as_u16() == 404 {
        let text = response.text().await.unwrap_or_default();
        return Ok(ConvUpdate {
            kind: ConvKind::Missing,
            progress: 0,
            video_id: None,
            slug: None,
            detail: Some(format_server_error(404, &text)),
        });
    }
    if !response.status().is_success() {
        return Err(error_from_status(response).await);
    }
    let value = parse_json(response).await?;
    let conversion = value.get("conversion").cloned().unwrap_or(Value::Null);
    let status = text_at(&conversion, "status")
        .or_else(|| text_at(&conversion, "conversionStatus"))
        .or_else(|| text_at(&value, "overallStatus"))
        .unwrap_or_else(|| "not_found".into());
    let progress = json_u32(&conversion, "progress").min(100);
    let video_id = text_at(&conversion, "videoId").filter(|item| !item.is_empty());
    let slug = text_at(&conversion, "slug").filter(|item| !item.is_empty());
    let error = text_at(&conversion, "error").filter(|item| !item.is_empty());
    let already = conversion
        .get("alreadyExisted")
        .and_then(|item| item.as_bool())
        .unwrap_or(false);
    let kind = match status.as_str() {
        "completed" | "complete" | "success" | "done" => ConvKind::Done,
        "failed" | "error" | "cancelled" | "canceled" => ConvKind::Failed,
        "not_found" | "" => ConvKind::Missing,
        _ => ConvKind::Running,
    };
    let detail = if kind == ConvKind::Failed {
        error.or(Some(status))
    } else if already {
        Some("alreadyExisted".into())
    } else {
        Some(status)
    };
    Ok(ConvUpdate {
        kind,
        progress,
        video_id,
        slug,
        detail,
    })
}

pub async fn cancel_upload(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    upload_id: &str,
) -> Result<(), ApiError> {
    let _ = success_json(
        http.post(format!("{base}/api/uploads/{upload_id}/cancel"))
            .bearer_auth(token),
        Duration::from_secs(30),
    )
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ProcessingUpload {
    pub upload_id: String,
    pub filename: String,
    pub size: u64,
    pub status: String,
    pub progress: u32,
    pub video_id: Option<String>,
    pub slug: Option<String>,
    pub source_fingerprint: Option<String>,
}

pub fn is_server_pipeline_status(status: &str) -> bool {
    matches!(
        status,
        "assembling"
            | "uploaded"
            | "converting"
            | "processing"
            | "moving"
            | "restarting"
            | "server_processing"
    )
}

pub async fn fetch_processing(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    username: &str,
) -> Result<Vec<ProcessingUpload>, ApiError> {
    let response = http
        .get(format!("{base}/api/uploads/processing"))
        .bearer_auth(token)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(classify_reqwest)?;
    if !response.status().is_success() {
        return Err(error_from_status(response).await);
    }
    let value = parse_json(response).await?;
    let items = value
        .get("videos")
        .and_then(|v| v.as_array())
        .or_else(|| value.get("uploads").and_then(|v| v.as_array()))
        .cloned()
        .unwrap_or_default();
    Ok(items
        .iter()
        .filter_map(|item| parse_processing_upload(item, username))
        .collect())
}

fn conversion_terminal_status(status: &str) -> bool {
    matches!(
        status,
        "completed"
            | "complete"
            | "done"
            | "success"
            | "failed"
            | "error"
            | "cancelled"
            | "canceled"
    )
}

fn parse_processing_upload(value: &Value, username: &str) -> Option<ProcessingUpload> {
    let upload_id = text_at(value, "id")
        .or_else(|| text_at(value, "uploadId"))
        .filter(|id| !id.is_empty())?;
    if let Some(owner) = text_at(value, "uploadedBy").filter(|s| !s.is_empty()) {
        if !username.is_empty() && !owner.eq_ignore_ascii_case(username) {
            return None;
        }
    }
    let filename = text_at(value, "originalName")
        .or_else(|| text_at(value, "filename"))
        .or_else(|| text_at(value, "inputFilename"))
        .unwrap_or_else(|| upload_id.clone());
    let status = text_at(value, "conversionStatus")
        .or_else(|| text_at(value, "phase"))
        .or_else(|| text_at(value, "status"))
        .unwrap_or_else(|| "converting".into());
    if conversion_terminal_status(&status) {
        return None;
    }
    let progress = json_u32(value, "progress").max(json_u32(value, "encodePercent"));
    Some(ProcessingUpload {
        upload_id,
        filename,
        size: json_u64(value, "size")
            .max(json_u64(value, "totalSize"))
            .max(json_u64(value, "inputSize")),
        status,
        progress,
        video_id: text_at(value, "videoId").filter(|s| !s.is_empty()),
        slug: text_at(value, "slug").filter(|s| !s.is_empty()),
        source_fingerprint: text_at(value, "sourceFingerprint")
            .or_else(|| text_at(value, "fileHash"))
            .or_else(|| text_at(value, "fingerprint")),
    })
}

pub async fn fetch_active(
    http: &reqwest::Client,
    base: &str,
    token: &str,
) -> Result<Vec<ActiveSession>, ApiError> {
    let response = http
        .get(format!("{base}/api/uploads/active"))
        .bearer_auth(token)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(classify_reqwest)?;
    if !response.status().is_success() {
        return Err(error_from_status(response).await);
    }
    let value = parse_json(response).await?;
    let items = value
        .get("uploads")
        .and_then(|v| v.as_array())
        .or_else(|| value.get("active").and_then(|v| v.as_array()))
        .or_else(|| value.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(items.iter().filter_map(parse_active_session).collect())
}

fn parse_active_session(value: &Value) -> Option<ActiveSession> {
    let id = text_at(value, "id").or_else(|| text_at(value, "uploadId"))?;
    if id.is_empty() {
        return None;
    }
    let status = text_at(value, "status").unwrap_or_default();
    if !matches!(status.as_str(), "uploading" | "assembling" | "") {
        return None;
    }
    let progress = json_u32(value, "progress");
    let needs_client = value
        .get("needsClient")
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| {
            status == "uploading" && progress < 100 || status == "assembling"
        });
    if !needs_client {
        return None;
    }
    let filename = text_at(value, "originalName")
        .or_else(|| text_at(value, "filename"))
        .unwrap_or_default();
    let source_fingerprint = text_at(value, "sourceFingerprint")
        .or_else(|| text_at(value, "fileHash"))
        .or_else(|| text_at(value, "fingerprint"));
    Some(ActiveSession {
        id,
        filename: filename.clone(),
        original_name: text_at(value, "originalName"),
        size: json_u64(value, "size").max(json_u64(value, "totalSize")),
        source_fingerprint,
        chunk_size: json_u64(value, "chunkSize"),
        total_chunks: json_u32(value, "totalChunks"),
        missing_chunks: json_u32_vec(value, "missingChunks"),
        progress,
        needs_client,
        status,
    })
}

async fn continue_upload<F>(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    path: &Path,
    upload_id: &str,
    known_fingerprint: Option<&str>,
    on_progress: &mut F,
) -> Result<String, ApiError>
where
    F: FnMut(ChunkProgress),
{
    let info = fetch_upload(http, base, token, upload_id)
        .await?
        .ok_or_else(|| ApiError::Fatal("upload session not found on server".into()))?;
    let file_size = std::fs::metadata(path)
        .map_err(|err| ApiError::Fatal(format!("could not read the file: {err}")))?
        .len();
    if info.size > 0 && file_size != info.size {
        return Err(ApiError::Fatal(format!(
            "file size mismatch: local {file_size} server {}",
            info.size
        )));
    }
    if let (Some(server_fp), Some(local_fp)) = (
        info.file_hash.as_deref().or(known_fingerprint),
        known_fingerprint,
    ) {
        if server_fp.trim().to_ascii_lowercase() != local_fp.trim().to_ascii_lowercase() {
            return Err(ApiError::Fatal(
                "fingerprint mismatch with server session".into(),
            ));
        }
    }
    let use_chunk = info.chunk_size.max(CHUNK_SIZE);
    let total = if info.total_chunks > 0 {
        info.total_chunks
    } else {
        chunk_count(file_size.max(info.size), use_chunk)
    };
    let missing = effective_missing(total, &info.missing_chunks, &info.chunks_on_disk);
  let mut done = total.saturating_sub(missing.len() as u32);
    on_progress(ChunkProgress {
        upload_id: upload_id.to_string(),
        phase: "uploading",
        uploaded_chunks: done,
        total_chunks: total,
        server_progress: 0,
    });
    for index in missing {
        send_chunk_retry(http, base, token, path, upload_id, index, use_chunk, file_size).await?;
        done = done.saturating_add(1);
        on_progress(ChunkProgress {
            upload_id: upload_id.to_string(),
            phase: "uploading",
            uploaded_chunks: done,
            total_chunks: total,
            server_progress: 0,
        });
    }
    on_progress(ChunkProgress {
        upload_id: upload_id.to_string(),
        phase: "assembling",
        uploaded_chunks: total,
        total_chunks: total,
        server_progress: 0,
    });
    complete_upload(http, base, token, upload_id).await?;
    Ok(upload_id.to_string())
}

pub(crate) async fn fetch_upload(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    upload_id: &str,
) -> Result<Option<UploadInfo>, ApiError> {
    let response = http
        .get(format!("{base}/api/uploads/{upload_id}"))
        .bearer_auth(token)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(classify_reqwest)?;
    if response.status().as_u16() == 404 {
        let _ = response.bytes().await;
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(error_from_status(response).await);
    }
    let value = parse_json(response).await?;
    let upload = value.get("upload").cloned().unwrap_or(value);
    let info = parse_upload(&upload);
    if info.id.is_empty() {
        return Err(ApiError::Fatal("server response has no upload id".into()));
    }
    Ok(Some(info))
}

async fn init_upload(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    filename: &str,
    total_chunks: u32,
    total_size: u64,
    chunk_size: u64,
    source_fingerprint: &str,
    force_new: bool,
) -> Result<InitResult, ApiError> {
    let mut body = serde_json::json!({
        "filename": filename,
        "totalChunks": total_chunks,
        "totalSize": total_size,
        "chunkSize": chunk_size,
        "sourceFingerprint": source_fingerprint,
        "libraryKind": "replay",
    });
    if force_new {
        body["forceNew"] = serde_json::Value::Bool(true);
    }
    let request = http
        .post(format!("{base}/api/uploads/init"))
        .bearer_auth(token)
        .json(&body);
    let response = tokio::time::timeout(Duration::from_secs(45), request.send())
        .await
        .map_err(|_| ApiError::Retryable("init: нет ответа сервера за 45 с".into()))?
        .map_err(classify_reqwest)?;
    let http_status = response.status().as_u16();
    if http_status == 409 {
        let text = response.text().await.unwrap_or_default();
        return Err(ApiError::Duplicate(format_server_error(409, &text)));
    }
    if !response.status().is_success() {
        return Err(error_from_status(response).await);
    }
    let value = parse_json(response).await?;
    let id = text_at(&value, "uploadId").unwrap_or_default();
    if id.is_empty() {
        return Err(ApiError::Fatal(format!(
            "init response has no uploadId: {}",
            clip(&value.to_string(), 300)
        )));
    }
    let resumed = value
        .get("resumed")
        .and_then(|item| item.as_bool())
        .unwrap_or(false);
    let resp_chunk = json_u64(&value, "chunkSize");
    let resp_total = json_u32(&value, "totalChunks");
    let missing_chunks = json_u32_vec(&value, "missingChunks");
    let chunks_on_disk = json_u32_vec(&value, "chunksOnDisk");
    Ok(InitResult {
        upload_id: id,
        resumed,
        missing_chunks,
        chunks_on_disk,
        chunk_size: if resp_chunk > 0 { resp_chunk } else { chunk_size },
        total_chunks: if resp_total > 0 {
            resp_total
        } else {
            total_chunks
        },
        http_status,
    })
}

async fn complete_upload(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    upload_id: &str,
) -> Result<(), ApiError> {
    let response = http
        .post(format!("{base}/api/uploads/complete"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "uploadId": upload_id }))
        .timeout(Duration::from_secs(30 * 60))
        .send()
        .await
        .map_err(classify_reqwest)?;
    if response.status().as_u16() == 409 {
        let text = response.text().await.unwrap_or_default();
        return Err(ApiError::Duplicate(format_server_error(409, &text)));
    }
    if !response.status().is_success() {
        return Err(error_from_status(response).await);
    }
    let _ = response.bytes().await;
    Ok(())
}

async fn send_chunk_retry(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    path: &Path,
    upload_id: &str,
    index: u32,
    chunk_size: u64,
    file_size: u64,
) -> Result<(), ApiError> {
    let mut last = None;
    for attempt in 1..=3u32 {
        match send_chunk(
            http, base, token, path, upload_id, index, chunk_size, file_size,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(ApiError::Retryable(msg)) => {
                last = Some(msg);
                tokio::time::sleep(Duration::from_secs(2 * u64::from(attempt))).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(ApiError::Retryable(
        last.unwrap_or_else(|| format!("chunk {index}: network")),
    ))
}

async fn send_chunk(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    path: &Path,
    upload_id: &str,
    index: u32,
    chunk_size: u64,
    file_size: u64,
) -> Result<(), ApiError> {
    let path = path.to_path_buf();
    let bytes = tokio::task::spawn_blocking(move || {
        read_chunk_sync(&path, index, chunk_size, file_size)
    })
    .await
    .map_err(|err| ApiError::Retryable(format!("read thread: {err}")))??;
    let part = multipart::Part::bytes(bytes)
        .file_name(format!("chunk_{index}"))
        .mime_str("application/octet-stream")
        .map_err(|err| ApiError::Fatal(err.to_string()))?;
    let form = multipart::Form::new().part("chunk", part);
    let index_text = index.to_string();
    let response = http
        .post(format!("{base}/api/uploads/chunk"))
        .query(&[("uploadId", upload_id), ("chunkIndex", index_text.as_str())])
        .bearer_auth(token)
        .multipart(form)
        .timeout(Duration::from_secs(180))
        .send()
        .await
        .map_err(classify_reqwest)?;
    if !response.status().is_success() {
        return Err(error_from_status(response).await);
    }
    let _ = response.bytes().await;
    Ok(())
}

fn read_chunk_sync(
    path: &Path,
    index: u32,
    chunk_size: u64,
    file_size: u64,
) -> Result<Vec<u8>, ApiError> {
    let start = u64::from(index) * chunk_size;
    if start >= file_size {
        return Err(ApiError::Fatal(format!(
            "chunk {index} is past the end of the file"
        )));
    }
    let len = (file_size - start).min(chunk_size) as usize;
    let mut file = std::fs::File::open(path).map_err(io_to_api)?;
    file.seek(SeekFrom::Start(start)).map_err(io_to_api)?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).map_err(io_to_api)?;
    Ok(buf)
}

fn io_to_api(err: std::io::Error) -> ApiError {
    match err.raw_os_error() {
        Some(32) | Some(33) => {
            ApiError::Retryable(format!("file is in use by another program: {err}"))
        }
        _ if err.kind() == std::io::ErrorKind::NotFound => {
            ApiError::Fatal(format!("file not found: {err}"))
        }
        _ => ApiError::Retryable(format!("reading file: {err}")),
    }
}

fn parse_user(value: Option<&Value>) -> Result<UserInfo, ApiError> {
    let user: UserBody = serde_json::from_value(value.cloned().unwrap_or(Value::Null))
        .map_err(|err| ApiError::Fatal(format!("response has no user: {err}")))?;
    Ok(UserInfo {
        username: user.username,
        role: user.role,
    })
}

fn parse_upload(value: &Value) -> UploadInfo {
    UploadInfo {
        id: text_at(value, "id").unwrap_or_default(),
        status: text_at(value, "status").unwrap_or_default(),
        total_chunks: json_u32(value, "totalChunks"),
        chunk_size: json_u64(value, "chunkSize"),
        size: json_u64(value, "size").max(json_u64(value, "totalSize")),
        missing_chunks: json_u32_vec(value, "missingChunks"),
        chunks_on_disk: json_u32_vec(value, "chunksOnDisk"),
        error: text_at(value, "error").filter(|item| !item.is_empty()),
        assembly_progress: json_u32(value, "assemblyProgress").min(100),
        file_hash: text_at(value, "sourceFingerprint")
            .or_else(|| text_at(value, "fileHash"))
            .or_else(|| text_at(value, "fingerprint")),
        filename: text_at(value, "originalName").or_else(|| text_at(value, "filename")),
    }
}

fn phase_of(status: &str) -> &'static str {
    match status {
        "assembling" => "assembling",
        "uploaded" => "uploaded",
        "moving" => "moving",
        "restarting" => "restarting",
        "processing" => "processing",
        "completed" | "converting" => "converting",
        _ => "assembling",
    }
}

async fn success_json(
    request: reqwest::RequestBuilder,
    timeout: Duration,
) -> Result<Value, ApiError> {
    let response = request
        .timeout(timeout)
        .send()
        .await
        .map_err(classify_reqwest)?;
    if !response.status().is_success() {
        return Err(error_from_status(response).await);
    }
    parse_json(response).await
}

async fn parse_json(response: reqwest::Response) -> Result<Value, ApiError> {
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|err| ApiError::Retryable(err.to_string()))?;
    serde_json::from_str(&text).map_err(|err| {
        ApiError::Fatal(format!(
            "{status} response is not JSON ({err}): {}",
            clip(&text, 300)
        ))
    })
}

async fn error_from_status(response: reqwest::Response) -> ApiError {
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let message = format_server_error(status, &text);
    if status == 401 {
        ApiError::Unauthorized(message)
    } else if status == 408 || status == 429 || status >= 500 {
        ApiError::Retryable(message)
    } else {
        ApiError::Fatal(message)
    }
}

fn classify_reqwest(err: reqwest::Error) -> ApiError {
    let message = err.to_string();
    if err.is_builder() {
        ApiError::Fatal(message)
    } else {
        ApiError::Retryable(message)
    }
}

fn format_server_error(status: u16, text: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        let err = value.get("error").and_then(|item| item.as_str()).unwrap_or("");
        let details = value
            .get("details")
            .and_then(|item| item.as_str())
            .filter(|item| !item.is_empty())
            .or_else(|| value.get("message").and_then(|item| item.as_str()))
            .unwrap_or("");
        return match (err.is_empty(), details.is_empty()) {
            (false, false) => format!("{status} {err}: {details}"),
            (false, true) => format!("{status} {err}"),
            (true, false) => format!("{status} {details}"),
            (true, true) => format!("{status} {}", clip(text, 400)),
        };
    }
    format!("{status} {}", clip(text, 400))
}

fn text_at(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|item| item.as_str()).map(str::to_string)
}

fn json_u64(value: &Value, key: &str) -> u64 {
    match value.get(key) {
        Some(Value::Number(number)) => number
            .as_u64()
            .or_else(|| number.as_f64().map(|item| item as u64))
            .unwrap_or(0),
        Some(Value::String(text)) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn json_u32(value: &Value, key: &str) -> u32 {
    json_u64(value, key).min(u64::from(u32::MAX)) as u32
}

fn json_u32_vec(value: &Value, key: &str) -> Vec<u32> {
    value
        .get(key)
        .and_then(|item| item.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_u64().map(|number| number as u32))
                .collect()
        })
        .unwrap_or_default()
}

fn clip(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let short: String = trimmed.chars().take(max).collect();
    format!("{short}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_is_applied_once() {
        assert_eq!(apply_prefix("game.mp4", "usr_"), "usr_game.mp4");
        assert_eq!(apply_prefix("usr_game.mp4", "usr_"), "usr_game.mp4");
        assert_eq!(apply_prefix("Usr_game.mp4", "usr_"), "Usr_game.mp4");
        assert_eq!(apply_prefix("game.mp4", "  "), "game.mp4");
        assert_eq!(prefix_from_username("usr"), "usr_");
        assert_eq!(prefix_from_username("Usr_"), "usr_");
    }

    #[test]
    fn chunk_math() {
        assert_eq!(chunk_count(0, CHUNK_SIZE), 0);
        assert_eq!(chunk_count(1, 5), 1);
        assert_eq!(chunk_count(5, 5), 1);
        assert_eq!(chunk_count(6, 5), 2);
    }

    #[test]
    fn missing_chunks_follow_the_server_list() {
        assert_eq!(effective_missing(4, &[1, 3, 3], &[0]), vec![1, 3]);
        assert!(effective_missing(4, &[], &[0, 1, 2, 3]).is_empty());
        assert_eq!(effective_missing(3, &[], &[]), vec![0, 1, 2]);
    }

    #[test]
    fn parse_processing_videos_array() {
        let value: Value = serde_json::json!({
            "success": true,
            "count": 1,
            "videos": [{
                "id": "abc",
                "originalName": "clip.mp4",
                "conversionStatus": "converting",
                "progress": 47,
                "uploadedBy": "usr",
                "size": 100
            }]
        });
        let row = parse_processing_upload(&value["videos"][0], "usr");
        assert!(row.is_some());
        assert_eq!(row.unwrap().upload_id, "abc");
        assert!(parse_processing_upload(&value["videos"][0], "other").is_none());
    }

    #[test]
    fn parse_check_duplicates_results() {
        let value: Value = serde_json::json!({
            "results": [
                { "sourceFingerprint": "a".repeat(64), "duplicate": true },
                { "sourceFingerprint": "b".repeat(64), "duplicate": false }
            ]
        });
        let fps = duplicate_fps_from_response(&value);
        assert_eq!(fps.len(), 1);
        assert!(fps.contains(&"a".repeat(64)));
    }

    #[test]
    fn server_error_joins_error_and_details() {
        let msg = format_server_error(400, r#"{"error":"File too large","details":"max 10GB"}"#);
        assert_eq!(msg, "400 File too large: max 10GB");
        let auth = format_server_error(
            401,
            r#"{"error":"Требуется авторизация","message":"Отсутствует заголовок Authorization"}"#,
        );
        assert_eq!(
            auth,
            "401 Требуется авторизация: Отсутствует заголовок Authorization"
        );
    }

    #[test]
    fn verbatim_windows_prefix() {
        assert_eq!(strip_verbatim(r"\\?\C:\videos\a.mp4"), r"C:\videos\a.mp4");
        assert_eq!(
            strip_verbatim(r"\\?\UNC\server\share\a.mp4"),
            r"\\server\share\a.mp4"
        );
    }
}
