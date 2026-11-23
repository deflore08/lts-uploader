use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::Ordering;

use tauri::{AppHandle, Manager};

use crate::queue::{filename_of, normalize_path};
use crate::state::{self, set_activity_progress, AppState};
use crate::upload::{self, ActiveSession, ProcessingUpload};
use crate::watcher;

pub async fn recover_active_uploads(app: &AppHandle) {
    let state = app.state::<AppState>();
    state.watcher_ready.store(false, Ordering::Release);
    let session = state::lock(&state.session).clone();
    let Some(session) = session else {
        state.watcher_ready.store(true, Ordering::Release);
        return;
    };
    let folder = state::lock(&state.config).watch_folder.clone();
    let root = folder_path(&folder);
    let base = crate::config::SERVER_URL;

    state::set_activity(&state, "resume", "fetching active sessions");
    state::publish(app);

    let active = match upload::fetch_active(&state.http, base, &session.token).await {
        Ok(items) => items,
        Err(err) => {
            state::push_log(&state, format!("resume: {err}"));
            finish_ready(app, &state).await;
            return;
        }
    };

    let resume_list: Vec<&ActiveSession> = active
        .iter()
        .filter(|item| item.needs_client)
        .filter(|item| {
            matches!(
                item.status.as_str(),
                "uploading" | "assembling" | ""
            )
        })
        .collect();

    let mut enqueued = 0usize;
    if let Some(root) = root.as_ref() {
        let total = resume_list.len() as u32;
        for (index, session_item) in resume_list.iter().enumerate() {
            if total > 0 {
                set_activity_progress(
                    &state,
                    "resume",
                    basename_of(&session_item.filename, &session_item.original_name),
                    index as u32 + 1,
                    total,
                );
                state::publish(app);
            }
            match attach_session(root, session_item).await {
                Ok(Some(path)) => {
                    let key = normalize_path(&path);
                    let name = filename_of(&path);
                    let uploaded = session_item
                        .total_chunks
                        .saturating_sub(session_item.missing_chunks.len() as u32);
                    if state::lock(&state.queue).enqueue_resume(
                        &key,
                        &name,
                        session_item.size,
                        &session_item.id,
                        session_item.total_chunks,
                        uploaded,
                        session_item.source_fingerprint.clone(),
                    ) {
                        if let Some(fp) = session_item.source_fingerprint.as_deref() {
                            state::mark_inflight(&state, fp);
                        }
                        enqueued += 1;
                        state::push_log(&state, format!("resume queued: {name}"));
                    }
                }
                Ok(None) => {
                    let name = basename_of(&session_item.filename, &session_item.original_name);
                    if state::lock(&state.queue).insert_missing_session(
                        &name,
                        session_item.size,
                        &session_item.id,
                    ) {
                        state::push_log(&state, format!("resume: file not found — {name}"));
                    }
                }
                Err(err) => {
                    state::push_log(&state, format!("resume {}: {err}", session_item.filename));
                }
            }
        }
    } else if !resume_list.is_empty() {
        state::push_log(&state, "resume: no watch folder (chunk resume skipped)");
    }

    let mut synced = 0usize;
    match upload::fetch_processing(&state.http, base, &session.token, &session.username).await {
        Ok(items) => synced += ingest_processing_list(app, &state, root.as_deref(), &items).await,
        Err(err) => state::push_log(&state, format!("processing: {err}")),
    }

    if enqueued > 0 {
        state.wake.notify_one();
        state::push_log(&state, format!("resume: {enqueued} uploads prioritized"));
    }
    if synced > 0 {
        state::push_log(&state, format!("processing: {synced} jobs in UI"));
    }
    finish_ready(app, &state).await;
}

async fn finish_ready(app: &AppHandle, state: &AppState) {
    state.watcher_ready.store(true, Ordering::Release);
    state::set_activity(&state, "watching", "waiting for stable files");
    state::publish(app);
}

async fn ingest_processing_list(
    app: &AppHandle,
    state: &AppState,
    root: Option<&Path>,
    items: &[ProcessingUpload],
) -> usize {
    let known_ids: HashSet<String> = state::lock(&state.queue)
        .list_visible()
        .into_iter()
        .filter_map(|job| job.upload_id)
        .collect();
    let mut count = 0usize;
    for item in items {
        if known_ids.contains(&item.upload_id) {
            continue;
        }
        let path = if let Some(root) = root {
            watcher::find_video_by_basename(root, &item.filename, item.size)
                .map(|p| normalize_path(&p))
                .unwrap_or_else(|| format!("server:{}", item.upload_id))
        } else {
            format!("server:{}", item.upload_id)
        };
        let total_chunks = 1u32;
        if state::lock(&state.queue).enqueue_server_pipeline(
            &path,
            &item.filename,
            item.size,
            &item.upload_id,
            &item.status,
            total_chunks,
            item.progress,
            item.source_fingerprint.clone(),
            item.video_id.clone(),
            item.slug.clone(),
        ) {
            if let Some(fp) = item.source_fingerprint.as_deref() {
                state::mark_inflight(&state, fp);
            }
            count += 1;
        }
    }
    if count > 0 {
        state::publish(app);
    }
    count
}

async fn attach_session(
    root: &Path,
    session: &ActiveSession,
) -> Result<Option<std::path::PathBuf>, String> {
    let basename = basename_of(&session.filename, &session.original_name);
    if basename.is_empty() || session.size == 0 {
        return Err("session has no filename or size".into());
    }
    let Some(path) = watcher::find_video_by_basename(root, &basename, session.size) else {
        return Ok(None);
    };
    if let Some(server_fp) = session
        .source_fingerprint
        .as_deref()
        .filter(|fp| fp.len() == 64)
    {
        let path_for_fp = path.clone();
        let local_fp = tokio::task::spawn_blocking(move || {
            crate::fingerprint::source_fingerprint(&path_for_fp)
        })
        .await
        .map_err(|err| err.to_string())??;
        if server_fp.trim().to_ascii_lowercase() != local_fp.trim().to_ascii_lowercase() {
            return Err("fingerprint mismatch".into());
        }
    }
    Ok(Some(path))
}

fn basename_of(filename: &str, original: &Option<String>) -> String {
    original
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| filename.to_string())
}

fn folder_path(folder: &str) -> Option<std::path::PathBuf> {
    let folder = folder.trim();
    if folder.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(folder))
    }
}