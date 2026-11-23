use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};

use crate::upload::has_video_ext;

const MAX_SCAN: usize = 5000;
const MAX_DEPTH: u32 = 8;

pub struct WatchHub {
    events: Arc<Mutex<Vec<PathBuf>>>,
    watcher: Option<RecommendedWatcher>,
}

impl WatchHub {
    pub fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            watcher: None,
        }
    }

    pub fn set_folder(&mut self, folder: Option<&Path>) -> Result<(), String> {
        self.watcher = None;
        let Some(folder) = folder else {
            return Ok(());
        };
        if !folder.is_dir() {
            return Err(format!("нет такой папки: {}", folder.display()));
        }
        let events = Arc::clone(&self.events);
        let mut watcher = RecommendedWatcher::new(
            move |result: Result<notify::Event, notify::Error>| {
                let Ok(event) = result else {
                    return;
                };
                if let Ok(mut queue) = events.lock() {
                    queue.extend(event.paths);
                }
            },
            Config::default(),
        )
        .map_err(|err| err.to_string())?;
        watcher
            .watch(folder, RecursiveMode::Recursive)
            .map_err(|err| err.to_string())?;
        self.watcher = Some(watcher);
        Ok(())
    }

    pub fn drain(&self) -> Vec<PathBuf> {
        match self.events.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(poison) => std::mem::take(&mut *poison.into_inner()),
        }
    }
}

pub fn find_video_by_basename(dir: &Path, basename: &str, size: u64) -> Option<PathBuf> {
    let mut out = None;
    find_walk(dir, 0, basename, size, &mut out);
    out
}

fn find_walk(dir: &Path, depth: u32, basename: &str, size: u64, out: &mut Option<PathBuf>) {
    if out.is_some() || depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.is_some() {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            find_walk(&path, depth + 1, basename, size, out);
            continue;
        }
        if !has_video_ext(&path) {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.eq_ignore_ascii_case(basename) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() == size {
            *out = Some(path);
            return;
        }
    }
}

pub fn collect_videos(dir: &Path) -> (Vec<PathBuf>, bool) {
    let mut out = Vec::new();
    let mut capped = false;
    walk(dir, 0, &mut out, &mut capped);
    (out, capped)
}

fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>, capped: &mut bool) {
    if *capped || depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_SCAN {
            *capped = true;
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|item| item.to_str())
                .unwrap_or("");
            if skip_dir(name) {
                continue;
            }
            walk(&path, depth + 1, out, capped);
        } else if has_video_ext(&path) {
            out.push(path);
        }
    }
}

fn skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || name.eq_ignore_ascii_case("node_modules")
        || name.eq_ignore_ascii_case("$RECYCLE.BIN")
        || name.eq_ignore_ascii_case("System Volume Information")
}
