# Desktop uploader (Rust + Tauri): загрузка и отпечатки

Спека десктопного клиента: folder watcher → публичный HTTP API Replay.

Алгоритм отпечатка: **`replay-source-fp-v1`**. Авторизация, init / chunk / complete — как в `docs/api-notes.md`.

## Дедуп (десктоп)

1. Локально: `replay-source-fp-v1` для стабильного файла в папке.
2. `POST /api/uploads/check-duplicates` — `{ "fingerprints": ["64hex…", …] }` (до **500**).
3. Совпадения **не ставятся в очередь**; отпечаток запоминается в сессии.
4. Новые файлы → `init` с `sourceFingerprint`, chunks, `complete`.

## Watcher (клиент v0.6+)

- Debounce размера, рекурсивный обход, silent skip дублей.
- `libraryKind: "replay"`.
- После login: resume из `GET /api/uploads/active` и синхронизация `GET /api/uploads/processing`.

## Чеклист API (дедуп + upload)

| Метод | Путь |
|--------|------|
| POST | `/api/auth/login` |
| POST | `/api/uploads/check-duplicates` |
| POST | `/api/uploads/check-duplicate` | один файл (legacy) |
| POST | `/api/uploads/init` |
| POST | `/api/uploads/chunk` |
| POST | `/api/uploads/complete` |
| GET | `/api/uploads/active` |
| GET | `/api/uploads/processing` |
| GET | `/api/uploads/:id` |
| GET | `/api/uploads/:id/conversion-progress` |