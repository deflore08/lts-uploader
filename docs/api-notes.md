# Заметки по HTTP API (десктопный клиент)

Описание HTTP API, с которым работает клиент (базовый URL задаётся при входе). Без привязки к внутренней структуре сервера.

## Авторизация

- `POST /api/auth/login` — `{ username, password }` → `{ success, token, user }`.
- `GET /api/auth/me` — `Authorization: Bearer <JWT>`.
- Ответ **401** — клиент очищает сессию; задачи в очереди остаются до повторного входа.
- Пароль на диск не пишется (токен — Windows Credential Manager).

## Загрузка файла

Чанк: **5 × 1024 × 1024** байт. Расширения: `.mp4`, `.avi`, `.mov`, `.mkv`, `.webm`, `.flv`, `.wmv`.

1. `POST /api/uploads/init` — `{ filename, totalChunks, totalSize, chunkSize, … }` → `uploadId`, `chunkSize`, `uploadedName`.
2. `POST /api/uploads/chunk?uploadId=&chunkIndex=` — multipart, поле **`chunk`**, индекс с нуля, Bearer.
3. `POST /api/uploads/complete` — `{ uploadId }`. Таймаут клиента до **30 минут**.

При обрыве `complete` клиент смотрит `GET /api/uploads/:id`: статусы `assembling`, `uploaded`, `converting`, `completed` — не слать complete повторно; `uploading` — докачка; `failed` — ошибка.

## Докачка

`GET /api/uploads/:id` → `chunksOnDisk`, `missingChunks`, `size`, `status`.

Отмена: `POST /api/uploads/:id/cancel` (пока сервер в `uploading`).

## После загрузки

- `GET /api/uploads/processing` — активные encode/processing (не финальный источник правды для `failed` vs `done`).
- `GET /api/uploads/:id/conversion-progress` — статус конвертации, прогресс, при успехе `videoId` / `slug`.

## Дедуп

`POST /api/uploads/check-duplicates` — `{ "fingerprints": ["hex…", …] }` (batch до **500**).

## Resume

`GET /api/uploads/active` — сессии, которым нужны чанки с клиента (`needsClient`).

## Поведение клиента

- Чанки **строго по порядку**, без параллели.
- Не шлёт заголовок `Origin` (нативный клиент).
- Локальная очередь + опрос conversion для фаз после `complete`.