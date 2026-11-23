use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::image::Image;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use crate::config::{self, AppConfig};
use crate::queue::{Job, Queue};
use crate::upload::FingerprintSet;
use crate::watcher::WatchHub;

#[derive(Clone)]
pub struct Session {
    pub username: String,
    pub role: String,
    pub token: String,
}

pub struct AppState {
    pub data_dir: PathBuf,
    pub config: Mutex<AppConfig>,
    pub queue: Mutex<Queue>,
    pub session: Mutex<Option<Session>>,
    pub fingerprints: Mutex<FingerprintSet>,
    pub skip_paths: Mutex<HashSet<String>>,
    pub http: reqwest::Client,
    pub wake: Arc<Notify>,
    pub watch: Mutex<WatchHub>,
    pub log: Mutex<VecDeque<String>>,
    pub activity: Mutex<ActivityView>,
    pub inflight_fingerprints: Mutex<HashSet<String>>,
    pub watcher_ready: AtomicBool,
    pub tray_mode: Mutex<String>,
    pub quitting: AtomicBool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AuthView {
    pub logged_in: bool,
    pub username: String,
    pub role: String,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivityView {
    pub mode: String,
    pub label: String,
    pub detail: String,
    pub step: u32,
    pub total: u32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub config: AppConfig,
    pub auth: AuthView,
    pub jobs: Vec<Job>,
    pub tray: String,
    pub logs: Vec<String>,
    pub activity: ActivityView,
    pub data_dir: String,
}

pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

pub fn activity_label(mode: &str) -> &'static str {
    match mode {
        "idle" => "Idle",
        "watching" => "Watching",
        "scan" => "Scanning folder",
        "fingerprint" => "Fingerprints",
        "check_server" => "Checking duplicates",
        "resume" => "Resuming uploads",
        "upload" => "Uploading",
        "server_poll" => "Server status",
        _ => "Working",
    }
}

pub fn set_activity(state: &AppState, mode: &str, detail: impl AsRef<str>) {
    set_activity_progress(state, mode, detail, 0, 0);
}

pub fn set_activity_progress(
    state: &AppState,
    mode: &str,
    detail: impl AsRef<str>,
    step: u32,
    total: u32,
) {
    let mut current = lock(&state.activity);
    current.mode = mode.to_string();
    current.label = activity_label(mode).to_string();
    current.detail = detail.as_ref().to_string();
    current.step = step;
    current.total = total;
}

pub fn mark_inflight(state: &AppState, fp: &str) {
    let normalized = fp.trim().to_ascii_lowercase();
    if normalized.len() == 64 {
        lock(&state.inflight_fingerprints).insert(normalized);
    }
}

pub fn clear_inflight(state: &AppState, fp: &str) {
    let normalized = fp.trim().to_ascii_lowercase();
    lock(&state.inflight_fingerprints).remove(&normalized);
}

pub fn should_dedup_skip(state: &AppState, fp: &str) -> bool {
    let normalized = fp.trim().to_ascii_lowercase();
    if lock(&state.inflight_fingerprints).contains(&normalized) {
        return false;
    }
    is_known_fingerprint(state, fp)
}

pub fn push_log(state: &AppState, message: impl AsRef<str>) {
    let line = format!(
        "{} {}",
        chrono::Local::now().format("%H:%M:%S"),
        message.as_ref()
    );
    eprintln!("[lts] {line}");
    let mut log = lock(&state.log);
    log.push_back(line);
    while log.len() > 80 {
        log.pop_front();
    }
}

pub fn note_fingerprint(state: &AppState, fp: &str) {
    let normalized = fp.trim().to_ascii_lowercase();
    if normalized.len() == 64 {
        lock(&state.fingerprints).hashes.insert(normalized);
    }
}

pub fn is_known_fingerprint(state: &AppState, fp: &str) -> bool {
    let normalized = fp.trim().to_ascii_lowercase();
    lock(&state.fingerprints).hashes.contains(&normalized)
}

pub fn skip_path(state: &AppState, path: &str) {
    lock(&state.skip_paths).insert(path.to_string());
}

pub fn should_skip_path(state: &AppState, path: &str) -> bool {
    lock(&state.skip_paths).contains(path)
}

pub fn api_base(state: &AppState) -> Result<String, String> {
    config::api_base(&lock(&state.config))
}

pub fn snapshot(state: &AppState) -> Snapshot {
    let config = lock(&state.config).clone();
    let jobs = lock(&state.queue).list_visible();
    let session = lock(&state.session).clone();
    let logs: Vec<String> = lock(&state.log).iter().cloned().collect();
    let tray = tray_kind(&jobs).to_string();
    let auth = match session {
        Some(session) => AuthView {
            logged_in: true,
            username: session.username,
            role: session.role,
        },
        None => AuthView {
            logged_in: false,
            username: config.username.clone(),
            role: String::new(),
        },
    };
    Snapshot {
        config,
        auth,
        jobs,
        tray,
        logs,
        activity: lock(&state.activity).clone(),
        data_dir: state.data_dir.display().to_string(),
    }
}

pub fn tray_kind(jobs: &[Job]) -> &'static str {
    let active = jobs.iter().any(|job| {
        matches!(
            job.status.as_str(),
            "pending"
                | "initializing"
                | "uploading"
                | "completing"
                | "assembling"
                | "uploaded"
                | "server_processing"
                | "converting"
                | "processing"
                | "moving"
                | "restarting"
        )
    });
    if active {
        "working"
    } else if jobs.iter().any(|job| job.status == "failed") {
        "error"
    } else {
        "idle"
    }
}

pub fn publish(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snap = snapshot(&state);
    update_tray(app, &snap.tray);
    let _ = app.emit("snapshot", snap);
}

fn update_tray(app: &AppHandle, mode: &str) {
    let state = app.state::<AppState>();
    let mut current = lock(&state.tray_mode);
    if *current == mode {
        return;
    }
    *current = mode.to_string();
    drop(current);
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let _ = tray.set_tooltip(Some(tray_tooltip(mode)));
    let _ = tray.set_icon(Some(tray_image(mode)));
}

pub fn tray_tooltip(mode: &str) -> String {
    let word = match mode {
        "working" => "working",
        "error" => "error",
        _ => "idle",
    };
    format!("LTS Uploader — {word}")
}

pub fn tray_image(_mode: &str) -> Image<'static> {
    Image::from_bytes(include_bytes!("../icons/32x32.png")).expect("tray icon")
}

pub fn reveal(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

pub fn clear_session(state: &AppState) {
    let username = lock(&state.session)
        .as_ref()
        .map(|session| session.username.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| lock(&state.config).username.clone());
    *lock(&state.session) = None;
    *lock(&state.fingerprints) = FingerprintSet::default();
    lock(&state.inflight_fingerprints).clear();
    state.watcher_ready.store(false, std::sync::atomic::Ordering::Release);
    let _ = crate::secrets::delete_token(&state.data_dir, &username);
}