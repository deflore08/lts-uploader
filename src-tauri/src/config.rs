use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const STABILITY_SECS: u64 = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppConfig {
    pub username: String,
    pub watch_folder: String,
    pub server_url: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            username: String::new(),
            watch_folder: String::new(),
            server_url: String::new(),
        }
    }
}

/// Base URL from config, or compile-time `LTS_DEFAULT_SERVER_URL` when config is empty (release builds only).
pub fn api_base(cfg: &AppConfig) -> Result<String, String> {
    let raw = cfg.server_url.trim();
    let raw = if raw.is_empty() {
        option_env!("LTS_DEFAULT_SERVER_URL").unwrap_or("")
    } else {
        raw
    };
    if raw.is_empty() {
        return Err("укажите адрес сервера (https://…)".into());
    }
    normalize_base_url(raw)
}

impl AppConfig {
    fn file(dir: &Path) -> PathBuf {
        dir.join("config.json")
    }

    pub fn load(dir: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(Self::file(dir)) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|err| err.to_string())?;
        std::fs::write(Self::file(dir), text).map_err(|err| err.to_string())
    }
}

pub fn normalize_base_url(input: &str) -> Result<String, String> {
    let raw = input.trim().trim_end_matches('/').to_string();
    if raw.is_empty() {
        return Err("укажите адрес сервера".into());
    }
    let url = reqwest::Url::parse(&raw).map_err(|_| format!("некорректный URL: {raw}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("схема {other} не поддерживается, нужен http или https")),
    }
    if url.host_str().is_none() {
        return Err("в адресе нет хоста".into());
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err(
            "укажите корень сервера без пути, например http://127.0.0.1:8080".into(),
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("уберите параметры из адреса сервера".into());
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_lan_and_https_origins() {
        assert_eq!(
            normalize_base_url(" http://127.0.0.1:8080/ ").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            normalize_base_url("https://replay.example").unwrap(),
            "https://replay.example"
        );
    }

    #[test]
    fn rejects_api_path() {
        assert!(normalize_base_url("http://127.0.0.1:8080/api").is_err());
    }
}