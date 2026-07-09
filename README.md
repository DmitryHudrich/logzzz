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

Оба варианта проверены тестами в `logzz/src/config.rs`, но применяются только если
переменные окружения действительно заданы — пустая конфигурация не ломает существующие
локальные деплойменты, а лишь предупреждает о риске.
