# Архитектура

Документ описывает реализованное поведение и составлен по коду ветки `pet-12-improvements`, версия 1.1.0 (`src/`, `Dockerfile`, `deploy/`). Решения и их обоснование — в [docs/adr/](adr/), контракт Paperclip API — в [paperclip-api.md](paperclip-api.md). Номера строк относятся к коммиту, на котором написан документ; при сомнении ищите по имени функции.

Доработки v1.1.0 (текстовые ответы как в чате, проверка версии Paperclip, host-network deployment, права secret-файлов) спроектированы в [ADR-0009](adr/0009-v1.1.0-improvements.md) и описаны ниже: маршрутизация текста — §4 «Telegram → Paperclip» и «Текст как чат», миграция draft — §3 и §4, проверка версии — §4 «Старт». Вариант с `network_mode: host` — `deploy/compose.host-network.example.yml`: схема выше относится к изолированному примеру, различия — в [THREAT_MODEL](THREAT_MODEL.md) и [RUNBOOK](RUNBOOK.md) §6.

## 1. Схема

```
                    приватная сеть                         сеть с выходом только на proxy
  ┌─────────────┐   (без proxy)    ┌─────────────────────────────────┐  SOCKS5/HTTP   ┌───────┐   TLS    ┌──────────────────┐
  │  Paperclip  │ ◄─────────────── │        bridge (один процесс)    │ ─────────────► │ proxy │ ───────► │ api.telegram.org │
  │ /api/...    │  только исходящие│  poller  sync  runs  outbox     │  только        │(Xray) │  сквозь  │   Bot API        │
  └─────────────┘                  │  health_writer   sender         │  исходящие     └───────┘  туннель └──────────────────┘
                                   │         │                       │
                                   │   SQLite WAL (/state)           │
                                   └─────────────────────────────────┘
```

- Входящих портов нет: в `Dockerfile` нет `EXPOSE`, ни в одном compose-примере нет `ports:`.
- Клиент Paperclip строится с `.no_proxy()` (`src/paperclip/http.rs`), клиент Telegram всегда с `Proxy::all(TELEGRAM_PROXY_URL)` (`src/telegram/http.rs`). Переменные `HTTP(S)_PROXY`/`ALL_PROXY` из окружения не читаются (`tests/env_proxy.rs`).
- TLS до `api.telegram.org` устанавливается клиентом bridge сквозь соединение с proxy (rustls, провайдер `ring`); proxy TLS не терминирует. Само соединение bridge → proxy при схемах `socks5`/`socks5h`/`http` не шифруется, шифрование там зависит от выбранной схемы и сети. Транспорт до Paperclip определяется схемой `PAPERCLIP_URL` (`http://` или `https://`).

## 2. Задачи сервиса

Процесс (`src/main.rs`, `src/app.rs`) запускает tokio-задачи:

| Задача | Файл | Что делает |
|---|---|---|
| `poller` | `core/poller.rs` | long polling `getUpdates` (`allowed_updates: message, callback_query`); каждый update обрабатывается по очереди через `dialog::handle_update` |
| `sync` | `core/sync.rs` | раз в `PAPERCLIP_POLL_INTERVAL` читает ленту `attention` каждой компании, находит новые карточки, доставляет их в Telegram, закрывает карточки, решённые в другом месте, дочитывает результат governed-действий |
| `runs` | `core/runs.rs` | раз в `PAPERCLIP_RUNS_POLL_INTERVAL` читает `heartbeat-runs`, сообщает о сбоях; при `NOTIFY_TASK_COMPLETED=true` — о завершённых задачах |
| `outbox` | `core/outbox.rs` | отправляет в Paperclip ответы пользователя из очереди |
| `health_writer` | `app.rs` | раз в 15 с пишет `HEALTH_FILE` |
| sender | `core/sender.rs` | единая очередь исходящих сообщений Telegram: не чаще 1 сообщения в секунду (`MESSAGE_INTERVAL`), ретраи, уважает `retry_after` |

Запросы `GET /companies/{id}/live-runs` выполняются только при команде `/status` (`core/commands.rs`), в фоне их никто не опрашивает. Полный перечень вызовов Paperclip: [PAPERCLIP_PERMISSIONS.md](PAPERCLIP_PERMISSIONS.md).

Модули: `config` (переменные окружения), `domain` (типы карточек, черновиков), `paperclip` (DTO и HTTP-клиент), `telegram` (тонкий клиент Bot API), `render` (карточки в HTML, `callback_data`), `core` (логика), `store` (SQLite), `health`, `retry`, `fault` (точки отказа для тестов, активны только с feature `fault-injection`), `logging`, `shutdown`.

## 3. Состояние в SQLite

`src/store/sqlite.rs`. Файл `STATE_PATH`, режим `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`, `busy_timeout=5000`, `temp_store=MEMORY` (корневая ФС контейнера read-only, пишется только `/state`). При старте выполняются `PRAGMA integrity_check` (не `ok` → отказ старта без автопочинки) и миграции по `PRAGMA user_version` (версия БД новее поддерживаемой → отказ). Одно соединение живёт в выделенном потоке, каждая операция `Store` — одна транзакция. При остановке выполняется `PRAGMA wal_checkpoint(TRUNCATE)`.

Таблицы (миграция 1, `sqlite.rs:16-70`):

| Таблица | Назначение | Ключевые поля |
|---|---|---|
| `meta` | служебные значения | `key`/`value`: `telegram_offset`, `runs_baseline_at`, `done_baseline_at`, `started_at`, `self_user_id` (`store/mod.rs:220-226`) |
| `deliveries` | карточка Paperclip ↔ сообщение Telegram | `interaction_id` UNIQUE, `company_id`, `issue_id`, `issue_ref`, `kind`, `snapshot_json`, `status`, `gen` (поколение клавиатуры), `closed_reason`, `follow_until` |
| `delivery_messages` | куски сообщения карточки | `(delivery_id, chunk)`, `message_id` |
| `drafts` | черновики форм и ожидание текста. Для однотекстовых карточек (см. «Текст как чат») draft не создаётся | `delivery_id`, `step`, `answers_json`, `awaiting`, `prompt_message_id` |
| `outbox` | очередь ответов в Paperclip | `delivery_id` UNIQUE, `action`, `body_json`, `status`, `attempts`, `next_attempt_at`, `last_error` |
| `seen_runs`, `seen_done_issues` | уже показанные сбои runs и завершённые задачи | хранятся 14 дней (`runs.rs: SEEN_RETENTION`) |

Статусы `deliveries.status`: `new`, `sending`, `sent`, `answering`, `answered`, `following`, `closed`, `unsupported`. Статусы `outbox.status`: `pending`, `in_flight`, `done`, `abandoned`. В БД **нет** токенов и ключей. В `body_json` лежит ответ пользователя; после завершения записи он обнуляется (`sqlite.rs:595-598`), но `snapshot_json` содержит текст карточки (вопросы агентов), поэтому файл состояния нужно защищать как конфиденциальный.

## 4. Потоки данных

### Старт (`src/app.rs: run`)

Порядок: открытие состояния и `recover()` (записи outbox `in_flight` возвращаются в `pending`) → **миграция draft** `Store::reset_chat_text_drafts()` (см. ниже) → создание клиентов → `GET /api/health` и **проверка версии** `paperclip::version::log_check` → запуск задач. Затем в фоне `startup_messages`: `setMyCommands`, уведомления о сброшенных draft, `announce_restart` (незавершённые формы).

**Проверка версии** (`src/paperclip/version.rs`, ADR-0009 §3). Единственное место с проверенной версией — константа `TESTED_PAPERCLIP_VERSION` (`2026.1106.0`). `classify` относит `version` из `/api/health` к одному из случаев: `Tested` (точное совпадение после `trim`) → INFO `paperclip reachable` с `version_status="tested"`; `Other` (формат `YYYY.MMDD.N`, но другое значение) → WARN «paperclip version differs from the tested one» с `paperclip_version` и `tested_version`; `Missing` (поля нет или пусто) → WARN «paperclip version is missing»; `Unrecognized` (не формат `YYYY.MMDD.N`) → WARN «paperclip version is not recognized». Значение от сервера в логе обрезается до 64 символов. Если `/api/health` не ответил, пишется прежний WARN `paperclip health check failed, continuing`, версия не проверяется. Запуск не блокируется ни в одном случае.

**Миграция draft** (ADR-0009 §2). В v1.0.0 для однотекстовой карточки создавался draft с preview и подтверждением; в v1.1.0 их нет. При каждом старте в одной транзакции удаляются draft карточек, у которых в snapshot ровно один вопрос с `mode == text`, независимо от статуса доставки, и `deliveries.gen` увеличивается на 1 (старые кнопки дают «Устарело»). В outbox ничего не пишется, текст draft не читается и **не отправляется**. Draft других карточек (форма из нескольких вопросов, Reject, вердикты) не трогаются. Для карточек в статусе `sent` `announce_chat_text_reset` перерисовывает карточку и шлёт сообщение «черновик сброшен, текст не отправлен»; ошибка перерисовки пишется в WARN и старт не останавливает. Повторный старт ничего не меняет: новый код такие draft не создаёт, отдельного флага в `meta` нет. Версия схемы БД не менялась. Тесты: `tests/draft_migration.rs`.

### Paperclip → Telegram (`sync::cycle`)

1. Повторная доставка карточек в статусах `new`/`sending` (Telegram был недоступен или процесс упал) — `sync.rs:111-120`.
2. Для каждой компании читается лента `attention` (до 10 страниц по 100, при `400` на курсоре — один перезапуск с первой страницы, `http.rs`).
3. Для задач с новыми или изменившимися элементами ленты читается `GET /issues/{id}/interactions`; берутся pending-карточки, которых ещё нет в `deliveries` (карточки вне ленты берутся только если адресат не другой пользователь/агент, `sync.rs:317-326`).
4. Новая карточка записывается в `deliveries` (`new`) и отправляется: `cards::deliver` ставит `sending`, шлёт все куски, затем в одной транзакции `mark_sent` (`core/cards.rs:14-56`).
5. Открытые карточки, выпавшие из ленты, перечитываются: если решены в другом месте — запись закрывается, сообщение обновляется (при `NOTIFY_RESOLVED_ELSEWHERE=true`).
6. При 3 подряд неудачах `sync` отправляет «Paperclip недоступен с ЧЧ:ММ», при 401/403 — «Ключ Paperclip отклонён», после успеха — «Связь восстановлена» (`sync.rs:57-105`).

### Telegram → Paperclip

1. `poller` получает update. `allowlist::check` пропускает только владельца в своём private-чате; чужое — `advance_offset` без ответа и без побочных эффектов (`dialog.rs:140-145`).
2. Нажатие кнопки или текст превращаются в `Effects` (маршрутизация текста — ниже); `commit_update` в **одной транзакции** применяет операции (черновик, постановка в outbox) и записывает `telegram_offset = update_id + 1` (`dialog.rs:161`, `sqlite.rs:148-170`). `update_id` меньше сохранённого offset считается дублем и пропускается.
3. Затем идут побочные эффекты (ответ на callback, обновление сообщения); их сбой не критичен, состояние уже согласовано.
4. `outbox` отправляет ответ (см. раздел 5) и закрывает карточку.

### Текст как чат (`dialog::on_message`, `forms::chat_text_answer`)

«Подходящая» карточка (`forms::is_chat_text_question`): статус `sent`, тело `ask_user_questions` из одного вопроса с `answerMode: text`. Для неё рендер не добавляет кнопок, а только подсказку «ответьте обычным сообщением» (`render/forms.rs`); любое нажатие кнопки со старой клавиатуры даёт «Устарело». Порядок разбора текстового сообщения после allowlist, нетекстовых вложений и команд, первое совпадение выигрывает:

1. reply на prompt ожидающего сценария → текст в этот сценарий (как в v1);
2. reply на карточку: подходящая → `chat_text_answer`; другая → прежний путь (`on_text`); не в `sent` → «уже закрыта или ответ отправлен»;
3. reply на что-то иное считается сообщением без reply;
4. draft, ожидающий текст (причина Reject, вердикта, шаг многовопросной формы) → текст в него;
5. открытые подходящие карточки (`deliveries_with_status(Sent)` + фильтр): одна → `chat_text_answer`; несколько → просьба ответить reply на нужную, список `issue_ref`, ничего не отправляется; ни одной → «Сейчас нет вопросов, на которые можно ответить текстом. Сообщение не отправлено.».

`chat_text_answer`: `validate_text` (длина, формат; нарушение даёт сообщение с причиной, а в состоянии ничего не меняется, вопрос открыт), затем `Effects::enqueue(Resolution::Respond { otherText })` + удаление остатка draft + сообщение «📨 … принят и поставлен в очередь отправки». Дальше путь тот же, что у любого ответа: `commit_update` в одной транзакции с offset, `outbox`, reconcile, защита `outbox.delivery_id UNIQUE` от второго ответа (§5). Опечатку отменить нельзя. Тесты: `tests/chat_text.rs`, `tests/crash.rs` (рестарт, повторная доставка), `docs/TESTING.md` «Пункт 1».

### Ошибки Paperclip при отправке ответа (`core/outbox.rs`, классификация — `paperclip/http.rs: classify`)

| Ответ Paperclip | Что делает bridge |
|---|---|
| 2xx | карточка `answered`, сообщение обновляется «✅ Ответ отправлен». Частичные вердикты (`request_item_verdicts`) оставляют карточку открытой |
| 409 с кодом `interaction_*` (в т.ч. `interaction_already_resolved`, `interaction_superseded`, `interaction_issue_closed`, `interaction_stale_target`) | reconcile: карточка перечитывается. Ещё pending → повтор позже. Решена **ключом владельца и результат совпадает с нашим ответом** → успех. Иначе → «Решено в Paperclip … ваш ответ не отправлен», карточка закрыта (`outbox.rs:299-344`) |
| 409 без кода, 5xx, 429, таймаут, ошибка соединения | повтор бессрочно с backoff (`RETRY_BASE_DELAY`…`RETRY_MAX_DELAY`, full jitter); для 429 учитывается `Retry-After`. После `RETRY_NOTIFY_AFTER`-й неудачи — одно сообщение «ответ в очереди» (`outbox.rs:347-389`) |
| 400, 422 | запись outbox удаляется, карточка возвращается пользователю с текстом ошибки сервера (до 300 символов), можно исправить и отправить снова (`outbox.rs:116-143`) |
| 401, 403 без audience-кода | повтор через 5 минут (`AUTH_RETRY_INTERVAL`), состояние «ключ отклонён», healthcheck unhealthy |
| 403 с кодом `interaction_human_only`, `interaction_addressee_mismatch`, `interaction_creator_excluded`, `interaction_governed_action_denied` | карточка закрывается: «Paperclip не разрешает этому ключу ответить…, ответьте в UI» (`outbox.rs:145-155`) |
| 404 | карточка закрывается: «Карточка не найдена в Paperclip» |

`RETRY_MAX_ATTEMPTS` для outbox не действует: он ограничивает только отправку в Telegram (`core/sender.rs:160`).

### Governed (`core/governed.rs`)

Решение принимается по перечитанному состоянию карточки, а не по HTTP-коду. В outbox перед запросом карточка читается (`governed.rs:218-224`): уже не pending → `settle` (наше решение — успех, чужое — «решено в Paperclip»). Для `destructive` при `confirm` сначала одобряется связанный formal approval (`GET /issues/{id}/approvals` → `POST /approvals/{id}/approve`), затем принимается карточка. При отказе `destructive` — reject карточки и затем reject связанного approval, если он ещё `pending`. Любая неясная ошибка → повторное чтение, не более 3 попыток (`MAX_UNCLEAR`), затем «Не удалось, ответьте в Paperclip». Принятое действие переходит в `following` и дочитывается до `executed`/`failed`/`expired` не дольше `GOVERNED_RESULT_FOLLOW`.

## 5. Падение посреди отправки (SIGKILL, потеря питания)

Вопрос: может ли kill -9 в момент отправки ответа потерять его или отправить дважды. Ответ: **нет**; дублироваться может только карточка (вопрос) в Telegram.

Порядок для обычного ответа (`confirmation`, `checkbox`, форма, вердикты, текст как чат):

| Шаг | Где | Состояние на диске |
|---|---|---|
| 1. Нажатие кнопки превращается в запись `outbox(pending)`; offset сдвигается **в той же транзакции** | `dialog.rs:161` → `commit_update` → `Enqueue` (`sqlite.rs:186-205`); `outbox.delivery_id UNIQUE` не даёт поставить второй ответ на ту же карточку | `outbox: pending`, `deliveries: answering` |
| 2. `outbox_in_flight` | `outbox.rs:73` | `outbox: in_flight` |
| 3. `POST …/interactions/{id}/accept\|reject\|respond\|verdicts` | `outbox.rs:77-80` (governed: `governed.rs:218-310`) | запись всё ещё `in_flight` |
| 4. Коммит `outbox_done` / `abandon` / `retry` | `outbox.rs:204-211`, `store/mod.rs:439` | `outbox: done`, `deliveries: answered` |

Точка краша, которую воспроизводит тест: между шагами 3 и 4 (`fault::AFTER_HTTP_BEFORE_OUTBOX_COMMIT`, `outbox.rs:81`). Раскладка по моменту падения:

| Когда упали | Что после рестарта |
|---|---|
| До шага 1 (update не закоммичен) | offset не сдвинут, Telegram присылает update снова, он обрабатывается как первый (`tests/crash.rs::crash_before_commit_of_update_loses_nothing`). Если коммит успел, повтор update отсекается по offset (`sqlite.rs:155-160`) |
| После шага 1, до POST | запись `pending` или `in_flight`; `recover()` при старте возвращает `in_flight` в `pending` (`sqlite.rs:126-131`, вызывается из `app.rs`), outbox отправит ответ |
| POST дошёл до сервера, коммита нет (между 3 и 4) | `recover()` → `pending`; повторный POST получает `409 interaction_already_resolved`; `reconcile` перечитывает карточку и, если её решил владелец ключа тем же результатом, засчитывает как успех без второго эффекта (`outbox.rs:299-323`, `result_matches` 236-281, `resolved_by_us` 290-297). Тест: `tests/crash.rs::crash_after_http_before_outbox_commit_sends_once` (ровно один POST после рестарта, очередь пуста) |
| После шага 4 | всё завершено |

Для governed-решений повтор безопасен иначе: перед POST карточка перечитывается и, если она уже решена, вызывается `settle` без нового запроса (`governed.rs:218-224`); а повторный accept/reject сервер, по контракту, отвечает `200` (`docs/paperclip-api.md` §6.6, ADR-0008). Пока запись висит в outbox, `sync` карточку не закрывает (`sync.rs:340`, статус `answering`).

Что может задублироваться: **карточка в Telegram**. `cards::deliver` шлёт `sendMessage`, затем коммитит `sent`. У `sendMessage` нет идемпотентности; если процесс упал между ними, запись остаётся `sending` и после рестарта карточка отправляется ещё раз (`core/cards.rs:12-13,44-47`, `sync.rs:111-120`, `tests/crash.rs::crash_after_send_before_commit_redelivers_the_card`). Пользователь увидит два сообщения, нажатие на любое приведёт к одному ответу (одна запись outbox на карточку). Это принятый компромисс (ADR-0004).

Потеря питания: `synchronous=FULL` + WAL, подтверждённая транзакция (offset, outbox) не откатывается. Что **не проверено**: поведение на реальной файловой системе хоста при потере питания и реальный SIGKILL контейнера (проверены только инъекции отказа в процессе тестов).

## 6. Остановка

SIGTERM/SIGINT отменяют общий `CancellationToken` (`shutdown.rs`). Дальше `app.rs`:

1. Ожидание завершения `poller`, `sync`, `runs`, `health_writer` до `SHUTDOWN_GRACE`; по истечении — предупреждение «tasks did not stop within the grace period».
2. Ожидание завершения `outbox` (доотправка записи в работе) до `SHUTDOWN_GRACE`; по истечении — «outbox did not finish within the grace period»; запись остаётся `in_flight` и вернётся в `pending` при следующем старте.
3. Остановка sender, `wal_checkpoint(TRUNCATE)`, финальная запись heartbeat.

Худший случай до шага 3 — `2 × SHUTDOWN_GRACE`, поэтому `stop_grace_period` в compose должен быть строго больше (см. README).

## 7. Healthcheck

См. README, раздел «Healthcheck», и [ADR-0007](adr/0007-healthcheck-heartbeat.md). Состояние пишет `health::write_file` (`health.rs`), проверку делает `health::check`; `paperclip_auth_ok=false` выставляется при 401/403 и сбрасывается первым успешным вызовом Paperclip.

## 8. Логи

Пишутся в stderr (`LOG_FORMAT=json|text`). Вызовы журналирования передают идентификаторы, виды, классы ошибок и длительности; тексты вопросов и ответов, payload карточек, тела запросов и токены не логируются. Токены хранятся как `SecretString`; ошибки reqwest сводятся к классу без URL (в URL Bot API есть токен). ID чужих пользователей заменяются хешем `sha256(id)[..4]` в hex (8 символов). Журнал решений для аудита: `decision from telegram queued` (`update_id`, `delivery_id`, `interaction_id`, `kind`, `risk`, `action`), `answer delivered to Paperclip`, `governed decision applied` (`outcome`).
