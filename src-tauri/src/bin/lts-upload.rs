//! Ручная загрузка одного файла без окна.
//! Пароль: переменная LTS_PASSWORD или аргумент --password.
//!
//! cargo run --manifest-path src-tauri/Cargo.toml --bin lts-upload -- --base http://127.0.0.1:8080 --user name --file clip.mp4

use std::path::PathBuf;
use std::time::Duration;

use lts_uploader_lib::config::normalize_base_url;
use lts_uploader_lib::upload::{self, ApiError, ConvKind};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("ошибка: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") || args.is_empty() {
        eprintln!(
            "lts-upload --base URL --user USER --file PATH [--password PASS] [--no-wait]\nпароль можно передать через LTS_PASSWORD"
        );
        if args.is_empty() {
            std::process::exit(2);
        }
        return Ok(());
    }
    let base = normalize_base_url(&required(&args, "--base")?)?;
    let user = required(&args, "--user")?;
    let file = PathBuf::from(required(&args, "--file")?);
    let password = optional(&args, "--password")
        .or_else(|| std::env::var("LTS_PASSWORD").ok())
        .ok_or_else(|| "нужен пароль: LTS_PASSWORD или --password".to_string())?;
    let wait = !args.iter().any(|arg| arg == "--no-wait");
    if !file.is_file() {
        return Err(format!("нет файла {}", file.display()));
    }
    let remote = file
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "video.mp4".into());
    let http = upload::http_client().map_err(|err| err.to_string())?;
    let login = upload::login(&http, &base, &user, &password)
        .await
        .map_err(|err| err.to_string())?;
    println!(
        "вход: {} ({})",
        login.user.username,
        if login.user.role.is_empty() {
            "роль не указана"
        } else {
            login.user.role.as_str()
        }
    );
    println!("файл: {} → {remote}", file.display());
    let upload_id = upload::upload_file(
        &http,
        &base,
        &login.token,
        &file,
        &remote,
        upload::CHUNK_SIZE,
        None,
        false,
        None,
        |progress| {
            println!(
                "{} {}/{}",
                progress.phase, progress.uploaded_chunks, progress.total_chunks
            );
        },
    )
    .await
    .map_err(|err| err.to_string())?;
    println!("uploadId: {upload_id}");
    if !wait {
        return Ok(());
    }
    loop {
        match upload::poll_conversion(&http, &base, &login.token, &upload_id).await {
            Ok(update) => match update.kind {
                ConvKind::Running | ConvKind::Missing => {
                    println!(
                        "сервер: {} {}%",
                        update.detail.unwrap_or_else(|| "ожидание".into()),
                        update.progress
                    );
                }
                ConvKind::Done => {
                    if let Some(slug) = update.slug {
                        println!("готово: {base}/v/{slug}");
                    } else if let Some(id) = update.video_id {
                        println!("готово: {base}/watch/{id}");
                    } else {
                        println!("готово, ссылку сервер не отдал");
                    }
                    if update.detail.as_deref() == Some("alreadyExisted") {
                        println!("ролик уже был в каталоге");
                    }
                    return Ok(());
                }
                ConvKind::Failed => {
                    return Err(update
                        .detail
                        .unwrap_or_else(|| "конвертация не удалась".into()));
                }
            },
            Err(
                ApiError::Retryable(msg)
                | ApiError::Fatal(msg)
                | ApiError::Unauthorized(msg)
                | ApiError::Duplicate(msg),
            ) => {
                eprintln!("опрос: {msg}");
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn required(args: &[String], name: &str) -> Result<String, String> {
    optional(args, name).ok_or_else(|| format!("нет аргумента {name}"))
}

fn optional(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}
