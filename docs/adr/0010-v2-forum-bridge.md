# ADR-0010. v2: живая беседа с агентами через супергруппу с темами

- Статус: принято (реализовано в v2.0.0)
- Контекст: план v2 (беседа PET-4, план ревизии 10, согласован пользователем), этап 1 — PET-20. Предыдущие решения: [ADR-0002](0002-board-api-key.md), [ADR-0003](0003-sqlite-state.md), [ADR-0004](0004-outbox-exactly-once.md), [ADR-0005](0005-thin-telegram-client.md), [ADR-0006](0006-html-and-callback-data.md), [ADR-0007](0007-healthcheck-heartbeat.md)
- Заменяет: [ADR-0001](0001-attention-feed-as-source.md), [ADR-0008](0008-governed-from-telegram.md); частично — [ADR-0004](0004-outbox-exactly-once.md) (reconcile), [ADR-0006](0006-html-and-callback-data.md) (`callback_data`), [ADR-0009](0009-v1.1.0-improvements.md) §1–2 (карточки, draft). Дополняет [ADR-0002](0002-board-api-key.md)
- Отличия реализации от этого ADR перечислены в [ARCHITECTURE.md](../ARCHITECTURE.md) (пометки «Отличие от ADR-0010»)

## 0. Как проверялось

Пути в коде даны относительно корня исходников Paperclip (`/app` в контейнере Paperclip). Уровень доказательности у каждого ответа отмечен так:

- **[живой API]** — проверено запросом к работающему Paperclip (агентским ключом, только чтение);
- **[код]** — по исходникам, вживую не проверено: board-ключа у архитектора нет, а маршруты для board отвечают агентскому ключу `403` (проверено: `GET /api/companies?scope=accessible` и `GET /api/companies/{id}/chats` → `403`);
- **[док]** — документация Telegram Bot API (версия 10.4 от 9 ноября 2026) или Bot FAQ.

**Версия.** Живой сервер: `GET /api/health` → `version: "2026.1106.0"`, `commit: d69631b01b9c658f80bf8bed389863dce82643e1` **[живой API]**. Процесс сервера — `node server/dist-runtime/index.mjs` в том же контейнере, где лежат исходники `/app`, то есть прочитанный код и есть код живой версии. Эта версия совпадает с `TESTED_PAPERCLIP_VERSION` v1 (`src/paperclip/version.rs`). Без авторизации `/api/health` не отдаёт `version` (только `status` и `commit`), с любым ключом отдаёт (`server/src/routes/health.ts:255-330`) **[живой API]**.

## 1. Ответы на вопросы этапа 1

### 1.1. Один board-ключ на все компании? Как получить список компаний

**Да, один ключ действует во всех компаниях, где у пользователя активное членство.** Уверенность высокая, **[код]**, вживую не проверено.

- Ключ привязан только к пользователю: в таблице `board_api_keys` есть `user_id` и нет `company_id` (`packages/db/src/schema/board_api_keys.ts:4-20`).
- По ключу middleware строит actor `{ type: "board", userId, companyIds: access.companyIds, source: "board_key", keyId }` (`server/src/middleware/auth.ts:339-355`). `companyIds` — все активные членства пользователя (`server/src/services/board-auth.ts:72-102`: `company_memberships` с `principal_type='user'`, `status='active'`), то есть тот же набор, что у UI-сессии (`auth.ts:302-322`).
- **Срок жизни.** Без явного `expiresAt` ключ живёт 30 дней (`board-auth.ts:17`, `BOARD_API_KEY_TTL_MS`). Для бриджа ключ создаётся бессрочным (`expiresAt: null`), как в ADR-0002.

**Список компаний:** `GET /api/companies?scope=accessible` (`server/src/routes/companies.ts:380-399`) **[код]**. Параметр `scope=accessible` **обязателен**: без него instance admin получает *все* компании инстанса, включая те, где у него нет членства, а `GET /companies/{id}/chats` по ним ответит `403`. В ответе нужны поля `id`, `name`, `issuePrefix`, `status` (формат полей проверен на `GET /api/companies/{id}` **[живой API]**: `{"name":"PET","issuePrefix":"PET","status":"active"}`).

**Конфиг — оба варианта** (по требованию PET Lead):

- **Вариант A (основной):** один ключ, `PAPERCLIP_TOKEN_FILE`.
- **Вариант B (запасной):** список ключей, `PAPERCLIP_TOKEN_FILES=/run/secrets/pc_a,/run/secrets/pc_b`. Бридж опрашивает `GET /api/companies?scope=accessible` каждым ключом и объединяет компании по `id`. Если компанию видят несколько ключей, используется первый по порядку в списке. Для каждой беседы в store хранится `key_ref` (порядковый номер ключа и первые 8 символов `sha256` ключа), и комментарии в беседу публикуются этим же ключом. Все ключи должны принадлежать одному пользователю Paperclip: публиковать в беседу может только её владелец (см. 1.3). Если `conversationUserId` у бесед разных ключей отличается, это ошибка конфигурации, и старт отклоняется.
- Задавать обе переменные одновременно нельзя: старт отклоняется с понятной ошибкой.

### 1.2. Как найти беседы пользователя

**Отдельный маршрут: `GET /api/companies/{companyId}/chats`** (`server/src/routes/issues.ts:18219-18226`) **[код]**.

- Возвращает задачи, у которых `conversationUserId = <пользователь ключа>` и `conversationAgentId IS NOT NULL`, отсортированные по `updatedAt desc` (`server/src/services/issues.ts:9896-9898`).
- Требования: actor `board` с `userId` (board-ключ подходит) и включённый experimental-флаг `enableAgentChat`, иначе `404 "Agent Chat is disabled"`. Флаг сейчас включён: беседа PET-4 существует, и пользователь в ней пишет **[живой API]**.
- Общий список `GET /api/companies/{id}/issues` **не подходит**: без `q` он исключает беседы (`services/issues.ts:8029-8030`, `isNull(issues.conversationAgentId)`), а фильтров `conversationAgentId`/`conversationUserId` у него нет **[код]**.
- **Одна беседа на пару «агент + пользователь» в компании:** уникальный индекс `(companyId, conversationAgentId, conversationUserId)` и check `assigneeAgentId = conversationAgentId` (`packages/db/src/schema/issues.ts:102-109`) **[код]**. Случай «несколько бесед с одним агентом» из плана невозможен, поэтому название темы — просто «`<issuePrefix>` · `<имя агента>`».
- Признак беседы на задаче: `conversationAgentId`, `conversationUserId`, `conversationState` (`active`/`waiting`), `conversationSessionGeneration` (растёт при `/new`). На PET-4: `conversationAgentId=0755796e…`, `conversationUserId=xroXurj6…`, `conversationState=waiting`, `conversationSessionGeneration=0`, заголовок `Chat with PET Lead` **[живой API]**.
- Имя агента: `GET /api/companies/{id}/agents` (один раз за цикл обнаружения) или `GET /api/agents/{id}`. Поле `name` проверено на `GET /api/agents/{id}` **[живой API]**.

### 1.3. Будит ли агента комментарий, опубликованный board-ключом через API

**Да, так же, как комментарий из UI.** Вывод однозначный, **[код]**, вживую не проверено. В коде нет ни одного места на этом пути, которое отличало бы UI-сессию от board-ключа.

1. **UI ходит в тот же маршрут.** Страница беседы (`ui/src/pages/AgentChat.tsx` → `TaskDetailSurface` из `IssueDetail.tsx`) отправляет сообщение через `issuesApi.addComment` → `POST /api/issues/{id}/comments` с `clientRequestId = crypto.randomUUID()` (`ui/src/api/issues.ts:552-568`, `ui/src/pages/IssueDetail.tsx:4809-4813`).
2. **Для бесед в маршруте есть отдельная ветка с условием `req.actor.type === "board"`** (`server/src/routes/issues.ts:18265-18298`). И UI-сессия, и board-ключ — это `type: "board"` (`middleware/auth.ts:312-322` и `:344-355`). Ветка проверяет:
   - `enableAgentChat` включён, иначе `404`;
   - `req.actor.userId === issue.conversationUserId`, иначе `403 "Only the conversation owner can send messages…"`;
   - есть `clientRequestId`, иначе `422 "Chat messages require a clientRequestId for safe retries"`;
   - беседа не на паузе (`assertBoardCommentNotPaused`, `issues.ts:6827-6842`), иначе `409 "Task is paused…"`; исключение — текст `/new`.

   Поле `req.actor.source` (`session` или `board_key`) **нигде в ветке не читается**.
3. **Комментарий сохраняется одинаково:** `svc.addComment(issue.id, body, { userId }, { clientRequestId, authorType: "user" })`. В строку комментария попадает только `userId`, ни `keyId`, ни `source` не записываются. В журнал действий пишется `actorType: "user"` из `getActorInfo`, и для обоих источников это `"user"` (`server/src/routes/authz.ts:229-245`).
4. **Пробуждение одинаковое:** сразу после транзакции вызывается `deliverConversationComments(db, issue, heartbeat.wakeup)` (`server/src/services/agent-conversations.ts:523-575`). Она берёт все комментарии с `clientRequestId IS NOT NULL`, для которых ещё нет wakeup с ключом `conversation-comment:<commentId>` (`agent-conversations.ts:473-493`), и ставит wake агенту беседы: `reason: "issue_commented"`, `requestedByActorType: "user"`, `requestedByActorId: comment.authorUserId`. Источник actor здесь не участвует.
5. Квитанция (fast response) тоже ставится одинаково: `enqueueFastResponse(...)` в той же транзакции.
6. `sourceTrust` для пользователя всегда `null`, независимо от источника (`server/src/services/source-trust.ts:102`).

Живые данные согласуются с этим: все 21 сообщение пользователя в PET-4 (из UI) имеют `authorType: "user"`, `authorUserId`, `clientRequestId`, `createdByRunId: null`, `origin: "comment"` **[живой API]**. Комментарий бриджа будет иметь ровно тот же вид.

**Последствия для реализации:**

- `clientRequestId` обязателен и должен быть UUID (`packages/shared/src/validators/issue.ts:1114-1125`). Сервер идемпотентен по `(issueId, authorUserId, clientRequestId)` (уникальный индекс `issue_comments_client_request_uq`, `packages/db/src/schema/issue_comments.ts:50`). Повтор с тем же телом возвращает существующий комментарий, повтор с другим телом даёт `409 "Message request ID was already used for different content"` (`server/src/services/issues.ts:12577-12583`). Бридж формирует `clientRequestId` детерминированно, как UUIDv5 от `tg:<chat_id>:<message_id>`. Повтор после краша не создаёт второй комментарий и не будит агента дважды (идемпотентность wake — по `conversation-comment:<commentId>`).
- Заголовок `X-Paperclip-Run-Id` бридж **не передаёт**.
- Текст `/new` из Telegram — штатная команда Paperclip «новая сессия» (`resumeConversationForReset`, `agent-conversations.ts:495-520`). См. открытый вопрос 6.2.

**Опциональный живой smoke-тест** (не блокирует разработку; выполнит пользователь или QA после развёртывания, ключ подставляется локально и в комментарии не публикуется):

```sh
# 1. Компании, видимые ключом. Ожидается JSON-массив со всеми компаниями пользователя.
curl -s -H "Authorization: Bearer $PAPERCLIP_BOARD_KEY" \
  "http://<paperclip-host>:3100/api/companies?scope=accessible"

# 2. Сообщение в беседу (id — из GET /api/companies/<companyId>/chats).
#    Ожидается 201, а в UI беседы — сообщение от вас и ответ агента.
curl -s -X POST -H "Authorization: Bearer $PAPERCLIP_BOARD_KEY" -H "Content-Type: application/json" \
  -d "{\"body\":\"Проверка бриджа: ответь одним словом «ок».\",\"clientRequestId\":\"$(cat /proc/sys/kernel/random/uuid)\"}" \
  "http://<paperclip-host>:3100/api/issues/<conversationIssueId>/comments"
```

### 1.4. Новые комментарии: курсор, нагрузка, интервалы

**Курсор есть:** `GET /api/issues/{id}/comments?order=asc&after=<commentId>&limit=<N>` (`server/src/routes/issues.ts:16212-16250`, `server/src/services/issues.ts:12320-12372`) **[код + живой API]**.

- Порядок по умолчанию — `desc`; `asc` нужно передавать явно. Без `limit` возвращаются **все** комментарии. Максимум 500.
- **Курсор захватывает сам якорь.** По коду условие строгое (`createdAt > anchor OR (= AND id > anchor.id)`), но вживую `after=d45ce5f8…&order=asc` вернул первым сам `d45ce5f8…` **[живой API]**. Причина: в Postgres `createdAt` хранится с микросекундами, а якорь читается в JS `Date` с точностью до миллисекунд, поэтому сравнение `>` с усечённым значением истинно и для самого якоря. Комментарии не теряются, но возможны повторы. **Бридж обязан отбрасывать уже виденные `id`.**
- Если якоря нет (неизвестный `id` или не UUID), ответ — `[]` без ошибки (`services/issues.ts:12321-12337`). Защита от «вечно пустого» курсора описана в 2.3.
- Ленты комментариев сразу по многим задачам нет. `GET /api/companies/{id}/activity` не имеет курсора и фильтра по времени (`server/src/routes/activity.ts:229-245`). WebSocket `/api/companies/{id}/events/ws` board-ключ не принимает: bearer ищется только среди агентских ключей (`server/src/realtime/live-events-ws.ts:217-226`) **[код]**.
- **Дешёвый сигнал изменений — `updatedAt` беседы** из `GET /companies/{id}/chats`: `addComment` поднимает `issues.updatedAt` (`services/issues.ts:13032-13035`). **Исключение:** квитанции пишутся через `insertIssueComment` и `updatedAt` не трогают (`server/src/services/fast-responses.ts:965-976`). Но квитанция всегда идёт сразу после сообщения пользователя, а оно `updatedAt` поднимает, отсюда «горячее окно» в 2.3.
- **Rate limit.** Глобального ограничителя запросов в Paperclip нет. Для списка задач есть только лимит одновременных запросов (8 на actor+клиент); на `/comments`, `/chats` и `/companies` ограничений нет **[код]**. Paperclip и бридж работают в одной приватной сети.

Интервалы и бэкофф — в 2.3.

### 1.5. `externalChannelBinding` / `externalConversationState`

**Штатная привязка внешнего канала есть, но для бриджа она непригодна, и хранить в этих полях id темы нельзя.** **[код]**, значения на PET-4 проверены **[живой API]**: оба `null`.

- Это не колонки задачи, а **вычисляемые при чтении проекции** встроенных чат-каналов Paperclip:
  - `externalConversationState` — подзапрос `externalConversationStateSql()`, только для провайдеров `slack`/`speko` (`server/src/services/slack-conversation-state.ts:4-37`);
  - `externalChannelBinding` — сводка из таблиц `chat_conversations`/`chat_endpoints`/`chat_publications` (`server/src/services/chat-channel-binding.ts:16-76`).

  В схеме `issues` таких колонок нет.
- В `PATCH /api/issues/{id}` этих полей нет. Схема не строгая, поэтому неизвестные ключи молча отбрасываются (`packages/shared/src/validators/issue.ts:740`, `:928-955`), и записать их нельзя никаким actor.
- **Встроенный Telegram-канал Paperclip есть, но работает только через webhook.** Адаптер жёстко настроен на `mode: "webhook"` (`server/src/services/chat-sdk-runtime.ts:1487`). Настройка требует публичного HTTPS-адреса на порту 443/80/88/8443 (`server/src/services/chat-channels/telegram/setup.ts:112-120`). Paperclip у нас за NAT без входящих соединений, поэтому встроенный канал **не подходит**, и внешний бридж с long polling остаётся единственным вариантом. Кроме того, встроенный канал выключен experimental-флагом `enableChatConnectors` и создаёт собственные задачи (`originKind: "chat_channel"`), а не зеркалит существующие беседы Agent Chat.
- **Вывод:** связку «тема ↔ беседа» бридж хранит в своём SQLite (2.4).

### 1.6. Как отличить комментарии агента, квитанции, пользователя и системные

Поля комментария: `packages/db/src/schema/issue_comments.ts:15-50` **[код]**, формат проверен на PET-4 и PET-20 **[живой API]**.

| Вид | Признаки | Пересылать в Telegram |
|---|---|---|
| Ответ агента | `authorType: "agent"`, `authorAgentId` задан, `origin: "comment"`, обычно `createdByRunId` задан | **да** |
| Квитанция Paperclip | `origin: "fast_response"`, `fastResponseRequestId` задан; в беседе `authorType: "agent"`, `authorAgentId` = агент беседы (`server/src/services/fast-responses.ts:965-976`) | **да** (решение пользователя) |
| Сообщение пользователя (UI или бридж) | `authorType: "user"`, `authorUserId` задан, `clientRequestId` задан | нет |
| Системное сообщение | `authorType: "system"`, авторов нет; часто `presentation.kind: "system_notice"` (например, уведомления recovery) | нет (см. открытый вопрос 6.3); **в реализации — да, с ⚠️** (решение пользователя в PET-4, см. примечание к 6.3) |

**Правило фильтра:** `deletedAt == null && authorType == "agent" && authorAgentId == conversationAgentId`. Квитанции этим правилом уже покрыты. Квитанцию без агента (`authorUserId: "board-concierge"`, `authorType: "system"`) в беседе получить нельзя: у беседы агент всегда есть.

- Вживую в PET-4 54 комментария: 33 `agent/comment` и 21 `user/comment`, квитанций и системных нет **[живой API]**. Формат квитанции проверен только по коду.
- **Защита от эха** получается автоматически: комментарии, отправленные бриджем, имеют `authorType: "user"` и под фильтр не попадают. Хранить их id для защиты от эха не нужно. Они хранятся только ради идемпотентности входящей очереди.
- Поле `derivedAuthorAgentId` встречается у агентов локального CLI, которые пишут под пользовательской авторизацией (`server/src/routes/issues.ts:2194`). Для бесед Agent Chat это неактуально: в PET-4 у всех ответов агента `authorType: "agent"`.

### 1.7. Ограничения Telegram forum topics

**[док]**:

- `createForumTopic(chat_id, name 1–128 символов, icon_color?, icon_custom_emoji_id?)` возвращает `ForumTopic { message_thread_id, name, icon_color, … }`. Бот должен быть администратором супергруппы с правом **`can_manage_topics`**, а у супергруппы должны быть включены темы (`Chat.is_forum = true`).
- `closeForumTopic`/`reopenForumTopic` требуют `can_manage_topics`, кроме случая, когда тему создал сам бот. `deleteForumTopic` требует `can_delete_messages`. Бриджу удаление не нужно.
- В сообщениях из темы есть `message_thread_id` и `is_topic_message = true`. Сообщения в General приходят без `is_topic_message`. Служебные сообщения о темах (`forum_topic_created`, `forum_topic_closed`, `forum_topic_reopened` и т. п.) бридж игнорирует.
- `sendMessage(message_thread_id, text 1–4096 символов после разбора entities, parse_mode)`.
- **Лимиты (Bot FAQ):** не больше 1 сообщения в секунду в один чат и **не больше 20 сообщений в минуту в группу**. Лимит общий для всей супергруппы, то есть на **все темы вместе**. При превышении — `429` с `parameters.retry_after`.
- Обновления `getUpdates` хранятся на сервере Telegram не больше 24 часов. Update подтверждается вызовом с `offset` больше его `update_id`.
- **Удалённая или закрытая тема.** Документация не описывает, что вернёт `sendMessage` в удалённую или закрытую тему. По практике это `400 Bad Request: message thread not found` и `400 Bad Request: TOPIC_CLOSED`. **Не проверено**: Developer проверяет на тестовой супергруппе и фиксирует фактические тексты ошибок в `docs/paperclip-api.md`/тестах. Классификатор в бридже распознаёт обе ошибки по подстроке без учёта регистра.

### 1.8. Эффект `supersedeOnUserComment`

**[код]**, `server/src/services/issue-thread-interactions.ts:841-927`, `:4340-4431`.

- Флаг по умолчанию выключен (`:856-880`). Действует на карточки `request_confirmation` (кроме governed с `toolAction`), `request_checkbox_confirmation`, `request_item_verdicts`, `ask_user_questions`.
- Комментарий снимает карточку, только если `authorUserId` задан, `origin != "fast_response"` и `createdByRunId == null`. Комментарий бриджа board-ключом под эти условия **попадает**: карточка с флагом, созданная до комментария и ещё `pending`, получает статус `expired` (`result.outcome: "superseded_by_comment"`).
- **Исключение для бесед:** `ask_user_questions` в беседе Agent Chat (`conversationAgentId` и `conversationUserId` заданы) комментарием не снимается (`:4375`). Остальные виды карточек с флагом снимаются.
- Для v2 это ожидаемое поведение: пользователь «ответил словами», как и при сообщении из UI. Поведение комментария бриджа в точности совпадает с комментарием из UI. Карточки в Telegram v2 не показываются, поэтому пользователь узнаёт о карточке из ответа агента.

## 2. Архитектура v2

### 2.1. Схема

```
                         приватная сеть (без proxy)                       только исходящие, через proxy
 ┌──────────────┐   GET /companies, /chats, /comments   ┌────────────────────────────────────────┐  getUpdates / sendMessage /   ┌──────────────┐
 │  Paperclip   │ ◄──────────────────────────────────── │                bridge                  │  createForumTopic             │ Telegram     │
 │  /api/...    │   POST /issues/{id}/comments          │ discovery  outbound  sender  inbound   │ ────────────────────────────► │ Bot API      │
 └──────────────┘                                       │ topics     health    SQLite (/state)   │                               └──────────────┘
                                                        └────────────────────────────────────────┘
                                                                               ▲ сообщения пользователя
                                                    приватная супергруппа с темами: «PET · PET Lead», «NN · Lead», …
```

Сетевые решения v1 сохраняются: нет входящих портов, Paperclip через `.no_proxy()`, Telegram только через `TELEGRAM_PROXY_URL`, тонкий клиент Telegram (ADR-0005), HTML (ADR-0006), healthcheck (ADR-0007).

### 2.2. Модули и задачи

| Задача / модуль | Файл (предложение) | Что делает |
|---|---|---|
| `discovery` | `core/discovery.rs` | раз в `PAPERCLIP_DISCOVERY_INTERVAL` вызывает `GET /api/companies?scope=accessible` для каждого ключа и `GET /api/companies/{id}/agents` для имён; раз в `PAPERCLIP_POLL_INTERVAL` вызывает `GET /api/companies/{id}/chats`. Новая беседа: запись в `conversations` с водяным знаком (2.4) и задание на создание темы. Пропавшая беседа: `removed_at` |
| `outbound` | `core/outbound.rs` | опрашивает комментарии бесед (2.3), фильтрует (1.6), рендерит Markdown → HTML, режет на куски и **в одной транзакции** пишет куски в `tg_outbox` и сдвигает курсор беседы |
| `topics` | `core/topics.rs` | создаёт тему (`createForumTopic`), переоткрывает закрытую (`reopenForumTopic`), пересоздаёт удалённую. Работает через общую очередь `sender` |
| `sender` | `core/sender.rs` (переработка v1) | единая очередь всех вызовов Telegram, меняющих чат: `sendMessage`, `createForumTopic`, `reopenForumTopic`. Лимит **≤ 20 в минуту на супергруппу** (интервал 3 с, `TELEGRAM_GROUP_MIN_INTERVAL`), учитывает `retry_after`, ретраи с backoff |
| `inbound` (poller) | `core/poller.rs` + `core/inbound.rs` | long polling `getUpdates` (`allowed_updates: ["message"]`), фильтр (2.5); подходящее сообщение пишет в `pc_outbox` **в одной транзакции** со сдвигом offset |
| `pc_outbox` | `core/outbox.rs` (переработка v1) | публикует сообщения пользователя: `POST /api/issues/{id}/comments` с детерминированным `clientRequestId` |
| `render` | `render/text.rs` (из v1) | Markdown → Telegram HTML, нарезка по 4096 с корректным закрытием и повторным открытием тегов, `MAX_MESSAGE_CHUNKS` |
| `store` | `store/sqlite.rs` | SQLite (ADR-0003), миграция 2 (2.4) |
| `health_writer` | `app.rs` | как в v1, плюс поля `queue_depth` и `last_discovery_ok_at` |

**Удаляется из v1:** `core/sync.rs` (лента `attention`), `core/cards.rs`, `core/forms.rs`, `core/dialog.rs` (карточная часть), `core/governed.rs`, `core/runs.rs`, `core/commands.rs` (`/status` и прочие команды), `render/cards.rs`, `render/forms.rs`, `render/callback.rs`, `render/governed.rs`, `domain/interaction.rs`, `domain/draft.rs`, `domain/run.rs`, а также `setMyCommands` и обработка `callback_query`. Остаются и переиспользуются `retry.rs`, `health.rs`, `logging.rs`, `shutdown.rs`, `fault.rs`, `paperclip/version.rs`, `telegram/http.rs`, `render/text.rs`, `allowlist.rs` (переделать под группу).

### 2.3. Опрос Paperclip: интервалы и бэкофф

Беседы делятся на «горячие» и «холодные»:

1. **Каждые `PAPERCLIP_POLL_INTERVAL` (5 с):** на каждую компанию один `GET /companies/{id}/chats`. Комментарии беседы читаются, если выполнено хотя бы одно условие:
   - `updatedAt` беседы изменился с прошлого раза;
   - беседа «горячая»: `hot_until > now`. Окно `CONVERSATION_HOT_WINDOW` (10 мин) открывается при изменении `updatedAt`, при отправке комментария из Telegram и при получении комментария агента. Окно нужно, потому что квитанции не трогают `updatedAt`;
   - с последнего чтения прошло `PAPERCLIP_SWEEP_INTERVAL` (60 с) — страховочный полный обход.
2. **Чтение комментариев:** `GET /issues/{id}/comments?order=asc&after=<cursor>&limit=100` с повтором, пока страница полная. Уже виденные `id` отбрасываются (якорь возвращается повторно, см. 1.4).
   - **Пустой ответ при сдвинутом `updatedAt`** (якорь пропал): `GET …?order=desc&limit=100`, отбор не виденных `id` новее водяного знака, курсор переставляется на самый новый. Ситуация логируется как WARN.
3. **Каждые `PAPERCLIP_DISCOVERY_INTERVAL` (60 с):** список компаний и имена агентов.

**Нагрузка.** Возьмём 4 компании и по 7 бесед в каждой, то есть 28 бесед. В покое это 4 запроса за 5 с плюс 28 за 60 с, около 1,3 запроса в секунду. В активной беседе добавляется 1 запрос в 5 с. Для Paperclip в той же сети это пренебрежимо мало, ограничителя у него нет (1.4).

**Бэкофф** (переиспользуется `retry.rs`, full jitter `RETRY_BASE_DELAY`…`RETRY_MAX_DELAY`) **отдельно по каждой компании**: сбой одной компании не тормозит остальные. `429` учитывает `Retry-After`. `401` → состояние «ключ отклонён»: сообщение в General, healthcheck unhealthy, повтор раз в 5 минут, как в v1. `403`/`404` на `/chats` для одной компании → компания помечается недоступной до следующего цикла обнаружения. `404 "Agent Chat is disabled"` → одно сообщение в General, unhealthy.

### 2.4. Хранилище (SQLite, миграция 2)

SQLite и все его режимы — как в ADR-0003: транзакции нужны для атомарности «курсор + очередь» и «offset + очередь», а JSON-файл этого не даёт. Миграция 2 **удаляет таблицы v1** (`deliveries`, `delivery_messages`, `drafts`, `outbox`, `seen_runs`, `seen_done_issues`) и создаёт новые. Состояние карточек v1 в v2 не нужно. `meta.telegram_offset` сохраняется.

| Таблица | Назначение | Ключевые поля |
|---|---|---|
| `meta` | служебное | `telegram_offset`, `schema`, `started_at` |
| `companies` | обнаруженные компании | `company_id` PK, `name`, `issue_prefix`, `key_ref`, `last_seen_at`, `unavailable_since` |
| `conversations` | беседа ↔ тема | `issue_id` PK, `company_id`, `agent_id`, `agent_name`, `discovered_at`, `watermark_comment_id`, `cursor_comment_id`, `last_updated_at`, `hot_until`, `last_swept_at`, `topic_thread_id` (NULL, пока тема не создана) UNIQUE, `topic_name`, `topic_state` (`pending`, `active`, `closed`, `recreating`), `removed_at` |
| `seen_comments` | дедупликация курсора | `(issue_id, comment_id)` PK, `created_at`; хранится 7 дней или последние 500 на беседу |
| `tg_outbox` | куски в Telegram | `id`, `issue_id`, `comment_id`, `chunk`, `chunks`, `html`, `status` (`pending`, `sending`, `sent`, `abandoned`), `attempts`, `next_attempt_at`, `telegram_message_id`; UNIQUE `(comment_id, chunk)` |
| `pc_outbox` | сообщения пользователя в Paperclip | `id`, `chat_id`, `message_id`, `update_id`, `issue_id`, `client_request_id`, `body`, `status` (`pending`, `in_flight`, `done`, `rejected`), `attempts`, `next_attempt_at`, `last_error`, `comment_id`; UNIQUE `(chat_id, message_id)` |

- **Водяной знак (история с момента обнаружения).** При первом обнаружении беседы бридж читает `GET …/comments?order=desc&limit=1` и записывает этот `id` в `watermark_comment_id` и `cursor_comment_id`. Всё, что было до него, не пересылается. Если комментариев нет, курсор `NULL`, и первое чтение идёт без `after`.
- **`thread_id` в `tg_outbox` не хранится:** он берётся из `conversations` в момент отправки. Поэтому после пересоздания темы очередь сама уходит в новую.
- `body` в `pc_outbox` обнуляется после `done`/`rejected`, как в v1. `html` в `tg_outbox` обнуляется после `sent`. Токенов и ключей в БД нет.

### 2.5. Telegram → Paperclip

1. `getUpdates` с `allowed_updates: ["message"]`. Правки (`edited_message`) не синхронизируются.
2. Фильтр, первое совпадение выигрывает:
   - `chat.id != TELEGRAM_CHAT_ID` → offset сдвигается, ответа нет;
   - `from.id` не в `TELEGRAM_ALLOWED_USER_IDS` → offset, ответа нет (хеш id в логе, как в v1);
   - служебное сообщение о темах → игнорируется;
   - нет `is_topic_message` (General) → подсказка «пишите в тему нужного агента»;
   - тема не связана с беседой, или беседа `removed_at` → подсказка «эта тема не связана с беседой Paperclip»;
   - не текст (медиа, стикер) → подсказка «в v2 передаётся только текст»;
   - иначе → `pc_outbox(pending)` **в одной транзакции** с `telegram_offset = update_id + 1`. Это и обеспечивает идемпотентность по `update_id`.
3. `pc_outbox` → `POST /api/issues/{issue_id}/comments` `{ body, clientRequestId: UUIDv5(NS_BRIDGE, "tg:<chat_id>:<message_id>") }`. Классификация ответов:

| Ответ Paperclip | Действие |
|---|---|
| `201` | `done`, `hot_until` беседы продлевается. Подтверждение в Telegram не отправляется: само сообщение пользователя уже в теме, квитанция агента придёт отдельно (см. 6.4) |
| `409` с `Task is paused` | `rejected`, ответ в теме «беседа на паузе, возобновите её в Paperclip» |
| `409` с `already used for different content` | `rejected`, ERROR в лог (при детерминированном id такого быть не должно) |
| `403 Only the conversation owner…` | `rejected`, ERROR (ошибка конфигурации ключа), ответ в теме |
| `404` (беседа или Agent Chat недоступны) | `rejected`, ответ в теме |
| `422` | `rejected`, ответ в теме с текстом ошибки (до 300 символов) |
| `401` | состояние «ключ отклонён», повтор раз в 5 минут, запись остаётся `pending` |
| `5xx`, `429`, таймаут, сеть | повтор бессрочно с backoff, `Retry-After` для `429`; после `RETRY_NOTIFY_AFTER` — одно сообщение в теме «сообщение в очереди» |

Краш между POST и коммитом `done` → после рестарта `in_flight → pending`, повторный POST с тем же `clientRequestId` возвращает существующий комментарий (`201`) **без второго wake** (1.3). Это **точно один раз** без reconcile.

Длина: Telegram сам ограничивает текст 4096 символами, у Paperclip ограничения длины тела нет. Отдельная проверка длины не нужна.

### 2.6. Paperclip → Telegram

1. `outbound` получает новые комментарии беседы (2.3) и для каждого, прошедшего фильтр (1.6), в одной транзакции делает три вещи: вставляет куски в `tg_outbox` (UNIQUE `(comment_id, chunk)` защищает от повтора), добавляет `id` в `seen_comments` и сдвигает `cursor_comment_id`. Комментарии, не прошедшие фильтр, тоже попадают в `seen_comments` и сдвигают курсор.
2. `sender` отправляет куски по порядку `(created_at, comment_id, chunk)` внутри беседы. Между беседами — FIFO по времени постановки.
3. Если у беседы `topic_state = pending`, сначала `createForumTopic("<issuePrefix> · <agent_name>")` с обрезкой до 128 символов, затем `topic_thread_id` записывается в `conversations`.
4. Ошибки `sendMessage`:
   - `thread not found` → `topic_state = recreating`, `createForumTopic`, новый `topic_thread_id`, повтор куска;
   - `TOPIC_CLOSED` → `reopenForumTopic`, повтор;
   - `429` → ждать `retry_after`;
   - `400 can't parse entities` → повтор того же куска простым текстом без `parse_mode`;
   - `403` (бота удалили из группы, нет прав) → unhealthy и повтор раз в 5 минут.
5. Краш между `sendMessage` и коммитом `sent` → кусок отправляется повторно: дубль в Telegram — принятый компромисс, как в ADR-0004.

**Когда создаётся тема.** Сразу при обнаружении беседы (критерий приёмки 2): задание `createForumTopic` ставится в `sender`. Темы создаются через ту же очередь и тот же лимит 20/мин, поэтому при первом запуске с 28 беседами создание тем займёт около 1,5 минуты. Это ожидаемо.

### 2.7. Конфиг v2

| Переменная | v2 | Примечание |
|---|---|---|
| `PAPERCLIP_URL`, `PAPERCLIP_REQUEST_TIMEOUT`, `PAPERCLIP_UI_URL` | без изменений | |
| `PAPERCLIP_TOKEN_FILE` | без изменений | вариант A, один ключ |
| `PAPERCLIP_TOKEN_FILES` | **новая** | вариант B, список через запятую; взаимоисключается с `PAPERCLIP_TOKEN_FILE` |
| `PAPERCLIP_COMPANY_IDS` | **удалена** | компании обнаруживаются автоматически; если переменная задана, старт отклоняется с сообщением о миграции |
| `PAPERCLIP_POLL_INTERVAL` | смысл изменён, по умолчанию 5 с | опрос `/chats` и горячих бесед |
| `PAPERCLIP_DISCOVERY_INTERVAL` | **новая**, 60 с | список компаний и имена агентов |
| `PAPERCLIP_SWEEP_INTERVAL` | **новая**, 60 с | страховочный обход холодных бесед |
| `CONVERSATION_HOT_WINDOW` | **новая**, 10 мин | |
| `TELEGRAM_CHAT_ID` | смысл изменён | id супергруппы (`-100…`) |
| `TELEGRAM_USER_ID` | **заменена** на `TELEGRAM_ALLOWED_USER_IDS` (список) | если задана старая, старт отклоняется с подсказкой |
| `TELEGRAM_GROUP_MIN_INTERVAL` | **новая**, 3 с | лимит 20 сообщений/мин в группу |
| `TELEGRAM_BOT_TOKEN_FILE`, `TELEGRAM_PROXY_URL`, `TELEGRAM_API_BASE_URL`, `TELEGRAM_LONG_POLL_TIMEOUT`, `TELEGRAM_REQUEST_TIMEOUT`, `CONNECT_TIMEOUT`, `MAX_MESSAGE_CHUNKS`, `RETRY_*`, `STATE_PATH`, `HEALTH_*`, `LOG_*`, `SHUTDOWN_GRACE` | без изменений | |
| `PAPERCLIP_RUNS_POLL_INTERVAL`, `PAPERCLIP_GOVERNED_TIMEOUT`, `GOVERNED_*`, `NOTIFY_RESOLVED_ELSEWHERE`, `NOTIFY_RUN_FAILURES`, `NOTIFY_TASK_COMPLETED` | **удалены** | функций больше нет; если заданы — WARN при старте, старт не блокируется |

**Проверки при старте:**

- `getMe`;
- `getChat(TELEGRAM_CHAT_ID)`: `type == "supergroup"` и `is_forum == true`;
- `getChatMember(TELEGRAM_CHAT_ID, <bot id>)`: `status == "administrator"` и `can_manage_topics == true`;
- версия Paperclip (`paperclip/version.rs`).

Нарушение условий Telegram — понятная ошибка в лог, unhealthy, повтор раз в 5 минут (без падения в цикл рестартов).

### 2.8. Миграция с v1 (для README, этап QA)

1. Создать приватную супергруппу и включить темы (Topics).
2. Добавить бота администратором с правом «Управление темами».
3. Добавить себя; `TELEGRAM_ALLOWED_USER_IDS` = свой `user_id`.
4. `TELEGRAM_CHAT_ID` = id супергруппы.
5. Убрать `PAPERCLIP_COMPANY_IDS` и `TELEGRAM_USER_ID`.
6. Ключ Paperclip остаётся прежним (ADR-0002).
7. Volume `/state` остаётся прежним: миграция 2 очистит таблицы карточек.
8. Проверить, что в Paperclip включён Agent Chat (беседы видны в UI).

## 3. Раздел для Test Engineer: что мокать

### 3.1. Paperclip (mock HTTP)

| Эндпоинт | Обязательное поведение mock |
|---|---|
| `GET /api/health` | с авторизацией — `{status, version, commit}` |
| `GET /api/companies?scope=accessible` | разные наборы компаний для разных ключей (вариант B); `401` для отозванного ключа |
| `GET /api/companies/{id}/agents` | `[{id, name, …}]` |
| `GET /api/companies/{id}/chats` | список бесед с `id`, `identifier`, `conversationAgentId`, `conversationUserId`, `updatedAt`; `404 {"error":"Agent Chat is disabled"}`; `403`; появление новой беседы между циклами; исчезновение беседы |
| `GET /api/issues/{id}/comments` | `order` по умолчанию `desc`; `order=asc`; `limit`; **`after` включает сам якорь** (как живой сервер); неизвестный `after` → `[]`; комментарии всех видов из 1.6, включая `origin: "fast_response"` и `authorType: "system"`, `deletedAt` |
| `POST /api/issues/{id}/comments` | требует `clientRequestId` (иначе `422`); **идемпотентность**: тот же `clientRequestId` и тот же `body` → `201` с тем же комментарием, другой `body` → `409`; `409 {"error":"Task is paused. Resume it before sending a message."}`; `403 Only the conversation owner…`; `5xx`; `429` с `Retry-After`; при успехе поднимает `updatedAt` беседы и (опционально) добавляет квитанцию и ответ агента |

Mock должен считать вызовы POST по `clientRequestId`, чтобы тесты проверяли «ровно один комментарий».

### 3.2. Telegram (mock Bot API, как в v1)

| Метод | Поведение |
|---|---|
| `getMe`, `getChat`, `getChatMember` | варианты: не форум, бот не админ, нет `can_manage_topics` |
| `getUpdates` | сообщения: в тему, в General, в чужой чат, от чужого `user_id`, медиа, служебные `forum_topic_*`, повтор того же `update_id` |
| `createForumTopic` | возвращает растущий `message_thread_id`; учёт вызовов (тема не создаётся повторно после рестарта) |
| `reopenForumTopic` | `true` |
| `sendMessage` | проверка `message_thread_id`, `parse_mode=HTML`, длины ≤ 4096; ошибки `429` + `retry_after`, `400 message thread not found`, `400 TOPIC_CLOSED`, `400 can't parse entities`, `403` |

Mock считает темп отправки, чтобы тест проверял лимит ≤ 20/мин на группу.

### 3.3. Обязательные сценарии

Соответствуют критериям приёмки плана, раздел 5:

1. Две компании, по беседе в каждой: обе темы созданы, ответы приходят в свои темы.
2. Новая беседа появляется на ходу: тема создаётся без рестарта.
3. Фильтр: в Telegram уходят только ответы агента и квитанции; пользовательские и системные комментарии не уходят.
4. История до обнаружения не пересылается (водяной знак).
5. Сообщение из темы → ровно один POST с ожидаемым `clientRequestId`; повтор `update_id` не даёт второго POST.
6. Краши с `fault-injection`: (а) после POST, до коммита `pc_outbox` → после рестарта ровно один комментарий; (б) после `sendMessage`, до коммита `sent` → допускается дубль в Telegram, потерь нет; (в) после чтения комментариев, до коммита курсора → нет потерь и нет дублей.
7. Рестарт: темы не создаются заново, курсоры и offset сохраняются.
8. Allowlist и чужой чат.
9. Нарезка по 4096 и Markdown → HTML; fallback на простой текст.
10. Удалённая тема → пересоздание и доставка; закрытая → reopen.
11. Курсор, включающий якорь, не даёт дублей; пустой ответ при сдвинутом `updatedAt` → fallback по `desc`.
12. Проверки при старте Telegram (не форум, нет прав).

## 4. Рассмотренные альтернативы

| Вариант | Почему нет |
|---|---|
| Встроенный Telegram-канал Paperclip (`chat-channels`) | только webhook, нужен публичный HTTPS; Paperclip за NAT (1.5) |
| WebSocket live events вместо опроса | board-ключ не принимается (1.4) |
| Лента `activity` как единый источник новых комментариев | нет курсора и фильтра по времени, лимит 500, порядок только `desc` (1.4) |
| Хранить id темы в `externalChannelBinding` | поле только для чтения (1.5) |
| JSON-файл вместо SQLite | нет атомарности «курсор + очередь» и «offset + очередь» (ADR-0003) |

## 5. Риски

- **Лимит 20 сообщений/мин на всю супергруппу.** Длинные ответы нескольких агентов одновременно копятся в очереди. Задержка видна в `queue_depth`, при длине очереди больше 60 — WARN. Альтернатива (несколько супергрупп) — вне scope v2.
- **Тексты ошибок Telegram для удалённой и закрытой темы не задокументированы** (1.7). Developer проверяет вживую.
- **Experimental-флаг `enableAgentChat`.** Если его выключат или изменят API бесед, бридж потеряет источник. Это обнаруживается при старте и в цикле (`404 Agent Chat is disabled` → unhealthy).
- **Совпадение кода и живой версии** проверено на `2026.1106.0`/`d69631b`. При обновлении Paperclip нужно перепроверить 1.3 и 1.4 (smoke-тест из 1.3).
- **Вывод 1.3 сделан по коду.** Остаточный риск низкий: в ветке нет ни одной проверки источника actor. Живой smoke-тест можно провести на этапе QA.

## 6. Открытые вопросы (не блокируют разработку; по умолчанию — указанное поведение)

- **6.1.** Название темы при переименовании агента: по умолчанию тема **не** переименовывается (`editForumTopic` не вызывается).
- **6.2.** `/new` (и `/new@<bot>`) из Telegram: по умолчанию отправляется в беседу как `/new`, то есть Paperclip начинает новую сессию агента, как в UI.
- **6.3.** Системные уведомления (`authorType: "system"`, например о сбое запуска агента): по умолчанию **не** пересылаются, как требует решение «только комментарии агента».

  > **Примечание (v2.0.0): решено иначе.** Пользователь в беседе PET-4 решил пересылать служебные сообщения с пометкой ⚠️. Реализация: `domain/comment.rs: should_forward` пропускает `authorType: "system"`; кроме того, сообщения о сбое или отмене запуска, которые Paperclip публикует от имени агента беседы, распознаются по `GET /api/heartbeat-runs/{runId}` (`domain/comment.rs: is_service_notice`) и тоже получают ⚠️. Правило и проверка на живом API — [paperclip-api.md](../paperclip-api.md) §6.1, поведение — [ARCHITECTURE.md](../ARCHITECTURE.md) §4.3. Отдельный ADR не заводился.
- **6.4.** Подтверждение доставки сообщения в Paperclip реакцией (`setMessageReaction` 👀/✅): по умолчанию **нет**, чтобы не тратить лимит. Пользователь видит квитанцию агента.
