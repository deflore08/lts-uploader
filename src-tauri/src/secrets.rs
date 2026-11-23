use std::path::Path;

const SERVICE: &str = "ru.liandri.lts-uploader";

fn fallback_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("token.fallback")
}

fn entry(username: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, username).map_err(|err| err.to_string())
}

/// Токен кладётся в Credential Manager Windows.
/// Если хранилище недоступно, пишем файл `token.fallback`.
/// TODO: зашифровать запасной файл через DPAPI, сейчас он в открытом виде.
pub fn save_token(data_dir: &Path, username: &str, token: &str) -> Result<(), String> {
    if username.trim().is_empty() || token.is_empty() {
        return Err("пустой логин или токен".into());
    }
    match entry(username).and_then(|item| item.set_password(token).map_err(|err| err.to_string()))
    {
        Ok(()) => {
            let _ = std::fs::remove_file(fallback_path(data_dir));
            Ok(())
        }
        Err(err) => {
            let path = fallback_path(data_dir);
            std::fs::write(&path, format!("{username}\n{token}")).map_err(|io| {
                format!("Credential Manager: {err}; запасной файл: {io}")
            })?;
            eprintln!(
                "TODO: токен записан без шифрования в {} ({err})",
                path.display()
            );
            Ok(())
        }
    }
}

pub fn load_token(data_dir: &Path, username: &str) -> Result<Option<String>, String> {
    if username.trim().is_empty() {
        return Ok(None);
    }
    match entry(username) {
        Ok(item) => match item.get_password() {
            Ok(token) => return Ok(Some(token)),
            Err(keyring::Error::NoEntry) => {}
            Err(err) => eprintln!("keyring: {err}"),
        },
        Err(err) => eprintln!("keyring: {err}"),
    }
    let Ok(text) = std::fs::read_to_string(fallback_path(data_dir)) else {
        return Ok(None);
    };
    let mut lines = text.lines();
    let user = lines.next().unwrap_or("");
    let token = lines.next().unwrap_or("");
    if user == username && !token.is_empty() {
        Ok(Some(token.to_string()))
    } else {
        Ok(None)
    }
}

pub fn delete_token(data_dir: &Path, username: &str) -> Result<(), String> {
    if !username.trim().is_empty() {
        if let Ok(item) = entry(username) {
            match item.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(err) => eprintln!("keyring delete: {err}"),
            }
        }
    }
    let _ = std::fs::remove_file(fallback_path(data_dir));
    Ok(())
}
