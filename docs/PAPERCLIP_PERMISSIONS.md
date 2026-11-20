# Доступ bridge к Paperclip API

Перечень составлен по `src/paperclip/http.rs` (реальные вызовы клиента, версия 2.0.0, коммит `194eae5`) и контракту [paperclip-api.md](paperclip-api.md). Серверные требования выведены из кода Paperclip в [ADR-0010](adr/0010-v2-forum-bridge.md) §1. С board-ключом на живом сервере они проверены не все: уровень проверки указан в контракте.

## Тип учётных данных

Bridge работает с **board API key** (`pcp_board_…`) пользователя, чьи беседы с агентами зеркалируются ([ADR-0002](adr/0002-board-api-key.md)). Agent-ключи и run JWT не подходят: `GET /companies/{id}/chats` и публикация в беседу требуют actor `board` с `userId` (ADR-0010 §1.2, §1.3).

- **Вариант A (основной):** один ключ на все компании, `PAPERCLIP_TOKEN_FILE`. Ключ действует во всех компаниях, где у пользователя активное членство (ADR-0010 §1.1).
- **Вариант B:** несколько ключей **одного** пользователя, `PAPERCLIP_TOKEN_FILES` (файлы через запятую). Компанию обслуживает первый ключ, который её видит. Беседа запоминает свой ключ (`key_ref`), и комментарии в неё публикуются этим же ключом. Задать обе переменные одновременно нельзя.

У board key нет отдельных scopes: он несёт **полный доступ пользователя-владельца** и в нашем случае бессрочен. «Минимальные права» достигаются не настройкой ключа, а тем, что bridge вызывает только операции из таблицы ниже.

Требования к владельцу ключа:

- **он же владелец бесед**: публиковать в беседу может только `conversationUserId`, иначе `403 Only the conversation owner can send messages…`. Bridge видит только беседы владельца ключа, потому что так работает `/chats`. Если ключи в варианте B принадлежат разным пользователям, беседы второго пользователя пропускаются с ERROR в логе (ARCHITECTURE §4.2);
- **активное членство** в компаниях, которые нужно обслуживать. Роль должна позволять писать комментарии, то есть не `viewer`;
- в Paperclip **включён Agent Chat** (experimental-флаг `enableAgentChat`), иначе `404 "Agent Chat is disabled"`.

Заголовок: `Authorization: Bearer <token>`. Заголовок `X-Paperclip-Run-Id` не отправляется. Подключение к Paperclip идёт без proxy (`.no_proxy()`).

## Вызываемые операции

Все пути относительно `PAPERCLIP_URL`, под префиксом `/api`.

| Операция | Метод и путь | Когда | Каким ключом |
|---|---|---|---|
| Проверка доступности и версии | `GET /api/health` | один раз при старте | первым |
| Компании | `GET /api/companies?scope=accessible` | каждые `PAPERCLIP_DISCOVERY_INTERVAL` (60 с) | каждым |
| Имена агентов | `GET /api/companies/{companyId}/agents` | там же, для названий тем | ключом компании |
| Беседы пользователя | `GET /api/companies/{companyId}/chats` | каждые `PAPERCLIP_POLL_INTERVAL` (5 с) по каждой компании | ключом компании |
| Водяной знак | `GET /api/issues/{issueId}/comments?order=desc&limit=1` | при обнаружении беседы | ключом компании |
| Новые комментарии | `GET /api/issues/{issueId}/comments?order=asc&after=<id>&limit=100` | беседа изменилась, «горячая» или пора страховочного обхода | ключом компании |
| Восстановление курсора | `GET /api/issues/{issueId}/comments?order=desc&limit=100` | пустой ответ по курсору при сдвинутом `updatedAt` | ключом компании |
| Запуск автора комментария | `GET /api/heartbeat-runs/{runId}` | только для пересылаемого однострочного (до 300 символов) комментария агента беседы с `createdByRunId`, не квитанции: распознавание служебного сообщения ([paperclip-api.md](paperclip-api.md) §6.1). Читаются только `status` и `error` | ключом компании |
| Сообщение пользователя | `POST /api/issues/{issueId}/comments` с телом `{"body": …, "clientRequestId": "<UUIDv5>"}` | сообщение из темы, через `pc_outbox` | ключом беседы (`key_ref`) |
| Отзыв ключа | `POST /api/cli-auth/revoke-current` | только подкоманда `bridge revoke-paperclip-key` | ключом из `PAPERCLIP_TOKEN_FILE` |

`issueId` в запросах на чтение и запись комментариев берётся только из бесед, найденных через `/chats`.

### Bridge публикует комментарии

В v2 bridge **пишет** комментарии в беседы. Комментарий board-ключом неотличим от сообщения из UI: `authorType: "user"`, `authorUserId` владельца ключа, источник не сохраняется (ADR-0010 §1.3). Последствия:

- комментарий будит агента беседы, как сообщение из UI. Повтор с тем же `clientRequestId` второй раз агента не будит;
- `/new` начинает новую сессию агента;
- **`supersedeOnUserComment`** (ADR-0010 §1.8): pending-карточки задачи с этим флагом, созданные до комментария, получают статус `expired` (`superseded_by_comment`). Так же происходит при сообщении из UI. Исключение — `ask_user_questions` в беседе Agent Chat, она комментарием не снимается. Карточки в Telegram v2 не показываются, о них пользователь узнаёт из ответа агента или в UI.

## Больше не вызывается с v2

| Вызов v1 | Чем заменён |
|---|---|
| `GET /api/companies/{id}/attention` (лента карточек) | `/chats` и комментарии бесед |
| `GET /api/issues/{id}/interactions`, `POST …/interactions/{id}/accept\|reject\|respond\|verdicts` | карточки в Telegram не показываются и не решаются |
| `GET /api/issues/{id}/approvals`, `POST /api/approvals/{id}/approve\|reject` | governed из Telegram удалён |
| `GET /api/companies/{id}/issues?status=…` | команды `/status`, `/backlog` удалены |
| `GET /api/companies/{id}/heartbeat-runs` (список) | уведомления о сбоях run удалены. Читается только один запуск по id, см. таблицу выше |
| `GET /api/companies/{id}/live-runs` | команда `/status` удалена |

## Что bridge не вызывает

Создание и изменение задач (`POST`/`PATCH /issues`), комментарии вне бесед Agent Chat, карточки и их решения, approvals, управление runs (отмена, перезапуск) и список runs, endpoints секретов, управление ключами (кроме `revoke-current` по явной команде), WebSocket live events.

## Обработка ответов сервера

Подробные таблицы — в [ARCHITECTURE.md](ARCHITECTURE.md) §4.2 (ошибки `/chats`) и §4.4 (ответы на `POST`). Кратко:

- `401` от любого вызова — «ключ отклонён»: одно сообщение в General на ключ, healthcheck unhealthy, повтор раз в 5 минут;
- `404 "Agent Chat is disabled"` на `/chats` — одно сообщение в General, unhealthy;
- другие `403`/`404` на `/chats` — компания пропускается до следующего обнаружения;
- `POST`: `403`, `404`, `409` (пауза), `4xx` — сообщение отклонено, причина показана в теме. `429` учитывает `Retry-After`. `5xx` и сетевые ошибки повторяются бессрочно с backoff, тот же `clientRequestId` делает повтор безопасным;
- `GET /heartbeat-runs/{id}`: любая ошибка — комментарий уходит без ⚠️, в лог пишется DEBUG.

## Как проверить доступ после настройки

1. `docker compose run --rm bridge check-config` проверяет конфигурацию, число ключей и сборку клиентов. **Сетевых вызовов не делает.**
2. При старте bridge вызывает `GET /api/health`. В лог попадает `paperclip reachable` с версией или `paperclip health check failed, continuing`. Этот вызов ключ не проверяет.
3. Работоспособность ключа видно по первому циклу обнаружения (до `PAPERCLIP_POLL_INTERVAL` после старта):
   - в логе `companies discovered` со списком префиксов компаний, затем `conversation discovered` для каждой беседы, и в супергруппе появляются темы;
   - `401` — в General придёт «Paperclip отклонил ключ бриджа (401)», `bridge healthcheck` вернёт unhealthy;
   - `404 Agent Chat is disabled` — в General придёт «В Paperclip выключен Agent Chat», unhealthy;
   - компании нет в списке: ключ не видит компанию (нет активного членства) или компания архивная.
4. Публикацию проверяет первое сообщение в теме. Если ключ принадлежит не владельцу беседы, в теме появится «ключ бриджа принадлежит не владельцу беседы». Ручной smoke-тест через `curl` — ADR-0010 §1.3.

Где в UI Paperclip выпускается и отзывается ключ, в репозитории не описано (не проверено). API-способы и отзыв ключа описаны в [RUNBOOK](RUNBOOK.md) и ADR-0002.
