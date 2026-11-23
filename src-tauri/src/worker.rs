use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use crate::config::STABILITY_SECS;
use crate::queue::{filename_of, normalize_path, path_key, Job};

const SCAN_WARMUP_SECS: u64 = 45;
const MAX_ENQUEUE_PER_LOOP: usize = 2;
use crate::secrets;
use crate::state::{
    self, clear_session, note_fingerprint, set_activity,
    set_activity_progress, should_dedup_skip, skip_path, should_skip_path, AppState,
};
use crate::upload::{self, ApiError, ConvKind};
use crate::watcher;

struct Candidate {
    size: u64,
    changed: Instant,
}

pub fn spawn(app: AppHandle) {
    let restore = app.clone();
    tauri::async_runtime::spawn(async move {
        restore_session(&restore).await;
    });
    let stability = app.clone();
    tauri::async_runtime::spawn(async move {
        stability_loop(stability).await;
    });
    let uploader = app.clone();
    tauri::async_runtime::spawn(async move {
        upload_loop(uploader).await;
    });
    let poll = app.clone();
    tauri::async_runtime::spawn(async move {
        poll_loop(poll).await;
    });
}

async fn restore_session(app: &AppHandle) {
    let state = app.state::<AppState>();
    let username = state::lock(&state.config).username.clone();
    if username.is_empty() {
        state
            .watcher_ready
            .store(true, Ordering::Release);
        state::publish(app);
        return;
    }
    let token = match secrets::load_token(&state.data_dir, &username) {
        Ok(Some(token)) => token,
        Ok(None) => {
            state.watcher_ready.store(true, Ordering::Release);
            state::publish(app);
            return;
        }
        Err(err) => {
            state::push_log(&state, format!("token read failed: {err}"));
            state.watcher_ready.store(true, Ordering::Release);
            state::publish(app);
            return;
        }
    };
    let Ok(base) = state::api_base(&state) else {
        state.watcher_ready.store(true, Ordering::Release);
        state::publish(app);
        return;
    };
    match upload::me(&state.http, &base, &token).await {
        Ok(user) => {
            *state::lock(&state.session) = Some(state::Session {
                username: user.username,
                role: user.role,
                token: token.clone(),
            });
            state::push_log(&state, "signed in");
            drop(state);
            crate::resume::recover_active_uploads(app).await;
            app.state::<AppState>().wake.notify_waiters();
            state::publish(app);
            return;
        }
        Err(ApiError::Unauthorized(msg)) => {
            let _ = secrets::delete_token(&state.data_dir, &username);
            state::push_log(&state, format!("session expired: {msg}"));
            state.watcher_ready.store(true, Ordering::Release);
        }
        Err(err) => {
            *state::lock(&state.session) = Some(state::Session {
                username: username.clone(),
                role: String::new(),
                token,
            });
            state::push_log(&state, format!("server offline, token kept: {err}"));
            drop(state);
            crate::resume::recover_active_uploads(app).await;
            app.state::<AppState>().wake.notify_waiters();
            state::publish(app);
            return;
        }
    }
    app.state::<AppState>().wake.notify_waiters();
    state::publish(app);
}

async fn stability_loop(app: AppHandle) {
    let mut candidates: HashMap<PathBuf, Candidate> = HashMap::new();
    let mut tick: u64 = 0;
    let mut warned_cap = false;
    let stability = STABILITY_SECS.max(2);
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        tick += 1;
        let state = app.state::<AppState>();
        if !state.watcher_ready.load(Ordering::Acquire) {
            continue;
        }
        let folder = state::lock(&state.config).watch_folder.clone();
        for path in state::lock(&state.watch).drain() {
            consider(&state, &mut candidates, path);
        }
        if tick >= SCAN_WARMUP_SECS && tick % 10 == 1 {
            if let Some(dir) = folder_path(&folder) {
                set_activity(&state, "scan", "обхожу папку");
                state::publish(&app);
                let dir = dir.clone();
                let (found, capped) = tokio::task::spawn_blocking(move || {
                    watcher::collect_videos(&dir)
                })
                .await
                .unwrap_or((Vec::new(), false));
                if capped && !warned_cap {
                    warned_cap = true;
                    state::push_log(
                        &state,
                        "в папке больше 5000 видео, смотрю только первые",
                    );
                }
                for path in found {
                    consider(&state, &mut candidates, path);
                }
            }
        }
        let need = Duration::from_secs(stability);
        let mut queued = Vec::new();
        let mut drop_paths = Vec::new();
        for (path, candidate) in candidates.iter_mut() {
            let meta = match std::fs::metadata(path) {
                Ok(meta) if meta.is_file() => meta,
                _ => {
                    drop_paths.push(path.clone());
                    continue;
                }
            };
            let size = meta.len();
            if size != candidate.size {
                candidate.size = size;
                candidate.changed = Instant::now();
                continue;
            }
            if size == 0 || candidate.changed.elapsed() < need {
                continue;
            }
            if std::fs::File::open(path).is_err() {
                continue;
            }
            queued.push((path.clone(), size));
        }
        let batch: Vec<(PathBuf, u64)> = queued
            .iter()
            .take(MAX_ENQUEUE_PER_LOOP)
            .cloned()
            .collect();
        for path in drop_paths {
            candidates.remove(&path);
        }
        let added = if batch.is_empty() {
            false
        } else {
            match ingest_stable_batch(&app, &state, batch).await {
                Ok((count, done_paths)) => {
                    for path in done_paths {
                        candidates.remove(&path);
                    }
                    count > 0
                }
                Err(err) => {
                    state::push_log(&state, format!("очередь: {err}"));
                    false
                }
            }
        };
        let logged_in = state::lock(&state.session).is_some();
        if logged_in && !folder.trim().is_empty() && candidates.is_empty() && !added {
            set_activity(&state, "watching", "waiting for stable files");
        } else if !logged_in {
            set_activity(&state, "idle", "войдите для загрузки");
        } else if folder.trim().is_empty() {
            set_activity(&state, "idle", "выберите папку");
        }
        if added {
            state.wake.notify_one();
        }
        if added || tick % 5 == 0 {
            state::publish(&app);
        }
    }
}

fn queue_has_path(state: &AppState, path: &Path) -> bool {
    let key = path_key(path);
    state::lock(&state.queue).list_visible().iter().any(|job| {
        job.path == key || path_key(Path::new(&job.path)) == key
    })
}

fn consider(state: &AppState, candidates: &mut HashMap<PathBuf, Candidate>, path: PathBuf) {
    if !upload::has_video_ext(&path) {
        return;
    }
    let key = path_key(&path);
    if should_skip_path(state, &key) {
        return;
    }
    if queue_has_path(state, &path) {
        return;
    }
    use std::collections::hash_map::Entry;
    match candidates.entry(path.clone()) {
        Entry::Vacant(slot) => {
            slot.insert(Candidate {
                size: 0,
                changed: Instant::now(),
            });
            let name = filename_of(&path);
            set_activity(state, "watching", format!("новый файл: {name}"));
            state::push_log(state, format!("увидел: {name}"));
        }
        Entry::Occupied(_) => {}
    }
}

fn folder_path(folder: &str) -> Option<PathBuf> {
    let folder = folder.trim();
    if folder.is_empty() {
        None
    } else {
        Some(PathBuf::from(folder))
    }
}

struct PreparedFile {
    key: String,
    quick_key: String,
    name: String,
    size: u64,
    fingerprint: String,
}

async fn ingest_stable_batch(
    app: &AppHandle,
    state: &AppState,
    batch: Vec<(PathBuf, u64)>,
) -> Result<(usize, Vec<PathBuf>), String> {
    let session = state::lock(&state.session).clone();
    let Some(session) = session else {
        return Ok((0, Vec::new()));
    };
    let filtered: Vec<(PathBuf, u64)> = batch
        .into_iter()
        .filter(|(path, _)| {
            let quick_key = path_key(path);
            !should_skip_path(state, &quick_key) && !queue_has_path(state, path)
        })
        .collect();
    if filtered.is_empty() {
        return Ok((0, Vec::new()));
    }
    let batch_paths: Vec<PathBuf> = filtered.iter().map(|(p, _)| p.clone()).collect();
    let count = filtered.len();
    set_activity(
        state,
        "fingerprint",
        format!("считаю отпечатки ({count} файл.)"),
    );
    state::publish(app);
    let total = filtered.len() as u32;
    let mut to_check = Vec::new();
    let mut ready = Vec::new();
    for (index, (path, size)) in filtered.iter().enumerate() {
        let name = filename_of(path);
        set_activity_progress(state, "fingerprint", &name, (index + 1) as u32, total);
        state::publish(app);
        let path_buf = path.clone();
        let quick_key = path_key(path);
        let (key, fp) = tokio::task::spawn_blocking(move || {
            let key = normalize_path(&path_buf);
            let fp = crate::fingerprint::source_fingerprint(&path_buf)?;
            Ok::<_, String>((key, fp))
        })
        .await
        .map_err(|err| err.to_string())??;
        let item = PreparedFile {
            key,
            quick_key,
            name,
            size: *size,
            fingerprint: fp,
        };
        if should_dedup_skip(state, &item.fingerprint) {
            skip_path(state, &item.key);
            skip_path(state, &item.quick_key);
            state::push_log(
                state,
                format!("дубль (сессия): {}", item.name),
            );
            continue;
        }
        to_check.push(upload::DuplicateProbe {
            source_fingerprint: item.fingerprint.clone(),
            size_bytes: item.size,
        });
        ready.push(item);
    }

    if !to_check.is_empty() {
        let batch_len = to_check.len() as u32;
        set_activity_progress(
            state,
            "check_server",
            format!("check-duplicates ×{batch_len}"),
            batch_len,
            batch_len,
        );
        state::publish(app);
        let base = state::api_base(&state).map_err(|err| err.to_string())?;
        match upload::check_duplicates_on_server(&state.http, &base, &session.token, &to_check).await
        {
            Ok(duplicates) => {
                let mut enqueued = 0usize;
                for item in ready {
                    let fp = item.fingerprint.trim().to_ascii_lowercase();
                    if duplicates.contains(&fp) {
                        note_fingerprint(state, &fp);
                        skip_path(state, &item.key);
                        skip_path(state, &item.quick_key);
                        state::push_log(
                            state,
                            format!("дубль на сервере: {}", item.name),
                        );
                        continue;
                    }
                    if state::lock(&state.queue).enqueue_new_with_fp(
                        &item.key,
                        &item.name,
                        item.size,
                        Some(item.fingerprint),
                    ) {
                        enqueued += 1;
                        state::push_log(state, format!("в очереди: {}", item.name));
                    }
                }
                return Ok((enqueued, batch_paths));
            }
            Err(err) => {
                state::push_log(state, format!("check-duplicates: {err}"));
                return Err(format!(
                    "дедуп не прошёл — файлы не в очереди, повторим позже ({err})"
                ));
            }
        }
    }

    let mut enqueued = 0usize;
    for item in ready {
        if state::lock(&state.queue).enqueue_new_with_fp(
            &item.key,
            &item.name,
            item.size,
            Some(item.fingerprint),
        ) {
            enqueued += 1;
            state::push_log(state, format!("в очереди: {}", item.name));
        }
    }
    Ok((enqueued, batch_paths))
}

async fn upload_loop(app: AppHandle) {
    loop {
        let wake = app.state::<AppState>().wake.clone();
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        if let Err(err) = upload_once(&app).await {
            let state = app.state::<AppState>();
            state::push_log(&state, format!("воркер: {err}"));
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
}

async fn upload_once(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let session = state::lock(&state.session).clone();
    let Some(session) = session else {
        return Ok(());
    };
    let job = state::lock(&state.queue).next_active();
    let Some(mut job) = job else {
        return Ok(());
    };
    set_activity(
        &state,
        "upload",
        format!("{} — {}", job.filename, job.status),
    );
    state::publish(app);
    let base = state::api_base(&state).map_err(|err| err.to_string())?;
    if job.upload_id.is_none() {
        if let Ok(meta) = std::fs::metadata(&job.path) {
            job.size = meta.len();
            state::lock(&state.queue).save(&job);
        }
    }
    let continue_id = if job.force_new {
        None
    } else {
        job.upload_id.clone()
    };
    let force_new = continue_id.is_none() && (job.force_new || job.upload_id.is_none());
    if job.force_new {
        job.force_new = false;
        state::lock(&state.queue).save(&job);
    }
    if job.status == "pending" {
        state::push_log(
            &state,
            format!("start: {} (init, forceNew={})", job.filename, force_new),
        );
        job.status = "initializing".into();
        state::lock(&state.queue).save(&job);
        state::publish(app);
    }
    let path = PathBuf::from(&job.path);
    let job_id = job.id;
    let job_path = job.path.clone();
    let http = state.http.clone();
    let app_progress = app.clone();

    let known_fp = if let Some(fp) = job
        .source_fingerprint
        .clone()
        .filter(|value| value.len() == 64)
    {
        fp
    } else {
        let path_for_fp = path.clone();
        tokio::task::spawn_blocking(move || crate::fingerprint::source_fingerprint(&path_for_fp))
            .await
            .map_err(|err| err.to_string())??
    };

    if !job.priority && should_dedup_skip(&state, &known_fp) {
        state::lock(&state.queue).remove(job_id);
        skip_path(&state, &job_path);
        state::publish(app);
        return Ok(());
    }

    state::mark_inflight(&state, &known_fp);
    let fp_for_upload = known_fp.clone();
    let continue_ref = continue_id.as_deref();
    let last_publish = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let last_publish_cb = last_publish.clone();
    let result = upload::upload_file(
        &http,
        &base,
        &session.token,
        &path,
        &job.filename,
        upload::CHUNK_SIZE,
        Some(&fp_for_upload),
        force_new,
        continue_ref,
        |progress| {
            let state = app_progress.state::<AppState>();
            let Some(mut current) = state::lock(&state.queue).get(job_id) else {
                return;
            };
            current.upload_id = Some(progress.upload_id);
            current.status = progress.phase.to_string();
            current.uploaded_chunks = progress.uploaded_chunks;
            current.total_chunks = progress.total_chunks;
            current.server_progress = progress.server_progress;
            current.error = None;
            state::lock(&state.queue).save(&current);
            let should_publish = {
                let mut last = last_publish_cb.lock().unwrap_or_else(|err| err.into_inner());
                let now = std::time::Instant::now();
                let hot = progress.phase == "uploading" || progress.phase == "initializing";
                let boundary = progress.uploaded_chunks == 0
                    || progress.uploaded_chunks >= progress.total_chunks;
                let due = now.duration_since(*last) >= std::time::Duration::from_millis(350);
                if !hot || boundary || due {
                    *last = now;
                    true
                } else {
                    false
                }
            };
            if should_publish {
                state::publish(&app_progress);
            }
        },
    )
    .await;

    let clear_fp = |state: &AppState| state::clear_inflight(state, &known_fp);
    match result {
        Ok(upload_id) => {
            let Some(mut job) = state::lock(&state.queue).get(job_id) else {
                return Ok(());
            };
            job.upload_id = Some(upload_id);
            job.status = "converting".into();
            job.error = None;
            job.server_progress = 0;
            state::lock(&state.queue).save(&job);
            clear_fp(&state);
            note_fingerprint(&state, &known_fp);
            state::push_log(&state, format!("on server: {}", job.filename));
        }
        Err(ApiError::Duplicate(_)) => {
            clear_fp(&state);
            note_fingerprint(&state, &known_fp);
            skip_path(&state, &job_path);
            state::lock(&state.queue).remove(job_id);
        }
        Err(ApiError::Unauthorized(msg)) => {
            clear_fp(&state);
            if let Some(mut job) = state::lock(&state.queue).get(job_id) {
                job.error = Some(msg.clone());
                state::lock(&state.queue).save(&job);
            }
            clear_session(&state);
            state::push_log(&state, format!("нужен повторный вход: {msg}"));
        }
        Err(ApiError::Retryable(msg)) => {
            if let Some(mut job) = state::lock(&state.queue).get(job_id) {
                job.error = Some(msg.clone());
                state::lock(&state.queue).save(&job);
            }
            state::push_log(&state, format!("повтор позже: {msg}"));
            tokio::time::sleep(Duration::from_secs(4)).await;
        }
        Err(ApiError::Fatal(msg)) => {
            clear_fp(&state);
            if let Some(mut job) = state::lock(&state.queue).get(job_id) {
                job.status = "failed".into();
                job.error = Some(msg.clone());
                state::lock(&state.queue).save(&job);
            }
            state::push_log(&state, format!("ошибка: {msg}"));
        }
    }
    state::publish(app);
    Ok(())
}

async fn poll_loop(app: AppHandle) {
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if let Err(err) = poll_once(&app).await {
            let state = app.state::<AppState>();
            state::push_log(&state, format!("статус сервера: {err}"));
        }
    }
}

async fn poll_once(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let session = state::lock(&state.session).clone();
    let Some(session) = session else {
        return Ok(());
    };
    let jobs = state::lock(&state.queue).processing();
    if jobs.is_empty() {
        return Ok(());
    }
    let base = state::api_base(&state).map_err(|err| err.to_string())?;
    let mut changed = false;
    for job in jobs {
        let Some(upload_id) = job.upload_id.clone() else {
            continue;
        };
        match upload::poll_conversion(&state.http, &base, &session.token, &upload_id).await {
            Ok(update) => {
                let Some(mut job) = state::lock(&state.queue).get(job.id) else {
                    continue;
                };
                if !is_server_phase(&job.status) && !is_conversion_failure(&job) {
                    continue;
                }
                match update.kind {
                    ConvKind::Running => {
                        let phase = conversion_phase(update.detail.as_deref());
                        if job.status != phase || job.server_progress != update.progress {
                            job.status = phase.into();
                            job.server_progress = update.progress;
                            state::lock(&state.queue).save(&job);
                            changed = true;
                        }
                    }
                    ConvKind::Done => {
                        job.status = "done".into();
                        job.server_progress = 100;
                        job.video_id = update.video_id;
                        job.slug = update.slug;
                        job.error = None;
                        state::lock(&state.queue).save(&job);
                        if update.detail.as_deref() == Some("alreadyExisted") {
                            state::push_log(
                                &state,
                                format!("уже было в каталоге: {}", job.filename),
                            );
                        }
                        let link = job
                            .slug
                            .as_ref()
                            .map(|slug| format!("{base}/v/{slug}"))
                            .or_else(|| {
                                job.video_id
                                    .as_ref()
                                    .map(|id| format!("{base}/watch/{id}"))
                            });
                        match link {
                            Some(link) => state::push_log(&state, format!("готово: {link}")),
                            None => state::push_log(&state, format!("готово: {}", job.filename)),
                        }
                        changed = true;
                    }
                    ConvKind::Failed => {
                        let message = update
                            .detail
                            .unwrap_or_else(|| "conversion failed".into());
                        if job.status != "failed" || job.error.as_deref() != Some(message.as_str())
                        {
                            job.status = "failed".into();
                            job.error = Some(message.clone());
                            state::lock(&state.queue).save(&job);
                            state::push_log(&state, format!("конвертация: {message}"));
                            changed = true;
                        }
                    }
                    ConvKind::Missing => {
                        if reconcile_missing(&state, &base, &session.token, &mut job).await? {
                            changed = true;
                        }
                    }
                }
            }
            Err(ApiError::Unauthorized(msg)) => {
                clear_session(&state);
                state::push_log(&state, format!("нужен повторный вход: {msg}"));
                changed = true;
                break;
            }
            Err(err) => {
                state::push_log(&state, format!("опрос {upload_id}: {err}"));
            }
        }
    }
    if changed {
        state::publish(app);
    }
    Ok(())
}

async fn reconcile_missing(
    state: &AppState,
    base: &str,
    token: &str,
    job: &mut Job,
) -> Result<bool, String> {
    let Some(upload_id) = job.upload_id.clone() else {
        return Ok(false);
    };
    match upload::fetch_upload(&state.http, base, token, &upload_id).await {
        Ok(Some(info)) if info.status == "assembling" => {
            let progress = info.assembly_progress;
            if job.status != "assembling" || job.server_progress != progress {
                job.status = "assembling".into();
                job.server_progress = progress;
                job.error = None;
                state::lock(&state.queue).save(job);
                return Ok(true);
            }
            Ok(false)
        }
        Ok(Some(info))
            if matches!(info.status.as_str(), "uploaded" | "converting" | "completed") =>
        {
            if job.status != "converting" {
                job.status = "converting".into();
                job.error = None;
                state::lock(&state.queue).save(job);
                return Ok(true);
            }
            Ok(false)
        }
        Ok(Some(info)) if info.status == "uploading" => {
            job.status = "uploading".into();
            job.error = Some("server is still waiting for chunks".into());
            state::lock(&state.queue).save(job);
            state.wake.notify_one();
            Ok(true)
        }
        Ok(Some(info)) if info.status == "failed" => {
            job.status = "failed".into();
            job.error = info.error.or(Some("server error".into()));
            state::lock(&state.queue).save(job);
            Ok(true)
        }
        Ok(None) => {
            job.status = "failed".into();
            job.error = Some("upload not found on the server".into());
            state::lock(&state.queue).save(job);
            Ok(true)
        }
        Ok(Some(_)) => Ok(false),
        Err(ApiError::Unauthorized(msg)) => {
            clear_session(state);
            state::push_log(state, format!("нужен повторный вход: {msg}"));
            Ok(true)
        }
        Err(err) => {
            state::push_log(state, format!("проверка загрузки: {err}"));
            Ok(false)
        }
    }
}

fn is_conversion_failure(job: &Job) -> bool {
    job.upload_id.is_some()
        && job.total_chunks > 0
        && job.uploaded_chunks >= job.total_chunks
        && job.error.as_deref() != Some("Cancelled")
}

fn is_server_phase(status: &str) -> bool {
    matches!(
        status,
        "server_processing"
            | "assembling"
            | "uploaded"
            | "converting"
            | "processing"
            | "moving"
            | "restarting"
    )
}

fn conversion_phase(detail: Option<&str>) -> &'static str {
    match detail.unwrap_or("converting") {
        "moving" => "moving",
        "restarting" => "restarting",
        "processing" => "processing",
        "uploaded" => "uploaded",
        "assembling" => "assembling",
        _ => "converting",
    }
}