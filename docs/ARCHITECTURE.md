# Архитектура

Документ описывает реализованное поведение версии 2.0.0 и составлен по коду `main` на коммите `194eae5` (`src/`, `Dockerfile`, `deploy/`). Решения и их обоснование — в [ADR-0010](adr/0010-v2-forum-bridge.md) и остальных [ADR](adr/), контракт Paperclip API — в [paperclip-api.md](paperclip-api.md). Ссылки на код даны как «файл: функция», без номеров строк. Версия v1 (карточки, governed, формы) описана в истории git, тег `v1.1.0`.

Если код расходится с ADR-0010, здесь описан код, а отличие отмечено пометкой «Отличие от ADR-0010».

## 1. Схема

```
                 приватная сеть (без proxy)                                     только исходящие, через proxy
 ┌─────────────┐  GET companies, agents, chats,  ┌───────────────────────────────────────┐  SOCKS5/HTTP  ┌───────┐  TLS сквозь  ┌──────────────────┐
 │  Paperclip  │  comments, heartbeat-runs/{id}  │        bridge (один процесс)          │ ────────────► │ proxy │ ───────────► │ api.telegram.org │
 │  /api/...   │ ◄────────────────────────────── │ discovery+outbound  sender+topics     │               │(Xray) │   туннель    │    Bot API       │
 └─────────────┘  POST issues/{id}/comments      │ poller+inbound  pc_outbox  checks     │               └───────┘              └──────────────────┘
                                                 │ health_writer     SQLite WAL (/state) │                                               │
                                                 └───────────────────────────────────────┘                                               ▼
                                                                         приватная супергруппа с темами: «PET · PET Lead», «NN · Lead», …
```

- Одна супергруппа с включёнными темами (`TELEGRAM_CHAT_ID`) обслуживает все компании, которые видят board-ключи. Одна беседа Agent Chat соответствует одной теме «`<issuePrefix>` · `<имя агента>`» (`render: topic_name`, обрезка до 128 символов).
- Входящих портов нет: в `Dockerfile` нет `EXPOSE`, в compose-примерах нет `ports:`.
- Клиент Paperclip строится с `.no_proxy()` (`paperclip/http.rs: HttpPaperclipClient::new`), по одному клиенту на ключ. Клиент Telegram всегда строится с `Proxy::all(TELEGRAM_PROXY_URL)` (`telegram/http.rs`), другого конструктора нет. Переменные `HTTP(S)_PROXY`/`ALL_PROXY` из окружения не читаются (`tests/env_proxy.rs`).
- TLS до `api.telegram.org` устанавливает сам bridge сквозь соединение с proxy (rustls, провайдер `ring`), proxy TLS не терминирует. Соединение bridge → proxy при схемах `socks5`/`socks5h`/`http` не шифруется. Транспорт до Paperclip определяется схемой `PAPERCLIP_URL`.

## 2. Задачи и модули

Процесс (`main.rs`, `app.rs: run`) запускает tokio-задачи:

| Задача | Файл | Что делает |
|---|---|---|
| `discovery` (с `outbound`) | `core/discovery.rs`, `core/outbound.rs` | цикл раз в `PAPERCLIP_POLL_INTERVAL` (5 с). Раз в `PAPERCLIP_DISCOVERY_INTERVAL` (60 с) обновляет список компаний каждого ключа и имена агентов. В каждом цикле запрашивает `/chats` каждой компании, регистрирует новые беседы, отмечает пропавшие и читает комментарии тех бесед, которым пора (§4.3). Бэкофф отдельный для каждой компании |
| `sender` (с `topics`) | `core/sender.rs`, `core/topics.rs` | единственная очередь вызовов Telegram, которые меняют чат: сообщения bridge, `createForumTopic`, `reopenForumTopic`, куски ответов из `tg_outbox`. Не чаще одного вызова в `TELEGRAM_GROUP_MIN_INTERVAL` (3 с, то есть 20 в минуту на группу). `retry_after` ставит на паузу всю очередь |
| `poller` (с `inbound`) | `core/poller.rs`, `core/inbound.rs` | long polling `getUpdates` (`allowed_updates: ["message"]`). Обновления обрабатываются по одному, по порядку `update_id`. Подходящее сообщение попадает в `pc_outbox` в одной транзакции со сдвигом offset |
| `pc_outbox` | `core/outbox.rs` | публикует сообщения пользователя: `POST /api/issues/{id}/comments` с детерминированным `clientRequestId` |
| `checks` | `core/checks.rs` | проверка супергруппы при старте (§4.1). Работает, пока проверка не пройдёт, затем завершается |
| `health_writer` | `app.rs: health_writer` | раз в 15 с пишет `HEALTH_FILE`, обновляет `queue_depth`, пишет WARN при очереди больше 60 |

Модули: `config` (переменные окружения, секреты из файлов), `domain` (компания, беседа, комментарий; правило пересылки `domain/comment.rs: should_forward` и распознавание служебных сообщений), `paperclip` (HTTP-клиент; `dto.rs` — единственное место, которое знает JSON Paperclip; `version.rs` — проверка версии), `telegram` (тонкий клиент Bot API, классификация ошибок тем), `render` (Markdown → Telegram HTML в `render/markdown.rs`, нарезка в `render/text.rs`, названия тем, тексты сообщений bridge), `core` (логика), `store` (SQLite), `health`, `retry`, `fault` (точки отказа для тестов, только с feature `fault-injection`), `logging`, `shutdown`.

Все вызовы Paperclip перечислены в [PAPERCLIP_PERMISSIONS.md](PAPERCLIP_PERMISSIONS.md).

## 3. Состояние в SQLite

`store/sqlite.rs`. Файл `STATE_PATH` (по умолчанию `/state/bridge.db`). Режимы: `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`, `busy_timeout=5000`, `temp_store=MEMORY` (корневая ФС контейнера read-only, запись возможна только в `/state`). При открытии выполняется `PRAGMA integrity_check`: если результат не `ok`, старт отклоняется без автопочинки. Миграции идут по `PRAGMA user_version`: если версия БД новее поддерживаемой, старт тоже отклоняется. Одно соединение живёт в выделенном потоке, каждая операция `Store` — одна транзакция. При остановке выполняется `PRAGMA wal_checkpoint(TRUNCATE)`.

**Миграция 2** (v1 → v2) удаляет таблицы v1 (`deliveries`, `delivery_messages`, `drafts`, `outbox`, `seen_runs`, `seen_done_issues`) и ключи `meta` `runs_baseline_at`, `done_baseline_at`, `self_user_id`, затем создаёт таблицы v2. Из v1 сохраняется только `meta.telegram_offset`. Откат на v1 после миграции возможен только из резервной копии ([BACKUP_RESTORE](BACKUP_RESTORE.md)).

| Таблица | Назначение | Ключевые поля |
|---|---|---|
| `meta` | служебные значения | `telegram_offset`, `started_at` |
| `companies` | обнаруженные компании | `company_id` PK, `name`, `issue_prefix`, `key_ref`, `last_seen_at` |
| `conversations` | беседа ↔ тема | `issue_id` PK, `company_id`, `identifier`, `agent_id`, `agent_name`, `key_ref`, `discovered_at`, `watermark_comment_id`, `cursor_comment_id`, `last_updated_at`, `hot_until`, `last_swept_at`, `topic_thread_id` UNIQUE (NULL, пока темы нет), `topic_name`, `topic_state` (`pending`, `active`, `recreating`), `removed_at` |
| `seen_comments` | дедупликация курсора | `(issue_id, comment_id)` PK, `created_at`. Раз в час удаляются записи старше 7 дней, при этом последние 500 на беседу сохраняются (`discovery.rs: SEEN_RETENTION`, `SEEN_KEEP`) |
| `tg_outbox` | куски ответов для Telegram | `id`, `issue_id`, `comment_id`, `chunk`, `chunks`, `html`, `plain`, `use_plain`, `status` (`pending`, `sent`, `abandoned`), `attempts`, `next_attempt_at`, `telegram_message_id`; UNIQUE `(comment_id, chunk)` |
| `pc_outbox` | сообщения пользователя для Paperclip | `id`, `chat_id`, `message_id`, `update_id`, `issue_id`, `client_request_id`, `body`, `status` (`pending`, `in_flight`, `done`, `rejected`), `attempts`, `next_attempt_at`, `last_error` (класс ошибки), `comment_id`, `notified`; UNIQUE `(chat_id, message_id)` |

- `key_ref` = `<номер ключа>:<первые 8 hex sha256(ключ)>` (`core/mod.rs: key_ref`). Сам ключ в БД не хранится. По `key_ref` беседа находит свой ключ и после перестановки файлов в `PAPERCLIP_TOKEN_FILES` (`Bridge::key_index`: сначала по хешу, затем по номеру).
- `thread_id` в `tg_outbox` не хранится: он берётся из `conversations` в момент отправки. Поэтому после пересоздания темы очередь уходит в новую тему.
- Тексты удаляются, как только они больше не нужны. В `tg_outbox` поля `html` и `plain` обнуляются при `sent` и `abandoned`. В `pc_outbox` поле `body` обнуляется при `done` и `rejected`. Пока запись в очереди, в файле лежит текст переписки. Кроме того, в файле хранятся названия компаний, имена агентов, идентификаторы бесед и тем. Поэтому `bridge.db` и его резервные копии нужно защищать как конфиденциальные. Токенов и ключей в БД нет.

> **Отличие от ADR-0010 §2.4.** Схема допускает значения `tg_outbox.status = 'sending'` и `topic_state = 'closed'`, а в `companies` есть поле `unavailable_since`, но код их не записывает. Кусок остаётся `pending` до коммита `sent`. Закрытая тема обнаруживается при отправке (`TOPIC_CLOSED`) и сразу переоткрывается. Недоступность компании хранится в памяти до следующего цикла обнаружения. `recover()` при старте возвращает `sending → pending`, но таких записей не бывает. Поля `plain` и `use_plain` добавлены для fallback в простой текст (§4.4).

## 4. Потоки данных

### 4.1. Старт (`app.rs: run`)

1. Переменные v1 проверяются до всего остального (`config.rs: REMOVED_VARS`, `IGNORED_VARS`). Если задана `PAPERCLIP_COMPANY_IDS` или `TELEGRAM_USER_ID`, старт отклоняется с подсказкой о миграции (код выхода 2). Переменные `GOVERNED_*`, `NOTIFY_*`, `PAPERCLIP_RUNS_POLL_INTERVAL`, `PAPERCLIP_GOVERNED_TIMEOUT` дают только WARN. Одновременно заданные `PAPERCLIP_TOKEN_FILE` и `PAPERCLIP_TOKEN_FILES`, а также один файл, указанный дважды, — ошибка конфигурации. `TELEGRAM_CHAT_ID` должен быть отрицательным (id супергруппы).
2. Открытие состояния: `integrity_check`, миграции, `recover()` (`pc_outbox` `in_flight → pending`).
3. Клиенты Paperclip (по одному на ключ) и Telegram.
4. `GET /api/health` первым ключом и проверка версии (`paperclip/version.rs: log_check`, ADR-0009 §3). Совпадение с `TESTED_PAPERCLIP_VERSION` (`2026.1106.0`) даёт INFO. Другая версия, отсутствие версии или непонятный формат дают WARN. Если `/api/health` не ответил, пишется WARN `paperclip health check failed, continuing`. Старт не блокируется ни в одном случае, повторной проверки нет.
5. Запуск задач (§2). Задача `checks` проверяет супергруппу: `getMe`, затем `getChat(TELEGRAM_CHAT_ID)` (`type == "supergroup"`, `is_forum == true`), затем `getChatMember(TELEGRAM_CHAT_ID, <id бота>)` (`status == "administrator"`, `can_manage_topics == true`). Нарушение — ERROR с причиной, `telegram_group_ok=false` (unhealthy) и повтор раз в 5 минут, процесс не падает. Сетевые ошибки дают повтор с backoff. После успешной проверки задача завершается. Если права отберут позже, это обнаружит `sender` по `403` (§4.4).

### 4.2. Обнаружение компаний и бесед (`core/discovery.rs`)

- **Компании.** Раз в `PAPERCLIP_DISCOVERY_INTERVAL` каждый ключ вызывает `GET /api/companies?scope=accessible`. Архивные компании (`status: "archived"`) отбрасываются. Списки ключей объединяются по `id`, и компанию обслуживает первый ключ в порядке конфигурации. Если список одного ключа недоступен, компании, которые этот ключ видел раньше, сохраняются. Затем для каждой компании вызывается `GET /api/companies/{id}/agents`, имена агентов хранятся в памяти. Отметка `last_discovery_ok_at` обновляется, только если все вызовы цикла успешны.
- **Беседы.** В каждом цикле по каждой доступной компании вызывается `GET /api/companies/{id}/chats`. В ответе — беседы владельца ключа, у которых задан `conversationAgentId`.
  - **Новая беседа.** Сначала читается водяной знак `GET …/comments?order=desc&limit=1`. Его `id` становится и `watermark_comment_id`, и `cursor_comment_id`, поэтому история до обнаружения не пересылается. Если водяной знак прочитать не удалось, беседа откладывается до следующего цикла. Затем в `conversations` вставляется строка с `topic_state = pending`, и `sender` получает сигнал создать тему.
  - **Беседы нет в списке:** `removed_at` = сейчас. Тема отвязывается: сообщения в неё получают подсказку (§4.5).
  - **Беседа вернулась в список:** `removed_at` = NULL. Если темы нет, `sender` создаёт её.
- **Ключи разных пользователей.** Первый увиденный `conversationUserId` запоминается как владелец (`discovery.rs: owner_matches`). Беседа с другим владельцем пропускается, и в каждом цикле в лог пишется ERROR `keys belong to different Paperclip users`. Публиковать в чужую беседу ключ всё равно не смог бы: Paperclip ответил бы `403 Only the conversation owner…`.

  > **Отличие от ADR-0010 §1.1.** По ADR старт отклоняется, а в коде он не отклоняется. Владельца можно узнать только из `/chats`, то есть уже после старта.
- **Ошибки `/chats`** (`discovery.rs: chats_failed`):
  - `404 "Agent Chat is disabled"` — одно сообщение в General, `agent_chat_ok=false`, повтор через `PAPERCLIP_DISCOVERY_INTERVAL`;
  - `401` — ключ отклонён (§4.6);
  - другой `403` или `404` — компания пропускается до следующего обнаружения;
  - `429`, `5xx`, сеть — бэкофф этой компании с учётом `Retry-After`; остальные компании опрашиваются как обычно.

### 4.3. Paperclip → Telegram (`core/outbound.rs`, `core/sender.rs`)

1. **Когда читать комментарии.** Комментарии беседы читаются в том же цикле `discovery`, если выполнено хотя бы одно условие:
   - `updatedAt` беседы изменился;
   - беседа «горячая» (`hot_until > now`). Окно `CONVERSATION_HOT_WINDOW` (10 мин) открывается при изменении `updatedAt`, при пересылке ответа агента и при доставке сообщения из Telegram. Окно нужно, потому что квитанции Paperclip не меняют `updatedAt`;
   - с последнего чтения прошло `PAPERCLIP_SWEEP_INTERVAL` (60 с) — страховочный обход.
2. **Чтение.** `GET /api/issues/{id}/comments?order=asc&after=<cursor>&limit=100`, страница за страницей, пока страница полная, но не больше 20 страниц за проход. Paperclip возвращает и сам якорь курсора (ADR-0010 §1.4), поэтому уже виденные `id` отбрасываются по `seen_comments`.
   - **Якорь пропал.** Первая страница пустая, хотя `updatedAt` сдвинулся (якорь удалён). Тогда читается `?order=desc&limit=100`, новыми считаются комментарии новее первого уже виденного, а курсор переставляется на самый новый. Ситуация логируется как WARN (`outbound.rs: fallback`).
3. **Фильтр** (`domain/comment.rs: should_forward`) — единственное место, где решается, что уходит в тему:
   - удалённые комментарии (`deletedAt`) не пересылаются;
   - `authorType: "agent"` пересылается, если `authorAgentId` совпадает с агентом беседы. Квитанции Paperclip (`origin: "fast_response"`) тоже пересылаются: они пишутся от имени агента беседы;
   - `authorType: "system"` пересылается с пометкой ⚠️;
   - `authorType: "user"` (сообщения из UI и из bridge) и остальные не пересылаются. Благодаря этому эхо невозможно: собственные комментарии bridge имеют `authorType: "user"`.

   > **Отличие от ADR-0010 §6.3.** По ADR системные уведомления по умолчанию не пересылаются. Код пересылает их с ⚠️: это решение пользователя в беседе PET-4.
4. **Служебные сообщения с ⚠️** (`outbound.rs: is_notice`, правило — `domain/comment.rs: is_service_notice`, контракт — [paperclip-api.md](paperclip-api.md) §6.1). Сообщение о сбое или отмене запуска Paperclip публикует от имени агента беседы, и по полям комментария его не отличить от ответа. Поэтому для пересылаемого комментария агента, у которого есть `createdByRunId`, который не является квитанцией и состоит из одной строки длиной до 300 символов, читается `GET /api/heartbeat-runs/{runId}`. Пометка ⚠️ ставится, если `status` запуска — `failed`, `cancelled` или `timed_out`, а текст комментария совпадает с первой строкой `error` запуска или с `status` (без учёта регистра). Bridge берёт из ответа только `status` и `error` (`paperclip/dto.rs: run`). `error` используется только для сравнения: в тему не отправляется и в лог не пишется. `stderr` и другие поля запуска не читаются. В тему уходит только текст самого комментария, который Paperclip опубликовал в беседе. Если запуск прочитать не удалось (`403`, `404`, сеть), комментарий уходит как обычный ответ без ⚠️, в лог пишется DEBUG с классом ошибки.
5. **Нарезка** (`render: comment_chunks`). Markdown разбивается на куски до 3500 единиц UTF-16. Блок кода, разрезанный границей куска, закрывается и открывается заново. Каждый кусок превращается в Telegram HTML. Текст из Paperclip экранируется, теги ставит только рендер. Кусков не больше `MAX_MESSAGE_CHUNKS` (3). Если кусков больше, последний заканчивается фразой «… сообщение обрезано, полностью — в Paperclip» и ссылкой на задачу, если задан `PAPERCLIP_UI_URL`. Для каждого куска хранится и простой текст для fallback.
6. **Коммит.** В одной транзакции (`store: commit_comments`): куски в `tg_outbox` (UNIQUE `(comment_id, chunk)` защищает от повтора), все прочитанные `id` в `seen_comments` (включая непересланные), новый `cursor_comment_id` и `last_swept_at`. Если что-то переслано, продлевается `hot_until`.
7. **Создание темы** (`topics: create`). `sender` берёт беседы с `topic_state` `pending` или `recreating` в порядке обнаружения и вызывает `createForumTopic("<issuePrefix> · <agent_name>")`. Темы создаются через ту же очередь и с тем же лимитом, поэтому при первом запуске с 28 беседами создание тем займёт около полутора минут.
8. **Отправка** (`sender: send_chunk`). Порядок шагов: сначала сообщение bridge, затем тема, затем кусок. Куски идут в порядке постановки (`tg_outbox.id`). Внутри беседы следующий кусок не уходит, пока не ушёл предыдущий. `sendMessage` вызывается с `message_thread_id`, `parse_mode=HTML` и выключенным превью ссылок. Ошибки:
   - `message thread not found` / `TOPIC_DELETED` → `topic_state = recreating`, тема создаётся заново, кусок уходит в новую тему;
   - `TOPIC_CLOSED` → `reopenForumTopic`, повтор. Если тема при этом оказалась удалённой, она пересоздаётся;
   - `can't parse entities` → тот же кусок уходит простым текстом без `parse_mode` (`use_plain`);
   - `429` → вся очередь на паузе на `retry_after` с джиттером;
   - `403` или `401` → ERROR, `telegram_send_ok=false` (unhealthy), пауза очереди на 5 минут;
   - прочие ошибки: временные повторяются с backoff, постоянные — до `RETRY_MAX_ATTEMPTS` попыток, затем кусок получает статус `abandoned`.

   Тексты ошибок тем вживую не проверены, классификатор работает по подстрокам ([paperclip-api.md](paperclip-api.md) §8).
9. **Сообщения bridge** (подсказки, «ключ отклонён», «беседа на паузе» и т. п., `render: notices`) хранятся только в памяти, это best effort. Если тема удалена или закрыта, сообщение уходит в General.

Переименование агента тему не переименовывает (ADR-0010 §6.1).

### 4.4. Telegram → Paperclip (`core/poller.rs`, `core/inbound.rs`, `core/outbox.rs`)

1. `getUpdates(offset, TELEGRAM_LONG_POLL_TIMEOUT)` с `allowed_updates: ["message"]`. Правки (`edited_message`), реакции и `callback_query` не запрашиваются.
2. **Allowlist** (`core/allowlist.rs: check`). Сначала чат, затем отправитель:
   - `chat.id != TELEGRAM_CHAT_ID` → `chat_mismatch`;
   - `from.id` не входит в `TELEGRAM_ALLOWED_USER_IDS` или поля `from` нет → `user_mismatch`. Анонимный администратор группы приходит как служебный `GroupAnonymousBot` и тоже отклоняется, если его id не добавлен в список;
   - обновление не `message` → `unsupported`.

   Отклонённое обновление только сдвигает offset, без ответа. Запись в лог появляется не чаще раза в 10 с, в ней счётчик подавленных и короткий хеш `sha256(user_id)` вместо id.
3. **Маршрут** (`inbound.rs: route`), первое совпадение выигрывает:

   | Сообщение | Действие |
   |---|---|
   | служебное о темах (`forum_topic_created`, `…_closed` и т. п.) | игнорируется |
   | в General (нет `is_topic_message`) | подсказка «Пишите в тему нужного агента» в General |
   | в теме без беседы или с беседой `removed_at` | подсказка «Эта тема не связана с беседой Paperclip» |
   | в теме, но не текст (фото, файл, стикер) | подсказка «передаётся только текст» |
   | текст в теме беседы | запись `pc_outbox(pending)` |

   Текст отправляется как есть, `/new` тоже: Paperclip начинает новую сессию агента, как из UI (ADR-0010 §6.2).
4. **Коммит** (`store: enqueue_message` → `commit_update`). Запись `pc_outbox` и `telegram_offset = update_id + 1` сохраняются **в одной транзакции**. `update_id` меньше сохранённого offset считается дублем и пропускается. Если обработка обновления упала, offset не сдвигается, и `poller` запрашивает то же обновление снова.
5. **Публикация** (`outbox.rs: process`). Запись переходит в `in_flight`, затем выполняется `POST /api/issues/{issue_id}/comments` с телом `{ body, clientRequestId }` ключом беседы (`key_ref`). `clientRequestId` = `UUIDv5(NS_BRIDGE, "tg:<chat_id>:<message_id>")`, где `NS_BRIDGE` = UUIDv5 от URL репозитория (`inbound.rs: client_request_id`). Значение закреплено тестом. Заголовок `X-Paperclip-Run-Id` не передаётся. Подтверждение в Telegram не отправляется: квитанция агента придёт сама (ADR-0010 §6.4).

   | Ответ Paperclip | Действие (`outbox.rs: classify`) |
   |---|---|
   | `2xx` | `done`, `body` обнуляется, `hot_until` беседы продлевается |
   | `409` `Task is paused…` | `rejected`, в теме: «Беседа на паузе…» |
   | `409` `…already used for different content` | `rejected`, ERROR. При детерминированном id такого быть не должно |
   | `403` (`Only the conversation owner…`) | `rejected`, ERROR, в теме: «ключ бриджа принадлежит не владельцу беседы» |
   | `404` | `rejected`, в теме: «Беседа не найдена в Paperclip или Agent Chat выключен» |
   | `400`, `422`, прочие `4xx` | `rejected`, в теме текст ошибки сервера (до 300 символов, экранирован) |
   | `401` | ключ отклонён (§4.6), запись остаётся `pending`, повтор раз в 5 минут |
   | `429`, `5xx`, таймаут, сеть, нечитаемый ответ | повтор бессрочно с backoff, для `429` учитывается `Retry-After`. После `RETRY_NOTIFY_AFTER` попыток в теме появляется одно сообщение «сообщение в очереди» |

   Отклонённая запись не блокирует следующие. `RETRY_MAX_ATTEMPTS` к `pc_outbox` не применяется.
6. Комментарий bridge будит агента так же, как сообщение из UI, и так же снимает карточки с `supersedeOnUserComment` (ADR-0010 §1.3, §1.8; [PAPERCLIP_PERMISSIONS](PAPERCLIP_PERMISSIONS.md)).

### 4.5. Пропавшие и удалённые темы

| Что случилось | Поведение |
|---|---|
| Тему удалили в Telegram | при следующей отправке в неё — `recreating`, новая тема, очередь уходит туда (`topics: lost`) |
| Тему закрыли | при следующей отправке — `reopenForumTopic` |
| Тему переименовали | связка хранится по `topic_thread_id`, имя не важно, маршрутизация не меняется |
| Беседа пропала из `/chats` | `removed_at`; сообщения из темы не передаются, бот отвечает подсказкой. Если беседа вернётся, связь восстанавливается |
| Вручную создали тему | сообщения в ней получают подсказку «не связана с беседой» |

### 4.6. Ключ отклонён (`core/mod.rs: key_rejected`)

`401` от любого вызова Paperclip переводит ключ в состояние «отклонён»: `paperclip_auth_ok=false` (unhealthy), ERROR и одно сообщение в General на ключ. Состояние снимается первым успешным вызовом этим ключом. Повтор `/chats` и `POST` идёт раз в 5 минут (`AUTH_RETRY_INTERVAL`).

## 5. Падение посреди отправки (SIGKILL, потеря питания)

Вопрос: может ли kill -9 потерять сообщение или отправить его дважды. Короткий ответ: **сообщения в Paperclip не теряются и не дублируются; ответ агента в Telegram может прийти дважды**.

**Telegram → Paperclip.** Reconcile из v1 не нужен: «ровно один раз» обеспечивает идемпотентность сервера по `clientRequestId`.

| Когда упали | Что после рестарта |
|---|---|
| До коммита обновления | offset не сдвинут, Telegram присылает обновление снова, оно обрабатывается как новое |
| После коммита, до `POST` | запись `pending` (или `in_flight`, её `recover()` вернёт в `pending`), `pc_outbox` отправит |
| `POST` дошёл, коммита `done` нет (`fault::AFTER_HTTP_BEFORE_OUTBOX_COMMIT`) | `recover()` → `pending`. Повторный `POST` с тем же `clientRequestId` и телом возвращает существующий комментарий без второго wake агента (ADR-0010 §1.3). Тест: `tests/crash.rs::crash_after_post_gives_exactly_one_comment` |
| Повтор того же `update_id` | отсекается по offset и по UNIQUE `(chat_id, message_id)` |

**Paperclip → Telegram.**

| Когда упали | Что после рестарта |
|---|---|
| После чтения комментариев, до коммита курсора (`fault::AFTER_READ_BEFORE_CURSOR_COMMIT`) | курсор и `tg_outbox` не изменились, комментарии читаются снова. Ни потерь, ни дублей. Тест: `tests/crash.rs::crash_before_cursor_commit_loses_and_duplicates_nothing` |
| После `sendMessage`, до коммита `sent` (`fault::AFTER_SEND_BEFORE_COMMIT`) | кусок остаётся `pending` и отправляется ещё раз. **Дубль куска в Telegram — принятый компромисс** (ADR-0004): у `sendMessage` нет идемпотентности. Тест: `tests/crash.rs::crash_after_send_repeats_the_piece_without_loss` |

Потеря питания: `synchronous=FULL` + WAL, подтверждённая транзакция (offset, курсор, очереди) не откатывается. **Не проверено**: поведение реальной файловой системы хоста при потере питания и реальный SIGKILL контейнера. Проверены только инъекции отказа в тестах.

## 6. Остановка

SIGTERM/SIGINT отменяют общий `CancellationToken` (`shutdown.rs`). Дальше `app.rs: run`:

1. Ожидание `checks`, `poller`, `discovery`, `health_writer` до `SHUTDOWN_GRACE`. По истечении — WARN `tasks did not stop within the grace period`. Long poll прерывается без сдвига offset.
2. Ожидание `pc_outbox` до `SHUTDOWN_GRACE`: новые записи не берутся, запись в работе дописывается. По истечении — WARN `pc_outbox did not finish within the grace period`. Запись остаётся `in_flight` и при следующем старте вернётся в `pending`.
3. Остановка `sender`: его токен отменяется последним, чтобы ушли последние сообщения других задач. Затем `wal_checkpoint(TRUNCATE)` и финальная запись heartbeat.

Худший случай до шага 3 — `2 × SHUTDOWN_GRACE`, поэтому `stop_grace_period` в compose должен быть строго больше (README, «Остановка»).

## 7. Healthcheck

[ADR-0007](adr/0007-healthcheck-heartbeat.md), `health.rs`. Раз в 15 с `health_writer` атомарно пишет `HEALTH_FILE`: `pid`, `started_at`, `written_at`, `last_telegram_ok_at`, `last_paperclip_ok_at`, `last_discovery_ok_at`, `paperclip_auth_ok`, `telegram_group_ok`, `telegram_send_ok`, `agent_chat_ok`, `queue_depth` (куски `tg_outbox` в `pending`). `bridge healthcheck` сетевых запросов не делает и возвращает unhealthy, если:

- файл старше 60 с или не читается;
- `paperclip_auth_ok=false` (`401`, §4.6);
- `agent_chat_ok=false` (`404 Agent Chat is disabled`);
- `telegram_group_ok=false` (проверка супергруппы, §4.1);
- `telegram_send_ok=false` (`403`/`401` при отправке, §4.3);
- нет успешного вызова Telegram или Paperclip дольше `HEALTH_MAX_STALE` (до первого успеха отсчёт идёт от старта).

`queue_depth` и `last_discovery_ok_at` только наблюдаются и на результат не влияют. При `queue_depth > 60` в лог пишется WARN.

## 8. Логи

Логи пишутся в stderr (`LOG_FORMAT=json|text`, `LOG_LEVEL`). В записи попадают идентификаторы (`issue_id`, `comment_id`, `update_id`, `thread_id`, `pc_outbox_id`), классы ошибок и счётчики. Тексты сообщений пользователя и ответов агентов, тексты ошибок Paperclip, поле `error` запусков, тела запросов и токены не логируются. Токены хранятся как `SecretString`, ошибки reqwest сводятся к классу без URL (в URL Bot API есть токен). Id чужих пользователей Telegram заменяются хешем `sha256(id)[..4]` в hex. URL proxy печатается только схемой.

Основные записи для аудита:

- `conversation discovered` (`issue_id`, `identifier`, `topic`), `forum topic created`;
- `agent replies queued for telegram`, `reply delivered to telegram`;
- `message queued for paperclip` (`update_id`, `issue_id`, `thread_id`), `message delivered to paperclip`, `message rejected by paperclip` (`class`);
- `ignored update from outside the allowlist` (`reason`, `sender_hash`).

Telegram id автора сообщения из разрешённого списка в лог не пишется. Кто из разрешённых пользователей написал сообщение, видно только в истории темы в Telegram.
