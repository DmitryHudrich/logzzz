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

3. Запусти сервисы:

```bash
docker compose up --build
```

4. Для первого запуска `downloader` авторизуй Telegram-сессию через REST:

```bash
curl http://127.0.0.1:8090/auth/status
curl -X POST http://127.0.0.1:8090/auth/request-code \
  -H 'Content-Type: application/json' \
  -d '{"phone":"+79990000000"}'
curl -X POST http://127.0.0.1:8090/auth/submit-code \
  -H 'Content-Type: application/json' \
  -d '{"code":"12345"}'
```

5. Если на аккаунте включён 2FA:

```bash
curl -X POST http://127.0.0.1:8090/auth/submit-password \
  -H 'Content-Type: application/json' \
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

Поддержка `config.yaml` в бинарниках сохранена только как опциональный override для ручного запуска вне compose.

Если нужно начать авторизацию `downloader` с нуля:

```bash
rm -f ./.local/downloader/downloader.session
docker compose up --build
```

## REST API downloader

`downloader` поднимает HTTP API по адресу `DOWNLOADER_REST_LISTEN_ADDR`.

По умолчанию:

- внутри compose: `0.0.0.0:8090`
- с хоста: `http://127.0.0.1:8090`
