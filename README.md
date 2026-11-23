# LTS Replay Uploader

Портативный десктопный загрузчик реплеев (Tauri 2 + Rust). Спека: [docs/desktop-uploader-spec.md](docs/desktop-uploader-spec.md).

Исходники: [github.com/deflore08/lts-uploader](https://github.com/deflore08/lts-uploader). Обзор безопасности — [SECURITY.md](SECURITY.md).

## Поведение (кратко)

- **Вход:** те же учётные данные, что и на веб-сайте Replay; токен — Credential Manager.
- **Старт:** после login → `GET /api/uploads/active`, догрузка прерванных, синхронизация `GET /api/uploads/processing`.
- **Дедуп:** `replay-source-fp-v1` → `POST /api/uploads/check-duplicates` (batch до 500).
- **Watcher:** рекурсивный обход, debounce 8 с, `libraryKind: "replay"`.

## Сборка

```powershell
git clone https://github.com/deflore08/lts-uploader.git
cd lts-uploader
npm install
npm run dev
```

Release:

```powershell
npm run build:win
```

Release: `opt-level = z`, strip, LTO, WebView2 **не** вшивается (`webviewInstallMode: skip`).

Данные: `%APPDATA%\ru.liandri.lts-uploader\` — `config.json` (`username`, `watchFolder`), `.instance.lock`.

## CLI

```powershell
cargo run --manifest-path src-tauri/Cargo.toml --bin lts-upload -- --base URL --user USER --file PATH
```

Пароль: `LTS_PASSWORD` или `--password`.