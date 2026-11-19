# Paperclip API: контракт для paperclip-telegram-bridge v2

> **Статус:** контракт, по которому реализован bridge v2. Paperclip `2026.1106.0` (`TESTED_PAPERCLIP_VERSION` в `src/paperclip/version.rs`). Доказательства, ссылки на исходники Paperclip и уровни проверки — в [ADR-0010](adr/0010-v2-forum-bridge.md), разделы 1.1–1.7. Реализация: `src/paperclip/` (`dto.rs` — единственное место, знающее JSON), contract-тесты — `tests/paperclip_contract.rs`, mock для интеграционных тестов — `tests/support/paperclip.rs`.
> **Метки проверки** (как в ADR-0010 §0): `[живой API]` — проверено запросом к работающему серверу; `[код]` — только по исходникам; `[не проверено вживую]` — поведение принято по практике, живой проверки не было.
> Все идентификаторы, адреса и токены ниже — плейсхолдеры. Контракт v1 (лента `attention`, карточки, runs) — в истории git до версии 2.0.0.

## 1. Общие правила

- **Base URL:** `PAPERCLIP_URL` (например, `http://paperclip.internal:3100`). Все пути под `/api`. Paperclip вызывается напрямую, без прокси (`.no_proxy()`).
- **Авторизация:** `Authorization: Bearer <board key>`. Один ключ действует во всех компаниях, где у пользователя активное членство (ADR-0010 §1.1) `[код]`. Вариант B: несколько ключей одного пользователя (`PAPERCLIP_TOKEN_FILES`); компания обслуживается первым ключом, который её видит; беседа запоминает ключ (`key_ref` = номер и первые 8 hex `sha256` ключа, сам ключ в БД не хранится).
- **Формат ошибок:** `{"error": "..."}`. Bridge сохраняет текст ошибки `4xx` (до 300 символов) для классификации и сообщения пользователю; текст `5xx` не сохраняется.
- **`X-Paperclip-Run-Id`:** bridge этот заголовок **не отправляет**: в беседе пишет пользователь, а не run.
- **Rate limit:** глобального ограничителя нет; `429` учитывается с `Retry-After` `[код]`.

## 2. Используемые операции

| Операция | Endpoint | Когда |
|---|---|---|
| Health и версия | `GET /api/health` | при старте (первым ключом) |
| Компании | `GET /api/companies?scope=accessible` | каждые `PAPERCLIP_DISCOVERY_INTERVAL` (60 с), каждым ключом |
| Имена агентов | `GET /api/companies/{companyId}/agents` | там же, для названий тем |
| Беседы пользователя | `GET /api/companies/{companyId}/chats` | каждые `PAPERCLIP_POLL_INTERVAL` (5 с) по каждой компании |
| Водяной знак | `GET /api/issues/{issueId}/comments?order=desc&limit=1` | при обнаружении беседы |
| Новые комментарии | `GET /api/issues/{issueId}/comments?order=asc&after=<id>&limit=100` | беседа изменилась, «горячая» или пора страховочного обхода |
| Восстановление курсора | `GET /api/issues/{issueId}/comments?order=desc&limit=100` | пустой ответ по курсору при сдвинутом `updatedAt` |
| Запуск автора комментария | `GET /api/heartbeat-runs/{runId}` | только для пересылаемого однострочного комментария с `createdByRunId` (до 300 символов): распознавание служебных сообщений, §6.1 |
| Сообщение пользователя | `POST /api/issues/{issueId}/comments` | из `pc_outbox` |
| Отзыв ключа | `POST /api/cli-auth/revoke-current` | команда `bridge revoke-paperclip-key` |

Bridge **не** вызывает: создание и изменение задач, карточки (`interactions`), approvals, список и управление runs (кроме чтения одного run, §6.1), `PATCH` задач, управление ключами.

## 3. Health — `GET /api/health` `[живой API]`

```json
{"status":"ok","version":"2026.1106.0","deploymentMode":"authenticated","deploymentExposure":"private"}
```

Без авторизации `version` не приходит, с любым ключом — приходит. Bridge сравнивает `version` с `TESTED_PAPERCLIP_VERSION` по точному совпадению:

| Значение `version` | Запись в логе |
|---|---|
| равна проверенной (после `trim`) | INFO `paperclip reachable`, `version_status="tested"` |
| формат `YYYY.MMDD.N`, но другая | WARN `paperclip version differs from the tested one` |
| поля нет, `null` или пустая строка | WARN `paperclip version is missing` |
| любая другая строка | WARN `paperclip version is not recognized` |

Если `/api/health` не ответил — WARN `paperclip health check failed, continuing`. **Ни один случай не блокирует запуск.**

## 4. Компании — `GET /api/companies?scope=accessible` `[код]`

`scope=accessible` обязателен: без него instance admin получает все компании инстанса, а `/chats` по чужим ответит `403`. Используемые поля: `id`, `name`, `issuePrefix`, `status`. Компании со `status: "archived"` не опрашиваются.

```json
[{"id":"c-1","name":"PET","issuePrefix":"PET","status":"active"}]
```

## 5. Беседы — `GET /api/companies/{companyId}/chats` `[код]`

Задачи с `conversationUserId` = владелец ключа и заданным `conversationAgentId`, по `updatedAt desc`. Одна беседа на пару «агент + пользователь» в компании, поэтому тема называется `«<issuePrefix> · <имя агента>»` (до 128 символов).

```json
[{"id":"issue-1","identifier":"PET-4","conversationAgentId":"agent-1",
  "conversationUserId":"user-1","conversationState":"waiting","updatedAt":"2026-11-19T13:56:56.463Z"}]
```

| Ответ | Действие bridge |
|---|---|
| `200` | новые беседы → водяной знак и тема; пропавшие → `removed_at`; вернувшиеся → снова активны |
| `404 {"error":"Agent Chat is disabled"}` | одно сообщение в General, healthcheck unhealthy до первого успешного ответа |
| `401` | ключ отклонён: одно сообщение в General, unhealthy, повтор раз в 5 минут |
| `403`, другой `404` | компания пропускается до следующего цикла обнаружения |
| `429`, `5xx`, сеть | бэкофф только для этой компании |

Если `conversationUserId` у бесед разных ключей отличается, ключи принадлежат разным пользователям: беседы второго пользователя пропускаются, ERROR в логе, unhealthy.

## 6. Комментарии — `GET /api/issues/{issueId}/comments`

- Порядок по умолчанию — `desc`; bridge передаёт `order` явно. Без `limit` сервер отдаёт все комментарии, максимум 500 `[код]`.
- **`after` возвращает сам якорь повторно** `[живой API]`: bridge отбрасывает уже виденные `id` (`seen_comments`). Неизвестный `after` → `[]` без ошибки `[код]`.
- `updatedAt` беседы растёт с каждым комментарием, кроме квитанций (`origin: "fast_response"`) `[код]`. Поэтому после активности беседа 10 минут «горячая» (`CONVERSATION_HOT_WINDOW`) и читается каждый цикл; холодные беседы читаются раз в `PAPERCLIP_SWEEP_INTERVAL`.

**Что пересылается в Telegram** (`domain::should_forward`, ADR-0010 §1.6, §6.3): `deletedAt == null && (authorType == "agent" && authorAgentId == conversationAgentId || authorType == "system")`. Это ответы агента беседы, квитанции Paperclip и системные уведомления (последние — с пометкой ⚠️, §6.1). Не пересылаются: сообщения пользователя (в том числе отправленные самим bridge — защита от эха), комментарии других агентов, удалённые.

```json
{"id":"c-1","body":"**Готово**","authorType":"agent","authorAgentId":"agent-1",
 "origin":"comment","createdAt":"2026-11-19T10:00:00.000Z","deletedAt":null}
```

### 6.1. Служебные сообщения: пометка ⚠️ `[живой API]`

Решение пользователя (PET-4, ответ на вопрос 6.3 ADR): служебные сообщения Paperclip в беседе уходят в тему с префиксом `⚠️ `. Служебными считаются:

1. комментарии `authorType: "system"` `[код]`: пометка без дополнительных запросов;
2. сообщения о сбое или отмене запуска (`ACP agent reported a terminal limit failure.`, `cancelled` и т. п.), которые Paperclip публикует от имени агента беседы `[живой API]`: правило ниже.
 Обычные ответы агента идут без пометки, комментарии пользователя по-прежнему не пересылаются.

Поля комментария сообщение второго вида не выделяют. Проверено на живом инстансе 2026-11-19: у сообщения `ACP agent reported a terminal limit failure.` тот же `authorType: "agent"` и `authorAgentId` агента беседы, `origin: "comment"`, `presentation: null`, а `metadata.authorizationReason: "internal_agent_write"` бывает и у обычных ответов. Отличает его запуск, указанный в `createdByRunId`:

```json
GET /api/heartbeat-runs/{runId}
{"id":"…","status":"failed","errorCode":"provider_quota",
 "error":"ACP agent reported a terminal limit failure.\nYou've hit your session limit · …"}
```

Правило (`domain::is_service_notice`, без учёта регистра): `status` запуска — `failed`, `cancelled` или `timed_out`, а текст комментария совпадает с первой строкой `error` либо с самим `status` (сообщение `cancelled` при `error: "Cancelled before issue reassignment"`). Обычный ответ, написанный запуском, который потом упал, правилу не соответствует: его текст не совпадает с ошибкой.

Запуск запрашивается только для комментария, который и так пересылается (`should_forward`), с `createdByRunId`, не квитанции, в одну строку длиной до 300 символов. Если запрос не удался (`403`, `404`, сеть), комментарий уходит как обычный ответ, без пометки; в лог пишется DEBUG.

## 7. Сообщение пользователя — `POST /api/issues/{issueId}/comments` `[код]`

```json
{"body":"текст из Telegram","clientRequestId":"<UUIDv5>"}
```

`clientRequestId` = UUIDv5 от `tg:<chat_id>:<message_id>` в пространстве имён bridge (UUIDv5 от URL репозитория; значение закреплено тестом `core::inbound`). Сервер идемпотентен по `(issueId, authorUserId, clientRequestId)`: повтор с тем же телом возвращает существующий комментарий и не будит агента второй раз. Комментарий board-ключом будит агента так же, как сообщение из UI (ADR-0010 §1.3) `[код]`.

| Ответ | Действие bridge |
|---|---|
| `201` | `done`; беседа становится «горячей» |
| `409` `Task is paused…` | `rejected`, в теме: «беседа на паузе» |
| `409` `…already used for different content` | `rejected`, ERROR (при детерминированном id не ожидается) |
| `403` `Only the conversation owner…` | `rejected`, ERROR, сообщение в теме про ключ |
| `404` | `rejected`, сообщение в теме |
| `400`, `422` | `rejected`, в теме текст ошибки (до 300 символов) |
| `401` | остаётся `pending`, ключ отклонён, повтор раз в 5 минут |
| `429`, `5xx`, таймаут, сеть, нечитаемый ответ | повтор с бэкоффом без ограничения числа попыток; после `RETRY_NOTIFY_AFTER` попыток — одно сообщение «в очереди» |

## 8. Telegram: ошибки тем `[не проверено вживую]`

Документация Bot API не описывает ответ `sendMessage` в удалённую или закрытую тему (ADR-0010 §1.7). Классификатор распознаёт ошибки `400` **по подстроке без учёта регистра**; все подстроки собраны в одном месте — модуль `telegram::topic_errors` (`src/telegram/mod.rs`):

| Ситуация | Ожидаемый текст (по практике) | Подстроки | Действие |
|---|---|---|---|
| Тема удалена | `Bad Request: message thread not found` | `thread not found`, `topic_deleted` | тема создаётся заново, очередь уходит в новую |
| Тема закрыта | `Bad Request: TOPIC_CLOSED` | `topic_closed` | `reopenForumTopic`, повтор |
| Ошибка разметки | `Bad Request: can't parse entities: …` | `can't parse entities` | тот же кусок простым текстом |

**Эти тексты вживую не проверялись**: тестовой супергруппы на этапе разработки не было. Сценарий проверки на этапе приёмки:

1. Удалить тему беседы в Telegram → агент отвечает в беседе → ожидается новая тема с тем же названием и ответ в ней; в логе WARN `forum topic not found, recreating`.
2. Закрыть тему беседы → агент отвечает → ожидается, что тема снова открыта и ответ в ней; в логе INFO `forum topic reopened`.

Если фактический текст ошибки отличается, меняется только список подстрок в `telegram::topic_errors` и этот раздел.

## 9. Нагрузка опроса

4 компании по 7 бесед: в покое около 4 запросов `/chats` за 5 с плюс 28 запросов комментариев за 60 с, примерно 1,3 запроса в секунду; активная беседа добавляет 1 запрос в 5 с (ADR-0010 §2.3).

## 10. Не используется bridge, но важно знать

- WebSocket `/api/companies/{companyId}/events/ws` не принимает board-ключ (ADR-0010 §1.4).
- `GET /api/companies/{companyId}/activity` — без курсора и фильтра по времени, не подходит как источник.
- `externalChannelBinding` / `externalConversationState` задачи — вычисляемые поля только для чтения; связь «тема ↔ беседа» хранится в SQLite bridge (ADR-0010 §1.5).
- Встроенный Telegram-канал Paperclip работает только через webhook и требует публичного HTTPS, поэтому не подходит (ADR-0010 §1.5).
