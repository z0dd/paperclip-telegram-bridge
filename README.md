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
| [docs/RUNBOOK.md](docs/RUNBOOK.md) | эксплуатация: ротация и отзыв ключей, права секретов, диагностика, остановка, развёртывание host-network |
| [docs/BACKUP_RESTORE.md](docs/BACKUP_RESTORE.md) | backup и restore состояния |
| [docs/PAPERCLIP_PERMISSIONS.md](docs/PAPERCLIP_PERMISSIONS.md) | какие вызовы Paperclip API делает bridge |
| [docs/paperclip-api.md](docs/paperclip-api.md) | контракт Paperclip API (результат исследования) |
| [docs/adr/](docs/adr/) | решения по архитектуре (ADR-0001…0009) |

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

# файлы секретов: см. «Хранение секретов (rootless Docker)» ниже;
# каталог secrets/ в .gitignore, права и владелец файлов важны

cp .env.example .env            # заполнить плейсхолдеры своими значениями
cp deploy/compose.example.yml compose.yml   # поправить имена внешних сетей

# если Paperclip и Xray слушают loopback хоста: cp deploy/compose.host-network.example.yml compose.yml
docker compose run --rm bridge check-config   # exit 0 и строка "configuration OK"
docker compose up -d
docker compose ps                              # статус healthy появится после start-period (30 с)
```

Секреты читаются **только из файлов** (`PAPERCLIP_TOKEN_FILE`, `TELEGRAM_BOT_TOKEN_FILE`); переменных с самими токенами нет намеренно. Каталог `secrets/` и `.env` уже в `.gitignore`, ответственность за «не закоммитить» на пользователе не лежит. CI (job `gitignore-secrets`) проверяет, что `git check-ignore` возвращает оба пути `secrets/telegram_bot_token` и `secrets/paperclip_token`, и падает, если правило убрать (`scripts/gitignore-check-test.sh`). Не кладите в репозиторий реальные или тестовые токены.

#### Хранение секретов (rootless Docker)

В rootless Docker рекомендуется **строгий вариант** прав:

```sh
install -d -m 0700 secrets
( umask 077; printf '%s' '<telegram bot token>' > secrets/telegram_bot_token )
( umask 077; printf '%s' '<paperclip token>'     > secrets/paperclip_token )
docker run --rm --network none -v "$PWD/secrets:/s" busybox:1.37 \
  sh -c 'chown 65532:65532 /s/telegram_bot_token /s/paperclip_token && chmod 0400 /s/telegram_bot_token /s/paperclip_token'
```

Контейнер в составе bridge запускается с UID 65532. Файлы `0400` собственные этому UID, хост-пользователь их не читает (это дополнительная защита). **Ротация:** удалить файл, записать новый, повторить рецепт.

**Допустимый вариант** (проще, менее строго): `chmod 0644` для файлов в каталоге `0700` хост-пользователя:

```sh
install -d -m 0700 secrets
( umask 077; printf '%s' '<telegram bot token>' > secrets/telegram_bot_token )
( umask 077; printf '%s' '<paperclip token>'     > secrets/paperclip_token )
chmod 0644 secrets/telegram_bot_token secrets/paperclip_token
```

**Не используйте:** `chmod 0400`, `0600`, `0640` для файла, владелец которого хост-пользователь: контейнер с UID 65532 такой файл не прочитает, и bridge не стартует. Проверяет job `rootless-secrets` (`scripts/secrets-rootless-test.sh`): в rootless Docker UID 65532 контейнера соответствует host-UID `subuid_start + 65531`, и матрица прав (строгий вариант, `0400`, `0600`, `0640`, `0644`) сверяется с ожидаемой; при расхождении job падает. Rootful Docker не проверялся.

В `deploy/compose.example.yml` контейнер запускается с `read_only: true`, `user: 65532:65532`, `cap_drop: [ALL]`, `no-new-privileges`, `tmpfs` на `/tmp`, без `ports:`, с именованным томом `bridge-state` на `/state` и `stop_grace_period: 30s`, и подключается только к внешним сетям `paperclip_internal` и `xray_egress` (имена-плейсхолдеры, замените на свои). Сети без маршрута в интернет — дополнительная защита от прямого выхода.

#### Вариант для хоста с Paperclip и Xray на loopback (`network_mode: host`)

Файл `deploy/compose.host-network.example.yml` рассчитан на хост, где Paperclip слушает `http://127.0.0.1:3100`, а Xray отдаёт HTTP proxy на `http://127.0.0.1:10808` (значения в файле — примеры, поправьте под себя). Контейнер работает с `network_mode: host`, `PAPERCLIP_URL` и `TELEGRAM_PROXY_URL` заданы прямо в `environment` (они важнее `.env`), входящих портов нет. Hardening тот же, что в изолированном примере: `read_only`, `65532:65532`, `cap_drop: [ALL]`, `no-new-privileges`, `tmpfs /tmp`, отдельный том `bridge-state`, `stop_grace_period: 30s`. Секреты готовятся так же, как выше. Это отдельный файл, а не override: `network_mode` несовместим с `networks`, поэтому наложить его на первый пример нельзя без `!reset`.

**Компромисс.** При `network_mode: host` у контейнера нет сетевой изоляции: у него есть прямой маршрут в интернет и доступ ко всем loopback-сервисам хоста. Сетевая защита изолированного варианта (сети без маршрута наружу) здесь теряется. Остаётся защита на уровне приложения: клиент Telegram всегда идёт через `TELEGRAM_PROXY_URL` и обходного пути не имеет (при недоступном proxy запросов к Telegram нет вообще), переменные `HTTP(S)_PROXY` не читаются, клиент Paperclip работает без proxy. Ограничение egress на хосте (firewall) вне поставки и не проверялось. Подробнее: [THREAT_MODEL](docs/THREAT_MODEL.md), [RUNBOOK](docs/RUNBOOK.md) §6. В rootless Docker `network_mode: host` видит loopback хоста, если демон запущен с `--detach-netns` (проверено на Docker 29.10.0 в CI); в старых установках это не так.

CI проверяет оба примера: `docker compose config` с сравнением hardening (job `compose-config`) и запуск (`scripts/compose-smoke.sh`, job `docker`). Живой деплой на сервер пользователя не выполнялся.

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
- Текстовые ответы работают как чат, см. раздел «Текстовые ответы» ниже.
- Команды (регистрируются при старте через `setMyCommands`, `src/core/commands.rs`): `/status` (что ждёт ответа, задачи в работе, активные агенты, сбои за 24 ч, длина очереди), `/backlog` (топ-10 задач `todo`/`backlog` по приоритету), `/cancel` (отменить ввод текста), `/help`. Команду `/start` бот тоже понимает как `/help`.
- Сообщает о сбоях агентов, о недоступности Paperclip (после 3 подряд неудач) и о восстановлении связи, об отклонённом ключе (401/403).
- Файлы, фото и голос не поддерживаются (ответ «Файлы и голос не поддерживаются.»).

## Текстовые ответы

Применяется к карточке `ask_user_questions` **ровно из одного вопроса со свободным текстом** (без вариантов). У такой карточки нет кнопок, только подсказка «Ответьте обычным сообщением в этот чат (или reply на эту карточку)». Порядок разбора входящего сообщения (`src/core/dialog.rs: on_message`):

1. Reply на prompt сценария, ожидающего текст (причина Reject, причина вердикта, текст на шаге многовопросной формы, «свой вариант»): текст уходит в этот сценарий, как раньше.
2. Reply на карточку: если карточка однотекстовая и открыта, ответ **сразу** ставится в outbox и отправляется в Paperclip, без preview и подтверждения; если карточка другого вида, поведение прежнее; если закрыта, ответ «Карточка … уже закрыта или ответ отправлен».
3. Сообщение без reply (или reply на сообщение, которое не карточка): если бот уже ждёт текст в другом сценарии, текст уходит туда; иначе, если открыта **ровно одна** однотекстовая карточка, это ответ на неё; если открыто несколько, бот пишет «Открыто несколько вопросов: … Ответьте reply на нужную карточку» и ничего не отправляет; если подходящих карточек нет, бот отвечает «Сейчас нет вопросов, на которые можно ответить текстом. Сообщение не отправлено.» и не создаёт ни задачу, ни комментарий.
4. Текст, нарушающий ограничения вопроса (`textValidation`: пустой, длина, формат), не отправляется: бот называет причину, вопрос остаётся открытым.

После постановки в очередь бот пишет «📨 Ответ на … принят и поставлен в очередь отправки», после доставки карточка помечается «✅ Ответ отправлен». Allowlist, outbox, повторы и защита от повторного ответа работают как для остальных карточек.

**Опечатку исправить нельзя:** ответ уходит сразу, отменить его через bridge невозможно (осознанное решение пользователя). Варианты выбора, формы из нескольких вопросов (кнопка «✍️ Ответить текстом» там остаётся), Approve/Reject и двухэтапное destructive-подтверждение не изменились.

**Обновление с v1.0.0.** При первом запуске новой версии незавершённый draft однотекстовой карточки сбрасывается, карточка перерисовывается без кнопок, в чат приходит «🔄 Bridge обновлён: черновик ответа на … сброшен, текст не отправлен». Введённый, но не подтверждённый текст **не отправляется**. Остальные draft не трогаются (ARCHITECTURE §4).

## Проверка версии Paperclip

При старте bridge читает `version` из `GET /api/health` и сравнивает её с проверенной версией (константа `TESTED_PAPERCLIP_VERSION` в `src/paperclip/version.rs`, сейчас `2026.1106.0`). Совпадение: обычная запись INFO. Другая версия, отсутствующая или не формата `YYYY.MMDD.N`: WARN с версией сервера и проверенной. **Запуск не блокируется ни в одном случае.** Подробнее: [docs/paperclip-api.md](docs/paperclip-api.md) §3.

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
