# Руководство разработчика

Команды ниже повторяют то, что выполняет CI (`.github/workflows/ci.yml`). Документ составлен без локального Rust-тулчейна и Docker (на машине автора их не было), поэтому подтверждением служит зелёный CI на том же коммите, а не локальный запуск.

## Требования

- Rust **1.100** (`rust-version = "1.100"` в `Cargo.toml`, CI использует toolchain `1.100.0`). Файла `rust-toolchain` в репозитории нет, версию ставьте сами.
- C-компилятор: SQLite собирается из исходников (`rusqlite` с feature `bundled`).
- Docker и `jq` — только для `scripts/docker-smoke.sh`.
- Прочего (OpenSSL, `cmake`) не нужно: TLS — `rustls` с провайдером `ring`, `aws-lc-sys` в зависимостях нет (ADR-0005).

## Сборка и проверки

```bash
cargo build                                          # бинарь target/debug/bridge
cargo fmt --all -- --check                           # job fmt
cargo clippy --all-targets --all-features -- -D warnings   # job clippy
cargo test --all-features                            # job test (включая crash-тесты)
cargo deny check all                                 # job deny (нужен cargo-deny; настройки в deny.toml)
docker build -t paperclip-telegram-bridge:test . && bash scripts/docker-smoke.sh   # job docker
```

Запуск тестов и их покрытие: [TESTING.md](TESTING.md).

## Локальный запуск

Переменные окружения из `.env` самим бинарём **не загружаются** (`.env.example` — просто шаблон для `docker compose`/`env_file`). Задавайте переменные в оболочке:

```bash
mkdir -p /tmp/bridge-dev && umask 077
printf '%s' '<токен бота>' > /tmp/bridge-dev/tg
printf '%s' '<board key>'  > /tmp/bridge-dev/pc

export PAPERCLIP_URL=http://127.0.0.1:3100
export PAPERCLIP_TOKEN_FILE=/tmp/bridge-dev/pc
export PAPERCLIP_COMPANY_IDS=<id компании>
export TELEGRAM_BOT_TOKEN_FILE=/tmp/bridge-dev/tg
export TELEGRAM_CHAT_ID=<id чата> TELEGRAM_USER_ID=<id пользователя>
export TELEGRAM_PROXY_URL=socks5h://127.0.0.1:1080
export STATE_PATH=/tmp/bridge-dev/bridge.db HEALTH_FILE=/tmp/bridge-dev/health.json
export LOG_FORMAT=text LOG_LEVEL=debug       # RUST_LOG не читается

cargo run -- check-config     # проверка без сети
cargo run -- run
```

Значения по умолчанию для `STATE_PATH` и `HEALTH_FILE` (`/state/...`) на хосте недоступны для записи, поэтому переопределите их, как выше. Proxy обязателен и при локальном запуске: без него процесс не стартует. Не используйте реальные секреты в файлах репозитория и не коммитьте их.

## Структура

```
src/
  main.rs           подкоманды run / check-config / healthcheck / revoke-paperclip-key, коды выхода
  app.rs            сборка сервиса, запуск задач, остановка
  config.rs         переменные окружения, SecretString, ProxyUrl
  health.rs         heartbeat-файл и его проверка
  shutdown.rs       SIGTERM/SIGINT -> CancellationToken
  retry.rs          backoff с jitter
  fault.rs          точки отказа для тестов (feature fault-injection)
  logging.rs        tracing, хеш ID, провайдер TLS
  timeutil.rs       время и форматирование
  domain/           типы карточек, черновиков, задач, runs (каталог)
  paperclip/        PaperclipApi (trait), DTO, HttpPaperclipClient
  telegram/         TelegramApi (trait), типы Bot API, HttpTelegramClient
  render/           карточки в HTML, клавиатуры, callback_data (каталог)
  store/            Store и SQL (sqlite.rs)
  core/             poller, dialog, sync, runs, outbox, sender, governed, forms, commands, allowlist, cards
tests/              интеграционные тесты; tests/support — общая обвязка (mock Paperclip на wiremock, fake Telegram, proxy)
scripts/            docker-smoke.sh, check-testing-refs.sh
deploy/             compose.example.yml (пример)
docs/               документация, adr/
```

Приёмы, которые встретите в коде:

- `PaperclipApi` и `TelegramApi` — traits с методами `-> impl Future`; боевая реализация — `Http*Client`, в тестах — mock-серверы wiremock и fake-реализации (`#[async_trait]` не используется).
- `core` не знает про SQLite: только `Store` и его методы, каждый — одна транзакция.
- Любое журналирование передаёт идентификаторы, виды и классы ошибок, но не тексты и не URL. Ошибки reqwest никогда не форматируются как есть (в URL Telegram — токен): используйте `.without_url()` и сводите к классу.

## Типовые изменения

| Задача | Что трогать |
|---|---|
| Новая переменная окружения | `src/config.rs` (разбор, значение по умолчанию, тест в том же файле), `.env.example`, таблица в `README.md` |
| Новый вызов Paperclip | метод в `PaperclipApi` (`src/paperclip/mod.rs`), реализация в `http.rs`, DTO в `dto.rs`, тест в `tests/paperclip_contract.rs`, строка в `docs/PAPERCLIP_PERMISSIONS.md` |
| Изменение схемы БД | новая запись в `MIGRATIONS` и `SCHEMA_VERSION` в `src/store/sqlite.rs`; существующие миграции не редактировать |
| Новое поведение карточек | `src/render/` и `src/core/`, тест в `tests/core_flow.rs`, `forms_flow.rs` или `governed.rs` |
| Изменение таблицы покрытия | `docs/TESTING.md`; `bash scripts/check-testing-refs.sh` проверяет, что каждая ссылка «файл::тест» существует |

## Правила репозитория

Вся работа ведётся в ветке `feat/bridge-v1` и одном PR; прямые push в `main` запрещены, merge делает только PET Lead. В коммитах и документах нет секретов, реальных адресов и имён инфраструктуры — только плейсхолдеры (`paperclip.internal`, `xray`, `123456789`).
