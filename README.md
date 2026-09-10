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
- `GET /api/search?type=url|login&q=<запрос>&tags=<t1,t2>&exclude_tags=<t3>&complete=1&page=<n>`
  — поиск с пагинацией, JSON. Фильтры:
  - `tags=vip,checked` — только записи, у которых есть **все** перечисленные теги (AND);
  - `exclude_tags=seen` — **скрыть** записи, у которых есть **любой** из указанных тегов
    (например, помеченные `seen`);
  - `complete=1` — только «полные» записи, где есть url **и** логин **и** пароль.
  Достаточно указать хотя бы один из `q` / `tags` / `exclude_tags`.
- `GET /api/import/status` — статус фонового импортёра (последний цикл, суммарные
  счётчики, очередь необработанных архивов).
- `GET /api/events` — поток событий импорта в реальном времени (SSE): новый импорт,
  стадии обработки архивов, проблемы. Токен — через `?token=` (см. ниже).
- `POST /api/archives` — загрузка архива (`multipart/form-data`, поле `file`); файл
  кладётся в inbox и импортируется в фоне.
- `GET /api/archives/queue` — очередь архивов в inbox со статусом
  (`queued` / `needs_password` / `password_submitted`).
- `POST /api/archives/password` — пароль для зашифрованного архива
  (`{"archive":"<имя-в-inbox>","password":"..."}`), кладёт `.pass` рядом с архивом.
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

## Веб-интерфейс

`logzz` REST-сервер отдаёт современный веб-интерфейс прямо на корневом пути. Если REST
включён (`LOGZZ_REST__LISTEN_ADDR`), открой в браузере:

```
http://127.0.0.1:8091/
```

Что умеет:

- поиск по URL или логину с пагинацией;
- фильтр по тегам (кликабельные чипы, клик циклирует: включить (AND) → исключить →
  сброс). Включённые теги сужают выдачу до записей со всеми ними; исключённые скрывают
  записи, у которых есть такой тег (удобно помечать просмотренные тегом `seen` и
  скрывать их);
- чекбокс «только полные записи» — показывать только те, где есть url, логин и пароль;
- на каждой карточке результата — снятие тегов (×), выбор существующего тега в один
  клик из меню «＋ тег», и ввод нового тега; всё сразу пишется в БД;
- показать/скопировать пароль, развернуть все исходные пути;
- сводка сверху (сколько учёток/файлов, очередь импорта);
- загрузка архивов drag&drop или кнопкой, с прогресс-баром: показывает передачу
  (%), затем стадии обработки на бэкенде (в очереди → распаковка → готово · N записей);
- живые уведомления через Server-Sent Events (`GET /api/events`): тосты в странице и
  системные push-уведомления (кнопка «🔔») при новом импорте, завершении обработки
  архива и проблемах (нужен пароль / ошибка извлечения / сбой цикла). Индикатор
  «● live» в области загрузки показывает состояние стрима;
- очередь архивов: панель показывает все архивы в inbox со статусом (в очереди →
  распаковка → готово, либо «нужен пароль»), подтягивается при загрузке страницы и
  раз в 5с (`GET /api/archives/queue`), поэтому видна и без открытого стрима;
- ввод пароля прямо в UI: если архив зашифрован, в его строке появляется поле для
  пароля — вводишь и импортёр повторяет распаковку (эндпоинт
  `POST /api/archives/password`, кладёт `.pass` рядом с архивом). Работает и для
  архивов, залитых раньше: они находятся через очередь, а не только по live-событию.

`/api/events` — единственный эндпоинт, который принимает токен через query-параметр
(`?token=...`), потому что браузерный `EventSource` не умеет слать заголовки; передаёт
только метаданные импорта (счётчики и имена архивов), не сами учётки.

Интерфейс — статическая страница (`frontend/index.html`), встроенная в бинарник
(`include_str!`) и отдаваемая самим Rust-сервером тем же origin, что и API, поэтому CORS
не нужен и никаких доп. процессов не требуется. Данные берутся из тех же `/api/*`
эндпоинтов.

Если REST защищён `LOGZZ_REST__API_TOKEN`, введи токен кнопкой «⚙ токен» — он хранится в
`localStorage` и уходит как `Authorization: Bearer` только на этот же сервер.

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

## Хранение и компрессия ClickHouse

Схема оптимизирована под минимальный объём на диске:

- **Кодеки сжатия.** Текстовые колонки (`url_raw`, `username_raw`, `password_raw`,
  `extra_json`, пути, теги, хэши) сжимаются `ZSTD(3)` вместо дефолтного `LZ4` — для
  такого текста это обычно в разы плотнее. Метки времени идут через
  `DoubleDelta + ZSTD`, числовые размеры — через `T64 + ZSTD`.
- **Сортировка для локальности.** `creds` теперь сортируется по
  `(host_root, url_raw, username_raw, password_raw, ingest_time)`: одинаковые и близкие
  записи лежат рядом, поэтому сжимаются заметно лучше (и группировка при поиске дешевле).
- **LowCardinality** для колонок с малым числом значений (`source_file`,
  `parse_status`, `host_*`).

Как это применяется:

- `migrations/001_init.sql` и `002_tags.sql` создают таблицы уже с кодеками — новые
  инсталляции получают всё из коробки.
- `migrations/003_storage_codecs.sql` навешивает кодеки на уже существующие таблицы
  через `ALTER ... MODIFY COLUMN` (идемпотентно).

Важно про существующие данные: `MODIFY COLUMN ... CODEC` — это изменение метаданных.
Новые вставки сразу пишутся с новым сжатием, а старые куски пережимаются постепенно при
фоновых слияниях. Чтобы пережать сразу и освободить место немедленно:

```sql
OPTIMIZE TABLE logzz.creds FINAL;
OPTIMIZE TABLE logzz.source_file_paths FINAL;
OPTIMIZE TABLE logzz.source_files FINAL;
OPTIMIZE TABLE logzz.cred_tags FINAL;
```

Посмотреть выигрыш (сжатый vs несжатый размер по колонкам):

```sql
SELECT table, formatReadableSize(sum(data_compressed_bytes)) AS compressed,
       formatReadableSize(sum(data_uncompressed_bytes)) AS uncompressed,
       round(sum(data_uncompressed_bytes) / sum(data_compressed_bytes), 2) AS ratio
FROM system.columns
WHERE database = 'logzz'
GROUP BY table ORDER BY table;
```

## Сборка Docker-образа

`Dockerfile` использует [cargo-chef](https://github.com/LukeMathWalker/cargo-chef) и
BuildKit-кеши, чтобы не пересобирать зависимости при каждом изменении кода:

- стадия `planner` строит `recipe.json` (слепок зависимостей);
- стадия `builder` сначала `cargo chef cook` собирает только зависимости (этот слой
  кешируется и переиспользуется, пока не менялись `Cargo.toml`/`Cargo.lock`), затем
  собирает сами крейты;
- `--mount=type=cache` кеширует реестр crates.io и `target/` между сборками.

Нужен BuildKit (включён по умолчанию в современных Docker и `docker compose`). Первая
сборка долгая (компилит все зависимости), последующие пересборки после правок кода —
быстрые. Отдельно ничего настраивать не надо: `docker compose up --build` уже это
использует.

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
