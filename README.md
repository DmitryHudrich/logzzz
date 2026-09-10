## Требования

- Docker и Docker Compose
- Telegram bot token для `logzz`, если нужен бот
- Telegram `api_id` и `api_hash` для `downloader`
- username peer, из которого `downloader` должен скачивать архивы

## Быстрый старт

1. Создай `.env` из примера:

```bash
cp .env.example .env
```

2. Заполни как минимум эти переменные:

```dotenv
TELEGRAM_BOT_TOKEN=
DOWNLOADER_PEER_NAME=
DOWNLOADER_API_ID=
DOWNLOADER_API_HASH=
```

3. **Обязательно для продакшена:** заполни `LOGZZ_TELEGRAM_ALLOWED_USER_IDS` и
   `DOWNLOADER_REST_API_TOKEN` — см. раздел [Access control](#access-control) ниже.
   Без них бот и REST API открыты для любого, кто до них дотянется.

3. Запусти сервисы:

```bash
docker compose up --build
```

4. Для первого запуска `downloader` авторизуй Telegram-сессию через REST. Если задан
   `DOWNLOADER_REST_API_TOKEN`, добавляй `-H "Authorization: Bearer $DOWNLOADER_REST_API_TOKEN"`
   к каждому запросу ниже:

```bash
curl http://127.0.0.1:8090/auth/status
curl -X POST http://127.0.0.1:8090/auth/request-code \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $DOWNLOADER_REST_API_TOKEN" \
  -d '{"phone":"+79990000000"}'
curl -X POST http://127.0.0.1:8090/auth/submit-code \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $DOWNLOADER_REST_API_TOKEN" \
  -d '{"code":"12345"}'
```

5. Если на аккаунте включён 2FA:

```bash
curl -X POST http://127.0.0.1:8090/auth/submit-password \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $DOWNLOADER_REST_API_TOKEN" \
  -d '{"password":"your-2fa-password"}'
```

После успешной авторизации `downloader` сохранит session в `./.local/downloader/downloader.session`. Следующие старты обычно уже не требуют ввода телефона и кода.

## Каталоги

- `./.local/archives`:
  сюда `downloader` складывает архивы, и отсюда `logzz` их парсит.
- `./.local/input`:
  сюда распаковываются архивы перед импортом.
- `./.local/reports`:
  сюда бот сохраняет выгрузки результатов.
- `./.local/downloader`:
  здесь лежат session/state файлы `downloader`.

## Конфигурация

Конфиг собирается в таком порядке:

`yaml < cli < env`

`config.yaml` больше не нужен для стандартного запуска. Базовый сценарий полностью работает через `.env`, переменные окружения и runtime-дефолты.

`docker compose` подставляет все основные runtime-пути и секреты через env:

- `LOGZZ_CLICKHOUSE__*`
- `LOGZZ_MIGRATIONS_DIR`
- `LOGZZ_INPUT_DIR`
- `LOGZZ_ARCHIVE_DIR`
- `LOGZZ_POLL_INTERVAL_SECS`
- `LOGZZ_TELEGRAM__*`
- `DOWNLOADER_*`

Если нужно начать авторизацию `downloader` с нуля:

```bash
rm -f ./.local/downloader/downloader.session
docker compose up --build
```

## REST API downloader

`downloader` поднимает HTTP API по адресу `DOWNLOADER_REST_LISTEN_ADDR`.

По умолчанию:

- внутри compose: `0.0.0.0:8090`
- с хоста: `http://127.0.0.1:8090` (порт публикуется только на loopback, см. `DOWNLOADER_REST_BIND_ADDR` в `.env.example`)

Если `DOWNLOADER_REST_API_TOKEN` не задан, все `/auth/*` эндпоинты (кроме `/health`)
принимают запросы без какой-либо авторизации — любой, кто дотянется до порта, может
угнать процесс логина Telegram-сессии. При старте без токена `downloader` пишет
предупреждение в лог. Держи `DOWNLOADER_REST_API_TOKEN` заданным всегда, когда порт
доступен за пределами localhost.

## REST API logzz

Помимо Telegram-бота, `logzz` поднимает собственный HTTP API для поиска, загрузки
архивов и мониторинга импорта. API включается, только если задан
`LOGZZ_REST__LISTEN_ADDR`.

По умолчанию в compose:

- внутри контейнера: `0.0.0.0:8091`
- с хоста: `http://127.0.0.1:8091` (порт публикуется только на loopback, см.
  `LOGZZ_REST_BIND_ADDR` / `LOGZZ_REST_PORT` в `.env.example`)

Все эндпоинты, кроме `/health`, требуют заголовок
`Authorization: Bearer $LOGZZ_REST_API_TOKEN`, если токен задан. Если
`LOGZZ_REST__API_TOKEN` пуст — API открыт для всех, кто дотянется до порта (при старте
пишется предупреждение в лог). Держи токен заданным всегда, когда порт доступен за
пределами localhost.

Эндпоинты:

- `GET /health` — проверка живости (без авторизации).
- `GET /metrics` — счётчики: всего учёток, всего исходных файлов, статистика последнего
  цикла импорта, список подключённых источников.
- `GET /api/search?type=url|login&q=<запрос>&tags=<t1,t2>&page=<n>` — поиск с
  пагинацией, JSON. Можно фильтровать по тегам: `tags=vip,checked` вернёт только записи,
  у которых есть **все** перечисленные теги. Достаточно указать хотя бы один из `q`/`tags`.
- `GET /api/import/status` — статус фонового импортёра (последний цикл, суммарные
  счётчики, очередь необработанных архивов).
- `POST /api/archives` — загрузка архива (`multipart/form-data`, поле `file`); файл
  кладётся в inbox и импортируется в фоне.
- `GET /api/tags` — список всех используемых тегов.
- `POST /api/tags` — добавить теги записи. Тело:
  `{"url":"...","username":"...","password":"...","tags":["vip"]}`.
- `DELETE /api/tags` — снять теги (тело такое же, как у `POST`).

Примеры:

```bash
curl http://127.0.0.1:8091/health

curl -H "Authorization: Bearer $LOGZZ_REST_API_TOKEN" \
  "http://127.0.0.1:8091/api/search?type=url&q=example.com&page=0"

curl -H "Authorization: Bearer $LOGZZ_REST_API_TOKEN" \
  -F "file=@dump.zip" \
  http://127.0.0.1:8091/api/archives

curl -H "Authorization: Bearer $LOGZZ_REST_API_TOKEN" \
  http://127.0.0.1:8091/metrics

# добавить теги к найденной записи
curl -X POST http://127.0.0.1:8091/api/tags \
  -H "Authorization: Bearer $LOGZZ_REST_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com","username":"alice","password":"hunter2","tags":["vip","checked"]}'

# поиск только по тегам
curl -H "Authorization: Bearer $LOGZZ_REST_API_TOKEN" \
  "http://127.0.0.1:8091/api/search?q=example.com&tags=vip,checked"
```

## Теги

К каждой найденной записи (уникальная тройка `url` + `username` + `password`) можно
привязать произвольные теги и потом фильтровать по ним при поиске.

- Теги хранятся в ClickHouse (`cred_tags`), привязка — по хэшу тройки.
- Управление — через REST (`/api/tags`, см. выше). Теги видны в ответах `/api/search` и
  в HTML-отчётах бота.
- В Telegram-боте можно фильтровать прямо в запросе через `#тег`:
  `/url example.com #vip #checked` — вернёт записи по `example.com` со всеми
  указанными тегами. Можно искать только по тегам: `/url #vip`.
- При нескольких тегах действует логика AND (запись должна иметь все указанные теги).

## Источники логов

Источник логов — это всё, что находит и передаёт архивы в пайплайн импорта. Источники
реализуют трейт `LogSource` (`logzz::domain::source`) и складывают архивы в общий inbox
(`ArchiveInbox`), откуда их забирает импортёр. Добавить новый источник = реализовать
трейт в слое `infrastructure` и зарегистрировать его при старте в `main.rs`.

Доступны:

- **Telegram downloader** (`downloader`) — отдельный бинарник, качает архивы из peer.
- **Локальная директория** — `logzz` следит за указанными папками и импортирует
  положенные туда `.zip`/`.rar`. Настраивается через `LOGZZ_SOURCES__LOCAL_DIRS`
  (список путей через запятую) или флаг `--local-source-dir`.
- **Загрузка по REST** — `POST /api/archives` (см. выше).

## Архитектура

Код `logzz` разделён по слоям clean architecture:

- `domain/` — сущности (`AccountRecord`, `FileHash`) и порты-трейты (`LogSource`,
  `ArchiveInbox`, `CredentialRepository`) + парсер. Ни от чего внешнего не зависит.
- `application/` — сценарии: `IngestService` (цикл импорта), `SearchService` (поиск),
  `SourceScheduler` (опрос источников). Зависит только от портов домена.
- `infrastructure/` — реализации портов: ClickHouse-репозиторий, извлечение архивов,
  файловый inbox, локальный источник, Telegram-IPC.
- `interface/` — точки входа: REST API, Telegram-бот, конфиг/CLI.

`main.rs` связывает слои: собирает реализации инфраструктуры и передаёт их сценариям
через трейты.

## Access control

Два места, которые по умолчанию **открыты для всех**, если явно не ограничить:

- **Telegram search bot** (`logzz`): без `LOGZZ_TELEGRAM_ALLOWED_USER_IDS` любой
  пользователь Telegram, нашедший бота, может выполнять `/url` и `/login` и получить
  полный доступ к импортированной базе учётных данных (включая пароли), а также
  загружать произвольные архивы через бота. Узнать свой `user_id` можно, например, у
  `@userinfobot`. Значение — список id через запятую:
  `LOGZZ_TELEGRAM_ALLOWED_USER_IDS=123456789,987654321`. При пустом значении `logzz`
  запускается (для обратной совместимости), но пишет громкое предупреждение в лог.
- **downloader REST API** (`/auth/*`): см. раздел выше про `DOWNLOADER_REST_API_TOKEN`.
- **logzz REST API** (`/api/*`, `/metrics`): без `LOGZZ_REST__API_TOKEN` любой, кто
  дотянется до порта, может искать по базе учёток и загружать архивы. См. раздел
  [REST API logzz](#rest-api-logzz).

Все варианты применяются только если соответствующие переменные окружения действительно
заданы — пустая конфигурация не ломает существующие локальные деплойменты, а лишь
предупреждает о риске.
