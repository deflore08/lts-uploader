# Security

## Network

The release desktop app talks to a fixed API host compiled into the client (`src-tauri/src/config.rs`, front-end in `src/main.js`). Review `upload.rs` and `worker.rs` for HTTP paths under `/api/…`.

The `lts-upload` CLI accepts `--base` per run (development tool).

## Secrets

- Password: sent only to `POST /api/auth/login`, not stored on disk.
- Session token: Windows Credential Manager (`src-tauri/src/secrets.rs`).
- `config.json` holds username and watch folder only.

## Reporting issues

Open a [GitHub issue](https://github.com/deflore08/lts-uploader/issues). Do not post live tokens or passwords.