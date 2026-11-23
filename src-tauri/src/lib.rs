mod commands;
pub mod config;
pub mod fingerprint;
mod queue;
mod resume;
mod secrets;
mod state;
mod thumb;
pub mod upload;
mod watcher;
mod worker;

use std::fs::OpenOptions;
use std::sync::atomic::Ordering;

use fs2::FileExt;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, WindowEvent};
use tauri_plugin_dialog::DialogExt;

use crate::state::{tray_image, tray_tooltip, AppState};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let quitting = window
                    .app_handle()
                    .try_state::<AppState>()
                    .map(|state| state.quitting.load(Ordering::Relaxed))
                    .unwrap_or(false);
                if !quitting {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            if let Err(err) = setup(app) {
                let _ = app
                    .dialog()
                    .message(err.clone())
                    .title("LTS Uploader")
                    .blocking_show();
                return Err(Box::new(std::io::Error::other(err)));
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::snapshot,
            commands::login,
            commands::logout,
            commands::pick_watch_folder,
            commands::upload_file_now,
            commands::retry_failed,
            commands::retry_job,
            commands::dismiss_job,
            commands::cancel_job,
            commands::open_external,
            commands::quit,
            commands::thumbnail,
        ])
        .run(tauri::generate_context!())
        .expect("ошибка запуска LTS Uploader");
}

fn setup(app: &mut tauri::App) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|err| err.to_string())?;
    std::fs::create_dir_all(&data_dir).map_err(|err| err.to_string())?;
    acquire_instance_lock(&data_dir)?;

    let config = config::AppConfig::load(&data_dir);
    let http = upload::http_client().map_err(|err| err.to_string())?;
    let mut watch = watcher::WatchHub::new();
    if !config.watch_folder.trim().is_empty() {
        if let Err(err) = watch.set_folder(Some(std::path::Path::new(config.watch_folder.trim())))
        {
            eprintln!("папка наблюдения: {err}");
        }
    }
    if let Some(window) = app.get_webview_window("main") {
        if let Ok(icon) = tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png")) {
            let _ = window.set_icon(icon);
        }
    }

    app.manage(AppState {
        data_dir,
        config: std::sync::Mutex::new(config),
        queue: std::sync::Mutex::new(queue::Queue::new()),
        session: std::sync::Mutex::new(None),
        fingerprints: std::sync::Mutex::new(upload::FingerprintSet::default()),
        skip_paths: std::sync::Mutex::new(std::collections::HashSet::new()),
        http,
        wake: std::sync::Arc::new(tokio::sync::Notify::new()),
        watch: std::sync::Mutex::new(watch),
        log: std::sync::Mutex::new(std::collections::VecDeque::new()),
        activity: std::sync::Mutex::new(state::ActivityView {
            mode: "idle".into(),
            label: state::activity_label("idle").into(),
            detail: String::new(),
            step: 0,
            total: 0,
        }),
        inflight_fingerprints: std::sync::Mutex::new(std::collections::HashSet::new()),
        watcher_ready: std::sync::atomic::AtomicBool::new(false),
        tray_mode: std::sync::Mutex::new(String::new()),
        quitting: std::sync::atomic::AtomicBool::new(false),
    });

    let open_item = MenuItem::with_id(app, "open", "Open", true, None::<&str>)
        .map_err(|err| err.to_string())?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)
        .map_err(|err| err.to_string())?;
    let menu = Menu::with_items(app, &[&open_item, &quit_item]).map_err(|err| err.to_string())?;
    let _tray = TrayIconBuilder::with_id("main")
        .tooltip(tray_tooltip("idle"))
        .icon(tray_image("idle"))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => state::reveal(app),
            "quit" => {
                if let Some(state) = app.try_state::<AppState>() {
                    state.quitting.store(true, Ordering::Relaxed);
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                state::reveal(tray.app_handle());
            }
        })
        .build(app)
        .map_err(|err| err.to_string())?;

    worker::spawn(app.handle().clone());
    Ok(())
}

fn acquire_instance_lock(dir: &std::path::Path) -> Result<(), String> {
    let path = dir.join(".instance.lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .map_err(|err| err.to_string())?;
    file.try_lock_exclusive().map_err(|_| {
        "LTS Uploader is already running. Open it from the tray icon, or quit that process."
            .to_string()
    })?;
    Box::leak(Box::new(file));
    Ok(())
}