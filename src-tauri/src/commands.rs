use std::path::Path;

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, FilePath};

use crate::queue::{filename_of, normalize_path};
use crate::secrets;
use crate::state::{self, note_fingerprint, set_activity, should_dedup_skip, skip_path, AppState, AuthView, Snapshot};
use crate::upload::{self, ApiError};
use tauri::Manager;

#[tauri::command]
pub fn snapshot(app: AppHandle) -> Snapshot {
    state::snapshot(&app.state::<AppState>())
}

#[tauri::command]
pub async fn login(app: AppHandle, username: String, password: String) -> Result<AuthView, String> {
    let username = username.trim().to_string();
    if username.is_empty() || password.is_empty() {
        return Err("username and password are required".into());
    }
    let state = app.state::<AppState>();
    let base = crate::config::SERVER_URL;
    let result = upload::login(&state.http, base, &username, &password)
        .await
        .map_err(human_error)?;
    secrets::save_token(&state.data_dir, &username, &result.token)?;
    {
        let mut cfg = state::lock(&state.config);
        cfg.username = username.clone();
        cfg.save(&state.data_dir)?;
    }
    let role = result.user.role.clone();
    let token = result.token.clone();
    *state::lock(&state.session) = Some(state::Session {
        username: result.user.username.clone(),
        role: role.clone(),
        token,
    });
    state::push_log(
        &state,
        format!("вход: {} ({})", result.user.username, role_or_unknown(&role)),
    );
    drop(state);
    crate::resume::recover_active_uploads(&app).await;
    app.state::<AppState>().wake.notify_one();
    state::publish(&app);
    Ok(AuthView {
        logged_in: true,
        username: result.user.username,
        role,
    })
}

#[tauri::command]
pub fn logout(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    state::clear_session(&state);
    state::push_log(&state, "выход");
    state::publish(&app);
    Ok(())
}

#[tauri::command]
pub async fn pick_watch_folder(app: AppHandle) -> Result<Option<String>, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Replay folder")
        .blocking_pick_folder();
    let Some(picked) = picked else {
        return Ok(None);
    };
    let folder = path_from_dialog(picked)?;
    let state = app.state::<AppState>();
    {
        let mut cfg = state::lock(&state.config);
        cfg.watch_folder = folder.clone();
        cfg.save(&state.data_dir)?;
    }
    apply_watch(&state)?;
    state::push_log(&state, format!("watch folder: {folder}"));
    drop(state);
    crate::resume::recover_active_uploads(&app).await;
    app.state::<AppState>().wake.notify_one();
    state::publish(&app);
    Ok(Some(folder))
}

#[tauri::command]
pub async fn upload_file_now(app: AppHandle) -> Result<String, String> {
    let picked = app
        .dialog()
        .file()
        .set_title("Choose a video")
        .add_filter(
            "Video",
            &["mp4", "avi", "mov", "mkv", "webm", "flv", "wmv"],
        )
        .blocking_pick_file();
    let Some(picked) = picked else {
        return Ok("no file selected".into());
    };
    let path = path_from_dialog(picked)?;
    let file_path = Path::new(&path);
    if !upload::has_video_ext(file_path) {
        return Err("need a video file: mp4, avi, mov, mkv, webm, flv, wmv".into());
    }
    let size = std::fs::metadata(file_path)
        .map_err(|err| err.to_string())?
        .len();
    let key = normalize_path(file_path);
    let name = filename_of(file_path);
    let state = app.state::<AppState>();

    let path_buf = file_path.to_path_buf();
    set_activity(&state, "fingerprint", filename_of(file_path));
    state::publish(&app);
    let fp = tokio::task::spawn_blocking(move || crate::fingerprint::source_fingerprint(&path_buf))
        .await
        .map_err(|err| err.to_string())??;

    if should_dedup_skip(&state, &fp) {
        skip_path(&state, &key);
        return Ok("already on the site".into());
    }

    let session = state::lock(&state.session).clone();
    if let Some(session) = session {
        set_activity(&state, "check_server", "check-duplicates: 1 файл");
        state::publish(&app);
        let base = crate::config::SERVER_URL;
        match upload::check_duplicates_on_server(
            &state.http,
            base,
            &session.token,
            &[upload::DuplicateProbe {
                source_fingerprint: fp.clone(),
                size_bytes: size,
            }],
        )
        .await
        {
            Ok(dups) if dups.contains(&fp.trim().to_ascii_lowercase()) => {
                note_fingerprint(&state, &fp);
                skip_path(&state, &key);
                state::publish(&app);
                return Ok("already on the site".into());
            }
            Err(err) => state::push_log(&state, format!("check-duplicates: {err}")),
            _ => {}
        }
    }

    let message = {
        let mut queue = state::lock(&state.queue);
        if let Some(mut existing) = queue.find_by_path(&key) {
            match existing.status.as_str() {
                "pending"
                | "uploading"
                | "completing"
                | "assembling"
                | "uploaded"
                | "server_processing"
                | "converting"
                | "processing"
                | "moving"
                | "restarting" => "already in the queue".to_string(),
                "failed" | "dismissed" => {
                    crate::queue::Queue::prepare_restart(&mut existing)?;
                    existing.size = size;
                    queue.save(&existing);
                    format!("queued again: {name}")
                }
                _ => {
                    existing.status = "pending".into();
                    existing.error = None;
                    existing.upload_id = None;
                    existing.uploaded_chunks = 0;
                    existing.total_chunks = 0;
                    existing.server_progress = 0;
                    existing.video_id = None;
                    existing.slug = None;
                    existing.size = size;
                    queue.save(&existing);
                    format!("sending again: {name}")
                }
            }
        } else {
            queue.enqueue_new_with_fp(&key, &name, size, Some(fp));
            format!("queued: {name}")
        }
    };
    state::push_log(&state, message.clone());
    state.wake.notify_one();
    state::publish(&app);
    Ok(message)
}

#[tauri::command]
pub fn retry_failed(app: AppHandle) -> Result<usize, String> {
    let state = app.state::<AppState>();
    let count = state::lock(&state.queue).retry_failed();
    if count > 0 {
        state::push_log(&state, format!("повтор ошибок: {count}"));
        state.wake.notify_one();
        state::publish(&app);
    }
    Ok(count)
}

#[tauri::command]
pub async fn retry_job(app: AppHandle, id: i64) -> Result<(), String> {
    let state = app.state::<AppState>();
    let old_upload_id = {
        let mut queue = state::lock(&state.queue);
        let Some(mut job) = queue.get(id) else {
            return Err("job not found".into());
        };
        if job.status != "failed" {
            return Err("only a failed upload can be retried".into());
        }
        let old = job.upload_id.clone();
        crate::queue::Queue::prepare_restart(&mut job)?;
        queue.save(&job);
        old
    };
    if let Some(upload_id) = old_upload_id {
        let session = state::lock(&state.session).clone();
        let base = crate::config::SERVER_URL;
        let http = state.http.clone();
        if let Some(session) = session {
            let _ = upload::cancel_upload(&http, &base, &session.token, &upload_id).await;
        }
    }
    state::push_log(&state, format!("повтор с нуля: id {id}"));
    state.wake.notify_one();
    state::publish(&app);
    Ok(())
}

#[tauri::command]
pub fn dismiss_job(app: AppHandle, id: i64) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut queue = state::lock(&state.queue);
    let Some(mut job) = queue.get(id) else {
        return Err("job not found".into());
    };
    if matches!(
        job.status.as_str(),
        "uploading"
            | "completing"
            | "pending"
            | "assembling"
            | "uploaded"
            | "server_processing"
            | "converting"
            | "processing"
            | "moving"
            | "restarting"
    ) {
        return Err("wait until it finishes, or cancel the upload first".into());
    }
    job.status = "dismissed".into();
    queue.save(&job);
    drop(queue);
    state::publish(&app);
    Ok(())
}

#[tauri::command]
pub async fn cancel_job(app: AppHandle, id: i64) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut job = {
        let queue = state::lock(&state.queue);
        queue.get(id).ok_or_else(|| "job not found".to_string())?
    };
    if !matches!(job.status.as_str(), "pending" | "uploading" | "completing") {
        return Err("this upload cannot be cancelled".into());
    }
    if let Some(upload_id) = job.upload_id.clone() {
        let session = state::lock(&state.session).clone();
        let base = crate::config::SERVER_URL;
        if let Some(session) = session {
            if let Err(err) =
                upload::cancel_upload(&state.http, &base, &session.token, &upload_id).await
            {
                state::push_log(&state, format!("отмена на сервере: {err}"));
            }
        }
    }
    job.status = "failed".into();
    job.error = Some("Cancelled".into());
    state::lock(&state.queue).save(&job);
    state::push_log(&state, format!("отменено: {}", job.filename));
    state::publish(&app);
    Ok(())
}

#[tauri::command]
pub fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    let parsed = reqwest::Url::parse(&url).map_err(|_| "invalid link".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("only http(s) links can be opened".into());
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn thumbnail(_app: AppHandle, path: String) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || crate::thumb::data_url(&path))
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
pub fn quit(app: AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.quitting.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    app.exit(0);
}

pub fn apply_watch(state: &AppState) -> Result<(), String> {
    let folder = state::lock(&state.config).watch_folder.clone();
    let mut hub = state::lock(&state.watch);
    if folder.trim().is_empty() {
        hub.set_folder(None)
    } else {
        hub.set_folder(Some(Path::new(folder.trim())))
    }
}

fn path_from_dialog(path: FilePath) -> Result<String, String> {
    path.into_path()
        .map(|item| item.to_string_lossy().to_string())
        .map_err(|_| "the dialog did not return a file path".to_string())
}

fn human_error(err: ApiError) -> String {
    match err {
        ApiError::Unauthorized(msg) => {
            if msg.contains("Invalid") || msg.contains("credentials") {
                "wrong username or password".into()
            } else {
                msg
            }
        }
        other => other.to_string(),
    }
}

fn role_or_unknown(role: &str) -> &str {
    if role.is_empty() {
        "role unknown"
    } else {
        role
    }
}