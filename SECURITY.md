# Security

## Network

The desktop app uses the **server base URL you enter at sign-in** (saved in local `config.json`). All HTTP calls go to that host under `/api/…`. Review `src-tauri/src/upload.rs` and `worker.rs` for paths.

Optional release builds may set a default host at **compile time** via `LTS_DEFAULT_SERVER_URL` (not stored in this repository).

The `lts-upload` CLI accepts `--base` for each run.

## Secrets

- Password: sent only to `POST /api/auth/login`, not stored on disk.
- Session token: Windows Credential Manager (`src-tauri/src/secrets.rs`).
- `config.json` holds username, watch folder, and server URL only (no password, no token).

## Reporting issues

Open a [GitHub issue](https://github.com/deflore08/lts-uploader/issues). Do not post live tokens or passwords.