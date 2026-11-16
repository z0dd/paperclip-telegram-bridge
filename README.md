# paperclip-telegram-bridge

Сервис-мост между Paperclip и Telegram. Он пересылает владельцу вопросы и запросы подтверждения от агентов Paperclip (карточки `interactions`) в личный чат Telegram и отправляет ответы обратно в Paperclip.

Что важно знать сразу:

- Bridge делает **только исходящие соединения**: к Paperclip (напрямую, без proxy) и к Telegram (только через proxy из `TELEGRAM_PROXY_URL`, например Xray). Входящих портов и webhook нет, Paperclip в интернет не публикуется.
- Без `TELEGRAM_PROXY_URL` процесс не стартует; прямого пути к Telegram в коде нет (`src/telegram/http.rs`, ADR-0005).
- Состояние (offset Telegram, очередь ответов, черновики форм) хранится в SQLite в каталоге `/state`.
- Ответ пользователя сначала записывается в очередь (outbox) и только потом отправляется в Paperclip, поэтому он не теряется при рестарте и не дублируется (`docs/ARCHITECTURE.md`, раздел «Падение посреди отправки»). Единственное исключение: после краша карточка (вопрос) может быть показана в Telegram повторно.
- Лицензия: Apache-2.0.

## Документация

| Документ | О чём |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | схема, задачи сервиса, состояние в SQLite, потоки данных, анализ краша посреди отправки |
| [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) | активы, угрозы (STRIDE), остаточные риски |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | локальная сборка, запуск, структура кода |
| [docs/TESTING.md](docs/TESTING.md) | как запускать тесты, покрытие требований, docker smoke-test |
| [docs/RUNBOOK.md](docs/RUNBOOK.md) | эксплуатация: ротация и отзыв ключей, диагностика, остановка |
| [docs/BACKUP_RESTORE.md](docs/BACKUP_RESTORE.md) | backup и restore состояния |
| [docs/PAPERCLIP_PERMISSIONS.md](docs/PAPERCLIP_PERMISSIONS.md) | какие вызовы Paperclip API делает bridge |
| [docs/paperclip-api.md](docs/paperclip-api.md) | контракт Paperclip API (результат исследования) |
| [docs/adr/](docs/adr/) | решения по архитектуре (ADR-0001…0008) |

## Быстрый старт

Образ в реестры не публикуется: его собирает пользователь. Каталог `deploy/` содержит только пример.

### Что нужно подготовить

1. Бот в Telegram: создать у BotFather, сохранить токен. Для бота не должен быть установлен webhook (иначе `getUpdates` вернёт конфликт, см. [RUNBOOK](docs/RUNBOOK.md)).
2. Числовые `TELEGRAM_CHAT_ID` (личный чат с ботом) и `TELEGRAM_USER_ID` (владелец). Для личного чата обычно совпадают.
3. Board API key Paperclip того пользователя, который отвечает на вопросы (`pcp_board_…`). Как его выпустить и почему подходит только board key: `docs/paperclip-api.md` §2.
4. ID компаний Paperclip для `PAPERCLIP_COMPANY_IDS`.
5. Рабочий proxy до Telegram (`socks5h://…`, `socks5://…`, `http://…` или `https://…`), доступный из сети контейнера.

### Сборка и запуск

```bash
docker build -t paperclip-telegram-bridge:local .

mkdir -p secrets
umask 077
printf '%s' '<токен бота>'      > secrets/telegram_bot_token
printf '%s' '<board key>'        > secrets/paperclip_token

cp .env.example .env            # заполнить плейсхолдеры своими значениями
cp deploy/compose.example.yml compose.yml   # поправить имена внешних сетей

docker compose run --rm bridge check-config   # exit 0 и строка "configuration OK"
docker compose up -d
docker compose ps                              # статус healthy появится после start-period (30 с)
```

Секреты читаются **только из файлов** (`PAPERCLIP_TOKEN_FILE`, `TELEGRAM_BOT_TOKEN_FILE`); переменных с самими токенами нет намеренно. Не коммитьте `.env` и `secrets/`: они в `.gitignore` (`.env`, `*.key`), но каталог `secrets/` туда не входит, следите сами.

В `deploy/compose.example.yml` контейнер запускается с `read_only: true`, `user: 65532:65532`, `cap_drop: [ALL]`, `no-new-privileges`, без `ports:`, с именованным томом `bridge-state` на `/state`, и подключается только к внешним сетям `paperclip_internal` и `xray_egress` (имена-плейсхолдеры, замените на свои). Сети без маршрута в интернет — дополнительная защита от прямого выхода.

### Команды бинаря

`bridge [run|check-config|healthcheck|revoke-paperclip-key]` (`src/main.rs`). Бинарь лежит по пути `/usr/local/bin/bridge`, это ENTRYPOINT образа; в образе нет shell.

| Команда | Что делает | Exit |
|---|---|---|
| `run` (по умолчанию) | запускает сервис | 0 штатная остановка, 1 сбой, 2 неверная конфигурация |
| `check-config` | проверяет конфигурацию и собирает HTTP-клиенты без сетевых вызовов | 0 / 2 |
| `healthcheck` | читает heartbeat-файл, без сети (используется `HEALTHCHECK`) | 0 healthy / 1 unhealthy |
| `revoke-paperclip-key` | `POST /api/cli-auth/revoke-current` ключом из `PAPERCLIP_TOKEN_FILE` | 0 / 1 / 2 |

Примеры с compose: `docker compose run --rm bridge check-config`, `docker compose exec bridge /usr/local/bin/bridge healthcheck`.

## Конфигурация

Источник правды — `src/config.rs`. Значения по умолчанию ниже взяты оттуда. Длительности: формат `humantime` (`15s`, `2m`, `1h`), ноль запрещён. Логические значения: `true/1/yes`, `false/0/no`. Неизвестные переменные игнорируются. Сообщения об ошибках называют переменную и причину, но не значение.

### Обязательные

| Переменная | Описание |
|---|---|
| `PAPERCLIP_URL` | базовый URL Paperclip (`http://` или `https://`; путь-префикс поддерживается) |
| `PAPERCLIP_TOKEN_FILE` | файл с board API key (одна строка; пустой файл или несколько строк — ошибка) |
| `PAPERCLIP_COMPANY_IDS` | ID компаний через запятую; допустимы буквы, цифры, `-`, `_` |
| `TELEGRAM_BOT_TOKEN_FILE` | файл с токеном бота |
| `TELEGRAM_CHAT_ID` | числовой ID чата (allowlist) |
| `TELEGRAM_USER_ID` | числовой ID пользователя (allowlist) |
| `TELEGRAM_PROXY_URL` | proxy для Telegram: схемы `http`, `https`, `socks5`, `socks5h` (рекомендуется `socks5h`: DNS разрешает proxy). Может содержать логин и пароль, в логи и ошибки не попадает |

### Необязательные

| Переменная | По умолчанию | Описание |
|---|---|---|
| `PAPERCLIP_UI_URL` | пусто | базовый URL UI; если задан, идентификатор задачи в карточке становится ссылкой `<UI>/<PREFIX>/issues/<ID>`. Пусто — ссылок нет |
| `TELEGRAM_API_BASE_URL` | `https://api.telegram.org` | база Bot API (для тестов) |
| `STATE_PATH` | `/state/bridge.db` | файл SQLite; каталог должен быть доступен на запись (рядом `-wal`, `-shm`) |
| `HEALTH_FILE` | `/state/health.json` | heartbeat-файл |
| `HEALTH_MAX_STALE` | `5m` | сколько может не быть успешного вызова Telegram или Paperclip, прежде чем `healthcheck` вернёт unhealthy |
| `PAPERCLIP_POLL_INTERVAL` | `15s` | период опроса ленты `attention` |
| `PAPERCLIP_RUNS_POLL_INTERVAL` | `60s` | период опроса сбоев runs |
| `TELEGRAM_LONG_POLL_TIMEOUT` | `25s` | таймаут long polling, не больше `50s` |
| `PAPERCLIP_REQUEST_TIMEOUT` | `15s` | таймаут запроса к Paperclip |
| `PAPERCLIP_GOVERNED_TIMEOUT` | `120s` | таймаут запросов accept/reject governed-карточек |
| `TELEGRAM_REQUEST_TIMEOUT` | `15s` | таймаут запроса к Telegram |
| `CONNECT_TIMEOUT` | `10s` | таймаут соединения (оба клиента) |
| `RETRY_BASE_DELAY` | `1s` | начало экспоненциального backoff с полным jitter |
| `RETRY_MAX_DELAY` | `60s` | потолок backoff; не меньше `RETRY_BASE_DELAY` |
| `RETRY_MAX_ATTEMPTS` | `6` | число попыток **для отправки в Telegram** (`src/core/sender.rs`); ответы в Paperclip ретраятся бессрочно |
| `RETRY_NOTIFY_AFTER` | `5` | после какой по счёту неудачной попытки доставки ответа в Paperclip пользователь получает одно сообщение «ответ в очереди» |
| `NOTIFY_RUN_FAILURES` | `true` | сообщать о сбоях (`failed`/`timed_out`) агентов |
| `NOTIFY_TASK_COMPLETED` | `false` | сообщать о завершённых задачах |
| `NOTIFY_RESOLVED_ELSEWHERE` | `true` | обновлять карточку в Telegram, когда её решили в другом месте. При `false` запись в состоянии закрывается, но сообщение в чате не меняется |
| `GOVERNED_APPROVALS_ENABLED` | `true` | см. раздел «Governed-подтверждения» |
| `GOVERNED_DESTRUCTIVE_MODE` | `confirm` | `confirm` или `ui_only` |
| `GOVERNED_SHOW_ARGUMENTS` | `false` | показывать аргументы действия |
| `GOVERNED_RESULT_FOLLOW` | `30m` | как долго дочитывать результат принятого действия |
| `MAX_MESSAGE_CHUNKS` | `3` | максимум сообщений на одну карточку, минимум 1 |
| `SHUTDOWN_GRACE` | `10s` | см. «Остановка» |
| `LOG_LEVEL` | `info` | `error`, `warn`, `info`, `debug` (переменная `RUST_LOG` не читается) |
| `LOG_FORMAT` | `json` | `json` или `text`; логи пишутся в stderr |

## Что умеет бот

- Показывает карточки Paperclip: `ask_user_questions` (в том числе формы из нескольких вопросов, пошагово), `request_confirmation`, `request_checkbox_confirmation`, `request_item_verdicts`, а также governed-карточки (`toolAction`, `secretProposal`). Остальные виды и формы длиннее 20 вопросов приходят как уведомление «ответьте в Paperclip».
- Текстовый ответ: кнопка «✍️ Ответить текстом» или reply на сообщение карточки.
- Команды (регистрируются при старте через `setMyCommands`, `src/core/commands.rs`): `/status` (что ждёт ответа, задачи в работе, активные агенты, сбои за 24 ч, длина очереди), `/backlog` (топ-10 задач `todo`/`backlog` по приоритету), `/cancel` (отменить ввод текста), `/help`. Команду `/start` бот тоже понимает как `/help`.
- Сообщает о сбоях агентов, о недоступности Paperclip (после 3 подряд неудач) и о восстановлении связи, об отклонённом ключе (401/403).
- Файлы, фото и голос не поддерживаются (ответ «Файлы и голос не поддерживаются.»).

## Безопасность

### Включите 2FA в Telegram

Тот, кто получил доступ к вашему аккаунту Telegram (кража сессии, SIM-swap, открытое чужое устройство), управляет агентами через bridge от вашего имени: отвечает на вопросы и, если governed не отключён, разрешает действия инструментов и привязку секретов. Поэтому **включите облачный пароль (двухэтапную проверку) в Telegram и периодически проверяйте активные сессии**. PIN в чате не вводится: тот, кто видит чат, видел бы и его. Подробнее: [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md).

Allowlist: обрабатываются только updates `message` и `callback_query` от `TELEGRAM_USER_ID` в чате `TELEGRAM_CHAT_ID`; сообщение должно быть из private-чата. Остальное молча пропускается (без ответа), в лог пишется не чаще одной записи в 10 секунд (причина и короткий хеш ID, без текста) — `src/core/allowlist.rs`.

### Governed-подтверждения (`GOVERNED_*`)

Governed-карточки — это `request_confirmation` с блоком `toolAction` (агент просит выполнить действие подключённого инструмента) или `secretProposal` (привязать существующий секрет к агенту). Bridge никогда не отправляет `rememberAction`: постоянных правил доверия из Telegram нет.

| Переменная | Что делает (по коду) |
|---|---|
| `GOVERNED_APPROVALS_ENABLED=false` | аварийный выключатель: governed-карточки приходят **только уведомлением без кнопок** («Решите в Paperclip»), решать нужно в UI (`src/render/cards.rs`, `notice_reason`) |
| `GOVERNED_DESTRUCTIVE_MODE=confirm` (по умолчанию) | карточки с риском `destructive`: первое нажатие «Разрешить» открывает в том же сообщении второй шаг «🔴 Да, выполнить / Отмена» (живёт 2 минуты); затем bridge одобряет связанный formal approval и принимает карточку |
| `GOVERNED_DESTRUCTIVE_MODE=ui_only` | у `destructive`-карточек **нет кнопок**, решение только в Paperclip. Карточки с риском `write` и `read`, а также `secretProposal`, по-прежнему решаются из Telegram |
| `GOVERNED_SHOW_ARGUMENTS=true` | в карточку добавляется сводка аргументов `arguments_summary_json`, **подготовленная сервером Paperclip** (с его редактированием), обрезанная по длине. По умолчанию аргументы в Telegram не отправляются, только описание действия (`previewMarkdown`) |
| `GOVERNED_RESULT_FOLLOW=30m` | после принятия действия bridge дочитывает `result.toolAction.status` до `executed`/`failed`/`expired`, но не дольше этого срока; потом пишет «Результат не получен, проверьте в Paperclip» |

Перед отправкой решения bridge перечитывает карточку: если она уже решена, истекла или изменились `argumentsHash`/`target.revisionId`, решение не отправляется. Для `secretProposal` при отклонении причина обязательна. Срок `toolAction` задаёт сервер (по контракту 1 час, `docs/paperclip-api.md` §6.6); если bridge или proxy недоступны дольше, действие истечёт без решения. Поведение проверено contract-тестами на фикстурах (`tests/governed.rs`); **на живом сервере governed-карточки не проверялись** (см. ограничения).

### Ключи

Board key даёт **полный доступ пользователя-владельца** и бессрочный (решение пользователя, ADR-0002). Сузить его по правам нельзя. Меры: хранить только в файле секрета, быстро отзывать при подозрении. Процедуры: [docs/RUNBOOK.md](docs/RUNBOOK.md).

## Остановка (`SHUTDOWN_GRACE` и `stop_grace_period`)

При SIGTERM/SIGINT (`src/shutdown.rs`) `src/app.rs` делает два последовательных ожидания по `SHUTDOWN_GRACE`: сначала опрос Telegram, sync, runs и запись heartbeat, затем outbox (доотправка ответа, который уже в работе). В худшем случае это **2 × `SHUTDOWN_GRACE`**, плюс остановка отправщика сообщений и `PRAGMA wal_checkpoint(TRUNCATE)`, которые таймаутом не ограничены.

Правило: **`stop_grace_period` в compose строго больше `2 × SHUTDOWN_GRACE` с запасом.** По умолчанию `SHUTDOWN_GRACE=10s`, поэтому в `deploy/compose.example.yml` стоит `stop_grace_period: 30s`. Меняете `SHUTDOWN_GRACE` — пересчитайте (например, `15s` → не меньше `40s`).

Что будет, если контейнер всё же получил SIGKILL: незавершённая запись outbox после рестарта возвращается из `in_flight` в `pending` и отправляется повторно, повтор безопасен (раздел «Падение посреди отправки» в [ARCHITECTURE](docs/ARCHITECTURE.md)). Наблюдение из CI: при недоступных upstream `docker stop` в smoke-тесте занял 10,17 с при `SHUTDOWN_GRACE=10s` (run 43833870661, шаг 11), то есть почти весь grace; причина не исследована.

## Healthcheck

Сетевых запросов нет. Раз в 15 секунд сервис атомарно пишет `HEALTH_FILE` (`pid`, `started_at`, `written_at`, `last_telegram_ok_at`, `last_paperclip_ok_at`, `paperclip_auth_ok`). `bridge healthcheck` возвращает unhealthy, если файл старше 60 секунд или не читается, Paperclip ответил 401/403, либо нет успешного вызова Telegram или Paperclip дольше `HEALTH_MAX_STALE` (до первого успеха отсчёт идёт от старта процесса). `HEALTHCHECK` в Dockerfile: интервал 30 с, таймаут 3 с, start-period 30 с, 3 попытки.

## Известные ограничения v1

- **Бессрочный board key с полным доступом пользователя** — главный остаточный риск; принят пользователем. Ключ не сужается по правам. Его нет в состоянии и backup.
- **Governed из Telegram:** захват Telegram-аккаунта позволяет одобрять действия инструментов и привязку секретов. Ограничивается флагами `GOVERNED_*` и 2FA.
- **Governed-карточки не проверены на живом Paperclip:** в тестовой компании нельзя было создать такие карточки; поведение взято из кода сервера и проверено contract-тестами. Нужна ручная проверка при первом реальном запуске.
- **Ответ board-ключом не проверен экспериментом** на реальном сервере (нет ключа на этапе исследования); проверено на mock и по коду сервера (`docs/paperclip-api.md` §12).
- **Возможен повторный показ карточки в Telegram**, если процесс упал между отправкой сообщения и записью в SQLite. Ответ при этом не дублируется. Документированный компромисс (ADR-0004).
- **При первом запуске** (и после потери состояния) доставляются все карточки, которые сейчас есть в ленте `attention`; baseline есть только у уведомлений о сбоях runs и о завершённых задачах. Если новых карточек больше 10 за цикл, сначала приходит сводка «Новых вопросов: N».
- Лента `attention` схлопывает несколько подтверждений одной задачи до самого нового; остальные появятся после решения предыдущего.
- Срок `toolAction` 1 час: при длительной недоступности bridge или proxy действие истечёт.
- Формы больше 20 вопросов, `suggest_tasks`, `connection_intent`, `connectionAuthorization`, прочие approvals — только уведомление «ответьте в Paperclip».
- Bridge не пишет комментарии в Paperclip (комментарий board-ключом закрывает часть карточек).
- Содержание вопросов и ответов проходит через серверы Telegram. В логах содержания нет.
- Нет подкоманды backup: онлайн-копия делается внешним контейнером с `sqlite3` ([docs/BACKUP_RESTORE.md](docs/BACKUP_RESTORE.md)).
- Deployment на сервер пользователя в рамках v1 не выполнялся; образ нигде не опубликован.
