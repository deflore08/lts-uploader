# LTS Replay Uploader

Портативный десктопный загрузчик реплеев (Tauri 2 + Rust). Спека: [docs/desktop-uploader-spec.md](docs/desktop-uploader-spec.md).

Исходники: [github.com/deflore08/lts-uploader](https://github.com/deflore08/lts-uploader). Обзор безопасности — [SECURITY.md](SECURITY.md).

## Поведение (кратко)

- **Вход:** адрес API (корень `https://…` без пути), логин и пароль. URL и username хранятся в локальном `config.json`; токен — в Credential Manager.
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

Опционально при сборке release без поля в UI можно задать URL по умолчанию: переменная окружения `LTS_DEFAULT_SERVER_URL` (см. `src-tauri/src/config.rs`).

Release: `opt-level = z`, strip, LTO, WebView2 **не** вшивается (`webviewInstallMode: skip`).

Данные: `%APPDATA%\ru.liandri.lts-uploader\` — `config.json` (`username`, `watchFolder`, `serverUrl`), `.instance.lock`.

## CLI

```powershell
cargo run --manifest-path src-tauri/Cargo.toml --bin lts-upload -- --base URL --user USER --file PATH
```

Пароль: `LTS_PASSWORD` или `--password`.