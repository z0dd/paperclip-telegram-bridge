# Paperclip API: контракт для paperclip-telegram-bridge

> **Статус:** контракт, по которому реализован bridge (исследование этапа 1, ревизия 4, с governed-подтверждениями §6.6). Paperclip `2026.1106.0`, `deploymentMode: authenticated`, `deploymentExposure: private`. Реализация: `src/paperclip/` (`dto.rs` — единственное место, знающее JSON), contract-тесты — `tests/paperclip_contract.rs`, `tests/governed.rs`.
> **Источники, по убыванию веса:** (1) реальные запросы к работающему серверу 2026-11-15; (2) эксперименты в тестовой задаче-песочнице и тестовые формы `ask_user_questions` (см. §6.1); (3) исходники сервера той же сборки (`server/src/...`, ссылки `файл:строка`); (4) `GET /api/openapi.json` (818 путей).
> **Метки проверки:** `[запрос]` — подтверждено реальным запросом; `[эксперимент]` — подтверждено экспериментом в песочнице; `[код]` — только по исходникам; `[не проверено]` — нужен board API key, которого на этапе исследования не было (токен ради исследования не выпускался).
> Все идентификаторы, адреса и токены ниже — плейсхолдеры.

## 0. Короткие выводы для архитектуры

1. **Bridge нужен board API key** (`pcp_board_…`) пользователя, который отвечает на вопросы. Agent key / agent JWT не может отвечать на `human_only` карточки (`403 interaction_human_only`) `[эксперимент]` и не видит company-wide ленту `attention` (`403 Board access required`) `[запрос]`.
2. **Board API key нельзя сузить.** У него нет scope: он несёт **полный доступ своего пользователя** ко всем его компаниям, включая права instance-admin, если они есть (`server/src/middleware/auth.ts:339-355`, `services/board-auth.ts:72-111`, таблица `board_api_keys` без колонок scope) `[код]`. По умолчанию срок 30 дней, `expiresAt: null` = бессрочно (`board-auth.ts:17,231`). Это главный риск безопасности. **Решение пользователя (гейт A):** бессрочный ключ основного пользователя, поэтому основная мера защиты — быстрый отзыв (§2, runbook в архитектуре).
3. **Источник pending-вопросов — `GET /api/companies/{companyId}/attention`**: одна company-wide лента, только `pending`, уже отфильтрованная по адресату. Payload карточки в ленте нет: его берём из `GET /api/issues/{issueId}/interactions`. Курсор ленты нестабилен, поэтому опрашиваем первую страницу без курсора.
4. **Повторный ответ на обычную карточку никогда не даёт 200.** Любой accept/reject/respond по уже разрешённой карточке возвращает `409` с `code` `[эксперимент]`. **Исключение — governed `toolAction`:** повтор того же решения возвращает `200` с текущей записью, а ошибки tool gateway приходят как `409` без `code` или `500` (§6.6) `[код]`. Разрешение сделано условным `UPDATE … WHERE status='pending'`: в гонке побеждает ровно один `[эксперимент: 6 параллельных → 1×200, 5×409]`. Idempotency key для ответов нет, только для создания.
5. **У run нет курсора «с момента X».** `GET /api/companies/{companyId}/heartbeat-runs` отдаёт последние N (по `createdAt DESC`, до 1000). Bridge дедуплицирует по `id` у себя. Лента `attention` дополнительно отдаёт `failed_run` после исчерпания retry.
6. **Глобального rate limit у Paperclip API нет** `[код]`. Есть локальные лимиты: `429 + Retry-After` на конкурентные одинаковые запросы списка задач (`routes/issues.ts:8593-8611`).

## 1. Общие правила

- **Base URL:** `PAPERCLIP_URL` (например, `http://paperclip.internal:3100`). Все пути под `/api`.
- **Авторизация:** `Authorization: Bearer <token>`. Порядок распознавания на сервере: board key → agent key → agent run JWT (`middleware/auth.ts:339,361,369`) `[код]`.
- **Формат ошибок:** `HttpError` → `{"error": "...", "code"?: "..."}`; ошибки аудитории → `{"error","code","details"}`; ошибки валидации Zod → `400 {"error":"Validation error","details":[...]}` (`middleware/error-handler.ts:195-249`) `[код]`, `[эксперимент]` для 409/422/404.
- **Коды:** `400` — тело не прошло схему; `401` — нет/неверный токен; `403` — нет прав или не та аудитория; `404` — нет ресурса; `409` — конфликт состояния (уже разрешено, stale, superseded); `422` — тело валидно, но нарушает правила карточки; `429` — локальный лимит; `5xx` — временная ошибка.
- **CSRF:** board key не проходит через browser CSRF guard (`middleware/board-mutation-guard.ts:93`) `[код]`, так что дополнительные заголовки не нужны.
- **`X-Paperclip-Run-Id`:** bridge этот заголовок **не отправляет**: у него нет run.

## 2. Авторизация и минимальные права

| Тип токена | Как получить | Срок | Что может для bridge | Вердикт |
|---|---|---|---|---|
| Board API key `pcp_board_<48 hex>` | `POST /api/board-api-keys` от имени пользователя (UI/CLI) | default 30 дней, `null` = бессрочно | читает `attention`, задачи, runs; отвечает на `human_only` и на карточки, адресованные этому пользователю | **Нужен** |
| Agent API key | `POST /api/agents/{id}/keys` (board only), scopes `standard` / `task_bridge` / `skill_test` | без срока, только revoke | не отвечает на `human_only`; без run получает `422 interaction_run_attribution_required`; не видит `attention`; `task_bridge` не видит список задач компании | Не подходит |
| Agent run JWT | выдаётся на heartbeat | 48 ч | то же, плюс привязан к одному run и задаче | Не подходит |
| MCP OAuth / `pcgw_*` | публичный MCP gateway | — | не для этого сценария | Не подходит |

**Минимально необходимые права.** Сузить board key нельзя, поэтому «минимальность» обеспечивается выбором пользователя-владельца ключа, а не scope:
- bridge должен действовать от имени **того же человека**, который отвечает на вопросы (иначе `403 interaction_addressee_mismatch` на карточках с `addresseeUserId` `[код]`, `services/issue-thread-interaction-resolution.ts:183-196`);
- пользователь нужен с ролью не ниже участника компании (не `viewer`), иначе нет права разрешать карточки;
- снизить риск можно только организационно: отдельный пользователь Paperclip только для bridge с членством в одной компании (тогда в `resolvedByUserId` будет он, а не основной пользователь), явный `expiresAt` и ротация. Варианты — в блокирующих вопросах.

**Фактически используемые bridge операции** (это и есть перечень требуемых прав для README):

| Операция | Endpoint | Требование на сервере |
|---|---|---|
| Проверка токена и версии | `GET /api/health`, `GET /api/companies/{companyId}` | доступ к компании |
| Лента pending-решений | `GET /api/companies/{companyId}/attention` | company access + board + userId (`routes/attention.ts:20-25`) |
| Карточки задачи | `GET /api/issues/{issueId}/interactions` | чтение задачи |
| Ответы | `POST …/accept`, `…/reject`, `…/respond`, `…/verdicts` | board (`routes/issues.ts:6038-6070`) |
| Задачи для `/status`, `/backlog` | `GET /api/companies/{companyId}/issues` | company access |
| Runs | `GET /api/companies/{companyId}/heartbeat-runs`, `GET /api/heartbeat-runs/{runId}` | company access + `company_scope:read` |
| Governed `toolAction`: accept / reject | те же `POST …/accept`, `…/reject` | актор — человек (board key даёт `user`), `human_only` принудительно (§6.6) |
| Governed `secretProposal`: accept | `POST …/accept` | плюс право `agent_config:update` на целевого агента (`services/secret-proposal-authorization.ts`) |
| Formal approval для destructive `toolAction` | `GET /api/issues/{issueId}/approvals`, `POST /api/approvals/{approvalId}/approve`, `…/reject` | board (`routes/approvals.ts:296-297,412-413`) |

Bridge **не** вызывает: создание/изменение задач, комментарии, `skip`, `cancel`, `dismiss`, `withdraw`, отмену runs, управление ключами, endpoints секретов и secret proposals (`/api/companies/{id}/secrets…`), approvals, не связанные с `toolAction`.

**Ротация и отзыв:** `DELETE /api/board-api-keys/{keyId}` или `POST /api/cli-auth/revoke-current` (отзывает ключ, которым сделан запрос); список — `GET /api/board-api-keys?includeInactive=true` (`routes/access.ts:2900-2978`) `[код]`.

## 3. Health

### `GET /api/health` `[запрос]`
Без авторизации. Пример ответа (сокращён):
```json
{"status":"ok","version":"2026.1106.0","deploymentMode":"authenticated","deploymentExposure":"private"}
```
Bridge при старте логирует `version` и предупреждает, если она не из проверенного диапазона.

## 4. Лента pending-решений: `GET /api/companies/{companyId}/attention`

**Проверка:** agent-токен → `403 {"error":"Board access required"}` `[запрос]`; ответ board-ключом `[не проверено]`, схема по коду (`routes/attention.ts:18-57`, `services/attention.ts`).

**Query:**
| Параметр | Значения | Заметки |
|---|---|---|
| `limit` | 1–100, default 50 | bridge: 100 |
| `sort` | `activity` (default, новые `activityAt` сверху) / `decide` | bridge: `activity` |
| `queue` | `questions`, `plans`, `prs` (seeded) | bridge не фильтрует: нужны и confirmations |
| `activitySince`, `activityUntil` | ISO date-time | фильтр по `activityAt` (= `updatedAt` карточки) |
| `cursor` | opaque base64url `{v,sort,id}` | **нестабилен**: если последний элемент страницы уже разрешён → `400 "Attention cursor no longer matches the filtered feed"` |
| `includeDismissed`, `archived`, `all` | `true`/`false` | `all` нельзя с `cursor`/`limit` |

**Ответ:**
```json
{
  "companyId": "<companyId>",
  "generatedAt": "2026-11-15T12:00:00.000Z",
  "totalCount": 3,
  "deskBadgeCount": 3,
  "nextCursor": null,
  "countsBySourceKind": {"issue_thread_interaction": 2, "failed_run": 1},
  "items": [
    {
      "id": "issue_thread_interaction:interaction:<interactionId>",
      "sourceKind": "issue_thread_interaction",
      "activityAt": "2026-11-15T11:59:00.000Z",
      "decisionVerbs": ["accept", "reject"],
      "subject": {
        "kind": "interaction", "id": "<interactionId>", "status": "pending", "title": "…",
        "href": "/<PREFIX>/issues/<PREFIX>-1#interaction-<interactionId>",
        "metadata": {"kind": "request_confirmation", "issueId": "<issueId>", "createdByAgentId": "<agentId>", "isPlanTarget": false, "targetDocumentKey": null}
      },
      "relatedIssue": {"id": "<issueId>", "identifier": "<PREFIX>-1", "title": "…"},
      "resolverAudience": {"effectivePolicy": "human_only", "addresseeUserId": "<userId>", "...": "..."}
    }
  ]
}
```

**Особенности (по коду):**
- Только `status='pending'`. Исключены карточки, адресованные агентам (`addresseeAgentId` вызываемого агента), и карточки с `addresseeUserId` другого пользователя. То есть лента уже содержит ровно то, на что владелец ключа может ответить.
- Несколько pending `request_confirmation` в одной задаче схлопываются до самой новой (`services/attention.ts:763-787`). Старые и так истекают при создании новой (см. §6.5).
- `dismissedAt` карточки лента не фильтрует: dismiss — отдельный механизм UI.
- Есть элементы `sourceKind: "failed_run"`: failed/timed_out run с исчерпанным bounded retry и без более нового run для той же пары агент+задача (`services/attention-exhausted-runs.ts:4-51`). В `metadata` есть `agentId`, `issueId`, `errorCode`, `error`, `retryExhaustedReason`.
- **Payload карточки в ленте нет.** Для рендера нужен `GET /api/issues/{issueId}/interactions`.
- Из-за схлопывания governed-карточка может быть скрыта за более новым обычным подтверждением той же задачи. Поэтому bridge для каждой задачи из ленты берёт **все** `pending` из `GET /api/issues/{issueId}/interactions`, а не только тот ID, что в ленте.
- Элементы `sourceKind: "approval"` — board approvals компании, `status: pending` (`services/attention.ts:1129-1170`) `[код]`. Среди них formal approval для destructive `toolAction` (`type: request_board_approval`, `payload.source: "tool_gateway"`, `payload.actionRequestId`). Отдельной карточкой bridge их не показывает (§6.6), остальные типы approvals в v1 игнорирует.

**Как опрашивать:** первая страница `?limit=100&sort=activity` без курсора, каждые `PAPERCLIP_POLL_INTERVAL` (default 15 с). Если `totalCount > 100`, дочитываем по `cursor`; при `400` на курсоре перезапрашиваем первую страницу. Новые для bridge = `subject.id`, которых нет в state. Пропавшие из ленты = разрешены где-то ещё (UI, другой клиент).

## 5. Карточки задачи: `GET /api/issues/{issueId}/interactions`

**Проверка:** `[запрос]`, `[эксперимент]`. Работает и agent-токеном.

Возвращает **все** карточки задачи (pending и разрешённые), по `createdAt, id`. Фильтров и пагинации нет.

**Важно:** если задача `done`/`cancelled`, pending-строки **показываются** как `status: "expired"`, `result.outcome: "issue_closed"`, хотя в БД они ещё `pending` (`services/issue-thread-interactions.ts:3054-3066`) `[код]`.

**Запись карточки** (реальный ответ, ID заменены):
```json
{
  "id": "<interactionId>", "companyId": "<companyId>", "issueId": "<issueId>",
  "kind": "request_confirmation", "status": "pending",
  "continuationPolicy": "none",
  "requestedResolverPolicy": "anyone", "effectiveResolverPolicy": "anyone",
  "resolverPolicyProvenance": "explicit", "effectiveResolverPolicySource": "requested",
  "idempotencyKey": "…", "originCommentIds": [], "sourceCommentId": null,
  "sourceIdentityContextId": "<uuid>", "sourceRunId": "<runId>",
  "title": "…", "summary": null,
  "createdByAgentId": "<agentId>", "createdByUserId": null,
  "addresseeAgentId": null, "addresseeUserId": null,
  "resolvedByAgentId": null, "resolvedByRunId": null, "resolvedByUserId": null,
  "payload": {"version": 1, "prompt": "…", "acceptLabel": "Approve", "rejectLabel": "Reject",
              "rejectRequiresReason": true, "allowDeclineReason": true,
              "detailsMarkdown": "…", "supersedeOnUserComment": false},
  "result": null, "resolvedAt": null, "dismissedAt": null, "dismissedByUserId": null,
  "createdAt": "2026-11-15T17:28:52.897Z", "updatedAt": "2026-11-15T17:28:52.897Z",
  "resolverPolicy": "anyone",
  "legacyResolverPolicyAliases": {"requested": "board_or_agents", "effective": "board_or_agents"}
}
```
Поле `acceptanceBlocker: "workspace_sync_pending"` может появиться, когда accept временно невозможен (`409`).

**Статусы:** `pending | accepted | rejected | answered | cancelled | expired | failed` (`packages/shared/src/constants.ts:332-340`). Срока жизни по времени у карточек нет: колонки `expiresAt` нет.

**Kinds:** `ask_user_questions`, `request_confirmation`, `request_checkbox_confirmation`, `request_item_verdicts`, `suggest_tasks`, `connection_intent`.

| Kind | Поддержка в v1 bridge | Действия |
|---|---|---|
| `ask_user_questions` (вкл. `questionSet`) | да | `respond` |
| `request_confirmation` | да | `accept`, `reject` |
| `request_checkbox_confirmation` | да | `accept` с `selectedOptionIds`, `reject` |
| `request_item_verdicts` | да (по пунктам) | `verdicts` |
| `suggest_tasks` | **нет**: только уведомление со ссылкой (создание задач из Telegram вне scope) | — |
| `connection_intent` | **нет**: только ссылка, разрешается своими endpoints | — |
| governed confirmation (`payload.toolAction` / `payload.secretProposal`) | **да** (решение пользователя на гейте A), особые правила §6.6 | `accept` (без `rememberAction`), `reject`; для destructive сначала formal approval |
| `request_confirmation` с `payload.connectionAuthorization` («Подключите ваш сервис») | **нет**: только уведомление, OAuth проходит в UI | — |

## 6. Схемы payload и ответов по kinds

### 6.1 `ask_user_questions`
**Payload (две эквивалентные формы, сервер хранит обе):**
- канонический `payload.questionSet` (`schema: "paperclip.question_set.v1"`): `questions[]` из `{id, header?, prompt, helpText?, required, answerMode: single_select|multi_select|text, options?[{id,label,description?,recommended?}], customAnswer?{enabled:true,label?,placeholder?}, initialText?, textValidation?{minLength,maxLength,pattern,inputType: text|number|integer,minimum,maximum}}`; плюс `title?`, `description?` (до 100 000 символов), `submitLabel?`. До 64 вопросов.
- совместимый `payload.questions[]`: `{id, prompt, helpText?, selectionMode: single|multi, required, allowOther, options[{id,label,description?,freeText?}]}`. Сервер генерирует его из `questionSet`: `customAnswer` превращается в дополнительную опцию `paperclip_custom_answer` с `freeText: true`, а text-вопрос — в одну опцию `paperclip_text_answer` `[запрос: реальная форма]`, `[эксперимент]`, `packages/shared/src/question-set.ts:23-30`.
- Карточка, созданная только в legacy-формате `payload.questions[]`, хранится **без** `questionSet`, а `allowOther: true` не добавляет опцию `paperclip_custom_answer` `[эксперимент]`.

Bridge рендерит **по `questionSet`, если он есть**, иначе по `questions`.

**Ответ:** `POST /api/issues/{issueId}/interactions/{interactionId}/respond`
```json
{"answers": [
  {"questionId": "license", "optionIds": ["apache2"]},
  {"questionId": "notes", "optionIds": [], "otherText": "свободный текст"},
  {"questionId": "pick", "optionIds": [], "otherText": "свой вариант"}
], "summaryMarkdown": null}
```
- single/multi select: `optionIds` с id опций; text: `optionIds: []` + `otherText`; свой вариант (customAnswer): `optionIds: []` (или с `paperclip_custom_answer`) + `otherText`. Для single-select нельзя одновременно реальную опцию и свой текст → `422`.
- **Не больше 20 ответов за запрос**, хотя вопросов может быть до 64 (`validators/issue.ts:2202-2208` vs `1463`): 21 ответ → `400 Validation error`, `code: too_big, maximum: 20` `[эксперимент]`. Форма с >20 отвечаемыми вопросами через API не отвечается: bridge показывает ссылку в UI.
- **Частичный ответ не принимается:** каждый `required` вопрос без ответа → `422 "Question <id> requires an answer"`. Необязательные можно пропустить: форма только из необязательных вопросов принимает `{"answers":[]}` → `200 answered`, `result.answers: []` `[эксперимент]`. Пустой `optionIds` у обязательного multi-select тоже даёт `422 requires an answer` `[эксперимент]`. Поэтому форма собирается в state bridge целиком и отправляется одним запросом.
- Ошибки `422` `[эксперимент]`, проверка идёт до первой ошибки:
  - `Question q_multi requires an answer` (частичная форма);
  - `Question q_single only allows one answer` (две опции в single-select);
  - `Unknown optionId for question q_single: zz`, `Unknown questionId: nope`, `Duplicate answer for questionId: q_single` — тело `{"error": "…"}` без `code`;
  - `{"error":"/response/answers/q_single: single-select answers cannot select an option and a custom answer","code":"invalid_question_response"}`;
  - `textValidation`: `/response/answers/q_text: must be at most 10`, `…: must be a valid integer` с тем же `code: invalid_question_response`.
  Bridge проверяет форму сам (те же правила) до отправки, а любой `422` считает постоянной ошибкой: ответ не повторяет, сообщает пользователю и предлагает ответить в UI.
- Text-вопрос принимается и с `optionIds: []`, и с `optionIds: ["paperclip_text_answer"]` + `otherText`; свой вариант в single-select — `optionIds: []` + `otherText` `[эксперимент]`. Сервер хранит ответ как прислали.
- Хранимый результат, статус `answered`: `{"version":1,"answers":[{"questionId":"license","optionIds":["apache2"]}],"summaryMarkdown":null}` `[запрос: реальная форма, ответ пользователя из UI: resolvedByUserId заполнен, resolvedByAgentId/RunId = null]`.
- Ответ на `respond` по другому kind → `422 "Only ask_user_questions interactions can be answered"` `[эксперимент]`.
- Ответ агента (`resolverPolicy: anyone`): `status: answered`, `resolvedByAgentId` и `resolvedByRunId` заполнены, `resolvedByUserId: null`. Пример хранимого результата формы из нескольких вопросов `[эксперимент]`:
  ```json
  {"version":1,"answers":[
    {"questionId":"q_single","optionIds":[],"otherText":"свой вариант"},
    {"questionId":"q_multi","optionIds":["x","z"]},
    {"questionId":"q_text","optionIds":["paperclip_text_answer"],"otherText":"7"}],
   "summaryMarkdown":"ТЕСТ"}
  ```
- Повторный `respond` (то же тело или невалидное) → `409 interaction_already_resolved` `[эксперимент]`.
- Гонка: 5 параллельных `respond` с разными ответами → ровно `1×200`, `4×409`; сохранён ответ победителя `[эксперимент]`.
- Новая `ask_user_questions` в той же задаче вытесняет предыдущую pending: `status: expired`, `result: {"version":1,"answers":[],"expirationReason":"superseded_by_newer_interaction","supersededByInteractionId":"<id>","summaryMarkdown":null}`. `respond` на неё → `409` с `code: interaction_already_resolved`, **а не** `interaction_superseded` `[эксперимент]`.
- Агент может создать `ask_user_questions` только в задаче своего run (`422 sourceRunId must belong to the same issue`, `services/issue-thread-interactions.ts:3550-3562`) `[эксперимент]`. Для bridge это не важно: он карточки не создаёт.
- **Тестовые формы** (все закрыты, `resolverPolicy: anyone`, `continuationPolicy: none`, без адресата): форма из 4 вопросов (валидация, повтор), вытесненная форма, гонка, legacy `questions[]` (лимит 20 ответов).

### 6.2 `request_confirmation`
**Payload:** `{version:1, prompt (≤1000), acceptLabel?, rejectLabel?, rejectRequiresReason?, rejectReasonLabel?, allowDeclineReason?, declineReasonPlaceholder?, detailsMarkdown? (≤20000), supersedeOnUserComment?, target?{type: issue_document|custom, …}, toolAction?, secretProposal?}`.
- Accept: `POST …/accept` с `{}` → `200`, `status: accepted`, `result: {"version":1,"outcome":"accepted"}` `[эксперимент]`.
- Reject: `POST …/reject` с `{"reason":"…"}` → `200`, `status: rejected`, `result: {"version":1,"outcome":"rejected","reason":"…"}` `[эксперимент]`.
- `rejectRequiresReason: true` и пустой reason → `422 "A decline reason is required for this confirmation"` `[эксперимент]`.
- Карточка с `target.type = issue_document`, чья ревизия устарела, при ответе → `409 interaction_stale_target` (`services/issue-thread-interactions.ts:1862-1941`) `[код]`; в реальных данных есть такие карточки со `status: expired, outcome: stale_target` `[запрос]`.
- Governed (`toolAction`/`secretProposal`): принудительно `human_only`, отдельные правила ответа и ошибок — §6.6.

### 6.3 `request_checkbox_confirmation`
**Payload:** `{version:1, prompt, detailsMarkdown?, options[{id,label (≤120),description?}], defaultSelectedOptionIds?, minSelected?, maxSelected?, acceptLabel?, rejectLabel?, rejectRequiresReason?, …}`.
- Accept: `{"selectedOptionIds":["a","c"]}` → `200`, `result: {"version":1,"outcome":"accepted","selectedOptionIds":["a","c"]}` `[эксперимент]`. Если `selectedOptionIds` не передан, берётся `defaultSelectedOptionIds` `[код]`. Bridge всегда передаёт явный список.
- `422`: `Select no more than 2 checkbox confirmation option(s)`, `Select at least 1 …`, `Unknown checkbox confirmation optionId: zzz` `[эксперимент]`. Дубликаты в массиве → `400`.
- Reject — как у confirmation.

### 6.4 `request_item_verdicts`
**Payload:** `{version:1, prompt, detailsMarkdown?, items[{id,label,description?,previewMarkdown?,href?,attachmentId?}], verdicts?[approve|reject|defer] (default — все три), requireReasonOn?[...], reasonLabel?, allowBulkApprove? (default true), …}`.
- `POST …/verdicts` с `{"verdicts":[{"id":"i1","verdict":"approve"},{"id":"i2","verdict":"reject","reason":"…"}]}`.
- **Частичная отправка разрешена:** вердикт только по `i1` → `200`, `status` остаётся `pending` `[эксперимент]`. Когда вердикты есть у всех пунктов → `status: answered`, `result: {"version":1,"outcome":"resolved","complete":true,"items":[{"id","verdict","reason"?,"resolvedByAgentId|resolvedByUserId","resolvedByRunId","resolvedAt"}]}` `[эксперимент]`.
- Повторная отправка уже оценённых пунктов → `200` с текущей записью, значения не сравниваются `[код]`. Это единственный идемпотентный ответ.
- `422 "A reason is required when verdict is reject"` `[эксперимент]`; `Unknown item verdict id`, `Verdict X is not enabled` `[код]`.

### 6.5 Автоматический выход из `pending`
`[код]`, частично `[запрос]` по реальным данным:
| Событие | Статус / outcome | Ответ при попытке разрешить |
|---|---|---|
| Агент создал новую карточку того же kind (`request_confirmation` или `ask_user_questions`) в той же задаче | `expired`, `superseded_by_newer_request` (`ask_user_questions`: `expirationReason: superseded_by_newer_interaction` `[эксперимент]`) | `409 interaction_superseded`; для `ask_user_questions` — `409 interaction_already_resolved` `[эксперимент]` |
| Комментарий человека при `payload.supersedeOnUserComment: true` (комментарий с `authorUserId` и **без** `createdByRunId`, `origin` не `fast_response`; `services/issue-thread-interactions.ts:4340-4355`) | `expired`, `superseded_by_comment` (+`commentId`) | `409 interaction_superseded` |
| Задача `done`/`cancelled` | `expired`, `issue_closed` (в GET-списке сразу) `[эксперимент]` | `409 interaction_issue_closed` `[эксперимент]` |
| Ревизия документа-цели устарела | `expired`, `stale_target` | `409 interaction_stale_target` |
| Агент-адресат удалён | `cancelled`, `addressee_deleted` | `409` |
| Агент отозвал | `cancelled`, `withdrawn` `[эксперимент]` | `409 interaction_already_resolved` `[эксперимент]` |
| Пользователь нажал skip / cancel в UI | `cancelled`, `skipped` / `cancelled: true` | `409` |

**Комментарий агента карточку не закрывает** `[эксперимент]`: в песочнице агент оставил комментарий после карточки с `supersedeOnUserComment: true`, карточка осталась `pending` (у комментария `authorUserId: null`, есть `createdByRunId`). Комментарий человека закрывает её `[код]`; board-ключом это `[не проверено]`.

**Следствие для bridge:** board-ключ даёт актора `user` (`middleware/auth.ts`, `source: board_key`), а у запроса bridge нет run. Поэтому комментарий bridge сервер посчитает комментарием человека, и тот закроет карточки с `supersedeOnUserComment` `[код]`. Значит, **bridge комментарии не пишет** (в v1 их и нет в scope).

### 6.6 Governed-подтверждения: `toolAction` и `secretProposal`

**Как проверено.** Положительный путь (ответ человеком) — `[код]`: в компании нет ни одной governed-карточки, а создать её через API нельзя (см. ниже). `[запрос]` подтверждены: отказ в создании, `GET /api/issues/{issueId}/approvals`, `GET /api/approvals/{id}` (`404`). Эксперимент с отзывом через `withdraw` не понадобился: карточки не создавались.

**Что это.** Обычный kind `request_confirmation`, в `payload` которого сервер положил блок `toolAction` или `secretProposal`. Карточки создаёт **только сервер**:
- `toolAction` — tool gateway, когда агент вызывает write/destructive-инструмент подключённого сервиса (MCP/connector) и политика требует одобрения (`services/tool-gateway.ts:2593-2645`);
- `secretProposal` — сервис secret proposals, когда агент просит **привязать уже существующий секрет компании** к конфигурации агента (`services/secret-proposals.ts:173-225`).

Попытка передать эти блоки при создании `[запрос]`: `422 {"error":"payload.secretProposal is server-owned metadata and cannot be supplied when creating an interaction"}` (для `toolAction` — то же, `routes/issues.ts:16817-16834`). Тело, не прошедшее схему, — `400 Validation error` раньше этой проверки.

#### Payload `toolAction` (`validators/issue.ts:1583-1598`)
| Поле | Тип | Показывать в Telegram |
|---|---|---|
| `version` | `1` | — |
| `actionRequestId`, `invocationId` | uuid | нет (только в state) |
| `toolName` | ≤500, техническое имя | нет, вместо него `toolDisplayName` |
| `toolDisplayName` | ≤500 | **да** |
| `connectionId`, `applicationId` | uuid \| null | нет |
| `appDisplayName` | ≤500 \| null | **да** (имя сервиса) |
| `risk` | `read` \| `write` \| `destructive` | **да**, определяет сценарий |
| `rememberActionScope?` | ≤1000, текст «агенту можно и дальше…» | нет: «запомнить разрешение» из Telegram не предлагается |
| `previewMarkdown` | ≤20000, человекочитаемое описание действия, собирается из аргументов | **да**, экранированный и обрезанный (архитектура §8.3 j) |
| `argumentsSummaryJson` | ≤20000, JSON аргументов **после серверной редакции** чувствительных полей (`sensitiveMode: "redact"`) | по умолчанию **нет**, только по флагу `GOVERNED_SHOW_ARGUMENTS` |
| `argumentsHash` | ≤255 | нет; bridge сверяет его перед отправкой ответа |
| `expiresAt` | ISO, **через 1 час** после создания (`tool-gateway.ts:2482`) | **да**: «действует до HH:MM» |

Остальное в payload сервер задаёт сам: `prompt: "Approve <toolDisplayName>?"`, `acceptLabel: "Approve action"`, `rejectLabel: "Reject action"`, `rejectRequiresReason: false`, `allowDeclineReason: true`, `supersedeOnUserComment: false`, `target: {type:"custom", key:"tool-action:<actionRequestId>", revisionId:<argumentsHash>}`, `detailsMarkdown` (имя инструмента, риск и **полный JSON аргументов** в блоке кода). `detailsMarkdown` у governed-карточек bridge **не показывает**: в нём те же аргументы. Карточка: `resolverPolicy: human_only`, `continuationPolicy: wake_assignee`, `idempotencyKey: tool-action:<actionRequestId>`, адресата нет.

#### Payload `secretProposal` (`validators/issue.ts:1600-1609`)
| Поле | Тип | Показывать |
|---|---|---|
| `proposalId` | uuid | нет |
| `sourceSecretLabel` | ≤500, имя существующего секрета компании | **да** |
| `configPath` | ≤500, куда в конфиг агента он попадёт (например, `env.GITHUB_TOKEN`) | **да** |
| `targetAgentId` | uuid | нет |
| `targetAgentName` | ≤500 | **да** |
| `justification` | ≤20000, обоснование агента | **да**, экранированное, обрезка 1000 символов |
| `expiresAt` | ISO, **через 14 дней** (`secret-proposals.ts:28`) | **да** |

**Значения секрета в карточке нет и быть не может:** это предложение привязать уже существующий секрет. Шифротекст хранится только в `company_secret_proposals` и стирается при закрытии (`issue-thread-interactions.ts:1147-1182`). Bridge **не обращается** к endpoints секретов и proposals. Серверные поля: `prompt: "Bind secret <label> to <agent> as <configPath>?"`, `acceptLabel: "Create binding"`, `rejectLabel: "Reject"`, **`rejectRequiresReason: true`**, `rejectReasonLabel: "Why should this binding not be created?"`, `human_only`, `continuationPolicy: wake_assignee`, `effectiveResolverPolicySource: governed_action`.

#### Кто может ответить
- Сервер вычисляет `governedAction = true`, если в payload есть любой из двух блоков, и тогда разрешает только актора `user` (`services/issue-thread-interaction-resolution.ts:169-173,238-246`). Агент получает `403 interaction_governed_action_denied` `[код]`. Board key даёт актора `user` (§1), так что ответ bridge разрешён `[код]`, `[не проверено]` ключом.
- `toolAction`: в `commitToolActionReview` актор записывается как `input.actor.userId ?? "board"` (`services/tool-action-review.ts:22-36,136-150`) `[код]`.
- `secretProposal` (accept): дополнительно нужно право `agent_config:update` на целевого агента (`assertCanResolveProposal`). Владельцу или админу компании оно есть `[код]`.

#### Ответы

**Accept `toolAction`:** `POST /api/issues/{issueId}/interactions/{interactionId}/accept`, тело `{}`. Поле `rememberAction: true` создаёт постоянное правило доверия для агента (`tool-action-review.ts:153-165`): **bridge его никогда не отправляет**. Маршрут обходит обычный `acceptInteraction` и вызывает `toolGateway.approveActionRequest` (`routes/issues.ts:17005-17026`). Ответ `200` — текущая запись карточки (`GET` после действия):
```json
{"status":"accepted","result":{"version":1,"outcome":"accepted",
 "toolAction":{"version":1,"status":"executed","rememberedAction":false,
   "resultSummary":"Row 42 added","resultHref":null,"errorCode":null,"errorMessage":null,
   "updatedAt":"2026-11-15T12:00:00.000Z"}}}
```
- `result.toolAction.status`: `approved` → `executing` → `executed` | `failed` (или `expired`). **Выполнение может идти синхронно внутри запроса** (`executionOnApprove`, `tool-gateway.ts:9990-10045`), поэтому ответ бывает долгим. Если выполнение асинхронное, статус дописывается позже, и его видно в `GET /api/issues/{issueId}/interactions`.
- `errorMessage` и `resultSummary` приходят от внешнего сервиса. Bridge показывает только `errorCode` и обрезанный до 300 символов `resultSummary`, а `errorMessage` не пересылает.

**Reject `toolAction`:** `POST …/reject`, тело `{"reason":"…"}` (необязательно, ≤4000). Ответ `200`, `status: rejected`, `result: {"version":1,"outcome":"rejected","reason":"…"}`. Invocation получает `denied / action_declined` `[код]`.

**Accept `secretProposal`:** `POST …/accept`, тело `{}`. Сначала карточка переходит в `accepted` обычным путём, затем сервер создаёт привязку (`routes/issues.ts:17093-17175`). **Ответ `200` приходит, даже если привязка не создалась:**
- успех: `result.secretProposal: {"version":1,"status":"executed","updatedAt":"…"}`;
- ошибка: `result.secretProposal: {"version":1,"status":"failed","errorCode":"<код или http_403>"}`, и сервер сам пишет в задачу комментарий «Secret binding execution failed». Карточка остаётся `accepted`, и повторить нельзя.

  Bridge обязан читать `result.secretProposal.status`, а не только HTTP-код.

**Reject `secretProposal`:** `POST …/reject`, **причина обязательна**: без неё `422 "A decline reason is required for this confirmation"` (`issue-thread-interactions.ts:2445-2451`) `[код]`, то же правило, что в §6.2 `[эксперимент]`. Успех: `status: rejected`, `result: {"version":1,"outcome":"rejected","reason":"…","secretProposal":{"version":1,"status":"rejected"}}`.

#### Formal approval для `risk: destructive`
- Для каждого destructive-действия tool gateway создаёт **отдельный board approval** (`type: request_board_approval`) и связывает его с задачей (`tool-gateway.ts:862-864,2554-2590`). Payload approval: `{title, summary, recommendedAction, risks[], source:"tool_gateway", invocationId, actionRequestId, tool, risk, argumentsHash}`.
- Пока approval не `approved`, accept карточки не проходит: `approveActionRequest` бросает `formal_approval_required` (`tool-gateway.ts:9898-9915`).
- Как найти approval: `GET /api/issues/{issueId}/approvals` `[запрос: 200, []]` или `GET /api/companies/{companyId}/approvals?status=pending` `[запрос: 200, []]`, затем совпадение `payload.actionRequestId == toolAction.actionRequestId`. Payload проходит `redactEventPayload`. Что `actionRequestId` при этом сохраняется — `[код]`, на реальной записи `[не проверено]`.
- Одобрить: `POST /api/approvals/{approvalId}/approve`, тело `{"decisionNote":"…"}` (необязательно). Повтор того же решения → `200` (`applied: false`), противоположное решение → `422` (`lib/approval-records.ts:22-66`) `[код]`. Отклонить: `POST /api/approvals/{approvalId}/reject`.
- Порядок для bridge: approve approval → accept карточки. При отказе: reject карточки → reject approval, если он ещё `pending` (сам сервер approval не закрывает `[код]`).

#### Ошибки и идемпотентность (отличия от §7)
| Ситуация | Ответ сервера | Источник |
|---|---|---|
| Повторный accept после accept / approve (`approved`, `executing`, `executed`, `failed`) | **`200`**, текущая запись | `tool-action-review.ts:86-94`, `tool-gateway.ts:9845-9851` |
| Повторный reject после reject | **`200`** | там же |
| Reject после accept или accept после reject | `409 {"error":"This review has already been resolved"}` **без `code`** | `tool-action-review.ts:95` |
| Срок истёк | `409 "This review has expired"` без `code`, либо **`500`** (см. ниже) | `tool-action-review.ts:121`, `tool-gateway.ts:9939-9944` |
| Задача закрыта | `409 "This task is closed"` без `code` | `tool-action-review.ts:97-101` |
| `formal_approval_required`, `action_not_pending`, `action_request_invalidated`, `approved_tool_target_changed`, `action_task_closed`, `action_expired` (из `approveActionRequest`) | **`500 {"error":"Internal server error"}`**: `ToolGatewayHttpError` не наследует `HttpError`, а маршрут задачи его не переводит (`middleware/error-handler.ts:144-145,313-318`) | `[код]` |
| Карточка уже не `pending` (обычная проверка, `secretProposal`) | `409 interaction_already_resolved` и другие коды §7 | общий путь |
| Связанный proposal уже не `pending` | `409 "Linked secret proposal is no longer pending"` без `code` | `issue-thread-interactions.ts:1182` |
| Гонка двух accept | транзакция с `FOR UPDATE` по задаче, карточке и request: побеждает один, второй получает `200` (то же решение) или `409` (другое решение) | `tool-action-review.ts:51-85` |

**Жизненный цикл без ответа:** истечение и отмена request переводят pending-карточку в `expired` или `cancelled`, `result.outcome: stale_target`, `result.toolAction.status: expired` (`tool-gateway.ts:2245-2330`). У `secretProposal` фоновая очистка истёкших (`sweepExpired`) даёт `expired`. Новая обычная `request_confirmation` в той же задаче **не** вытесняет governed-карточку: она исключена из supersede (`issue-thread-interactions.ts:290-301`). `supersedeOnUserComment: false`, поэтому комментарии её тоже не закрывают `[код]`.

**Правило bridge для governed** (заменяет правило §7 для этих карточек):
1. **Перед** отправкой перечитать карточку. Если она не `pending`, истекла или изменились `toolAction.argumentsHash` / `target.revisionId` по сравнению с тем, что видел пользователь, — не отправлять, показать актуальное состояние.
2. **После любого не-`2xx`** (включая `409` без `code` и `500`) перечитать карточку и связанный approval, а решение принимать по состоянию, а не по коду:
   - карточка `accepted` или `rejected` с нашим решением и нашим `resolvedByUserId` — успех;
   - другая развязка — «решено в Paperclip»;
   - карточка `pending`, есть pending approval — шаг formal approval;
   - карточка `pending`, причина неясна — ограниченное число повторов (3), затем «Не удалось, ответьте в Paperclip».
3. Таймаут accept `toolAction` — отдельный, длиннее обычного: выполнение синхронное. Повтор после таймаута безопасен (тот же accept → `200`).
4. Статусы `approved` / `executing` bridge дочитывает в следующих циклах sync до `executed` / `failed` / `expired` (не дольше 30 минут).

## 7. Идемпотентность и гонки

`[эксперимент]` в песочнице, подтверждено кодом (`services/issue-thread-interactions.ts:793-821`, условные `UPDATE … WHERE status='pending' RETURNING`):

| Сценарий | Результат |
|---|---|
| Повторный `accept` после `accept` | `409 {"error":"Interaction has already been resolved","code":"interaction_already_resolved"}` |
| `reject` после `accept` | `409 interaction_already_resolved` |
| `accept` после `withdraw` | `409 interaction_already_resolved` |
| 5× `accept` + 1× `reject` параллельно | ровно `1×200`, `5×409`; итог `accepted` |
| Несуществующая карточка | `404 {"error":"Interaction not found","code":"interaction_not_found"}` |
| Повторное создание с тем же `idempotencyKey` (в т. ч. после разрешения) | `201` с той же записью; другой body с тем же ключом → `409` |
| Неверный kind для действия | `422` |
| Ответ agent-токеном на `human_only` | `403 {"code":"interaction_human_only"}` |

Сервер проверяет статус **до** валидации тела, поэтому на разрешённую карточку приходит `409`, даже если тело неверное. Исключение: native-runtime question sets валидируют тело раньше, и там возможен `422`.

**Правило bridge для 409:** любой `409` с `code` из `interaction_already_resolved | interaction_superseded | interaction_issue_closed | interaction_stale_target` — **постоянный исход**, не ошибка. Bridge перечитывает карточку (`GET /api/issues/{issueId}/interactions`) и по `status/result/resolvedBy*` решает, что написать в Telegram:
- если `resolvedByUserId` = владелец ключа и `result` совпадает с отправленным → «принято» (наш предыдущий запрос дошёл, ответ потерялся);
- иначе → «уже решено в Paperclip: …» без повторной отправки.
`409` без `code` (например, `workspace_sync_pending`) считается временным: retry с backoff. **Кроме governed-карточек:** там `409` без `code` и `500` означают конечный исход, и действует правило §6.6.

## 8. Задачи для `/status` и `/backlog`: `GET /api/companies/{companyId}/issues`

**Проверка:** `[запрос]` agent-токеном; board-ключом `[не проверено]`. По коду board видит все задачи компании, agent — с read-фильтром.

**Query** (`routes/issues.ts:8259-8497`, `services/issues.ts`):
| Параметр | Значения |
|---|---|
| `status` | через запятую: `backlog,todo,in_progress,in_review,blocked,done,cancelled` `[запрос]` |
| `view` | `compact` — облегчённая строка, с `ETag` `[запрос]` |
| `limit`, `offset` | default 500, max 1000 |
| `sortField`, `sortDir` | `updated` / `id`, `asc` / `desc` |
| `afterId` | keyset-курсор, только с `sortField=id&sortDir=asc` без `offset` |
| `updatedSince` | `updatedAt > since` |
| `assigneeAgentId`, `projectId`, `parentId`, `q`, `attention=blocked`, … | фильтры |

**ETag:** только `view=compact` отдаёт `ETag` + `Cache-Control: private, must-revalidate`; `If-None-Match` → `304` `[запрос]`.

**Поля compact, нужные bridge:** `id, identifier, title, status, priority, assigneeAgentId, assigneeUserId, projectId, parentId, updatedAt, lastActivityAt, blockerAttention, reviewAttention`. Имя агента — из `GET /api/companies/{companyId}/agents` (кешировать).

**Сценарии:**
- `/status`: `status=in_progress,in_review,blocked&view=compact` + счётчики pending-карточек из state + последние failed runs.
- `/backlog`: `status=todo,backlog&view=compact&limit=20`, сортировка по умолчанию (приоритет, затем активность).

## 9. Agent runs

### `GET /api/companies/{companyId}/heartbeat-runs` `[запрос]`
- **Query:** только `agentId`, `limit` (default 200, max 1000), `summary=1`. Порядок `createdAt DESC` `[запрос]`. Курсора/since нет.
- **Поля:** `id, agentId, issueId, status, error, errorCode, exitCode, signal, stderrExcerpt (может содержать чувствительное — не логировать и не пересылать), createdAt, startedAt, finishedAt, retryOfRunId, scheduledRetryAt, scheduledRetryAttempt, scheduledRetryReason, livenessState, livenessReason, contextSnapshot{issueId, taskId, taskKey, wakeReason, …}, invocationSource, triggerDetail, …`.
- **Статусы:** `queued, scheduled_retry, running, succeeded, interrupted, failed, cancelled, timed_out` (`packages/shared/src/constants.ts:918-927`).
- Реальный пример ошибки (не сбой агента, а системная отмена):
```json
{"status":"cancelled","error":"Cancelled because issue assignee changed before the queued run could start; the new owner will be woken instead","errorCode":"issue_assignee_changed","exitCode":null}
```

### `GET /api/heartbeat-runs/{runId}` `[запрос]`
Один run. Несуществующий → `404 {"error":"Heartbeat run not found"}`. Дополнительно содержит `retryExhaustedReason`.

### `GET /api/companies/{companyId}/live-runs` `[запрос]`
Только `queued`/`running` (`limit` ≤ 50). Для `/status`.

**Что считать сбоем для уведомления:** `failed` и `timed_out`. `cancelled` и `interrupted` — не уведомлять (обычно системные причины). Если в run стоит `scheduledRetryAt` (будет повтор), уведомление откладывается до финального исхода. Источник «финального» сбоя — элемент `failed_run` в ленте `attention` (после исчерпания retry) `[код]`.

**Опрос:** каждые `PAPERCLIP_RUNS_POLL_INTERVAL` (default 60 с) `?limit=100&summary=1`; новые `failed|timed_out` с `finishedAt` позже старта bridge и `id` не в state → уведомление. При первом запуске текущие сбои помечаются как «виденные» без уведомления.

## 10. Нагрузка опроса

Реальные объёмы тестовой компании на 2026-11-15: 7 задач, 31 run за двое суток `[запрос]`.

| Запрос | Интервал (default) | Запросов/мин | Ответ |
|---|---|---|---|
| `attention?limit=100` | 15 с | 4 | 1 запрос на всю компанию, вне зависимости от числа задач |
| `issues/{id}/interactions` | только для задач с новыми/изменившимися элементами ленты | ≈0 в покое | на задачу |
| `heartbeat-runs?limit=100&summary=1` | 60 с | 1 | — |
| `issues?view=compact` | только по команде `/status`/`/backlog` | — | ETag → 304 |

**Итого в покое ≈5 запросов/мин.** Обход задач по одной не нужен. Если лента `attention` недоступна (например, сервер вернул `403`), fallback не используется: такой ключ неправильный, bridge сообщает об этом и ждёт (см. архитектуру). Резервная стратегия на случай отказа от ленты (не реализуется в v1): `issues?status=todo,in_progress,in_review,blocked&view=compact&updatedSince=<last>` + `interactions` только у изменившихся задач. При 50 активных задачах и интервале 30 с это ≤ 2 + изменившиеся запросов/мин.

## 11. Не используется bridge, но важно знать

- `POST …/dismiss` (только человек) лишь откладывает карточку в историю: статус остаётся `pending`, ответом это не является.
- `POST …/skip`, `…/cancel` — board only, закрывают карточку без ответа.
- `POST …/resolve-from-comment` — запись разговорного ответа по комментарию.
- WebSocket `/api/companies/{companyId}/events/ws` даёт события `heartbeat.run.status`, но board key там, судя по коду, не принимается (`realtime/live-events-ws.ts:145-230`). Polling надёжнее и не требует долгого соединения.
- `GET /api/companies/{companyId}/activity`: действия `issue.thread_interaction_*`, без курсора, max 500. Для bridge не нужен.

## 12. Ограничения проверки

| Что | Почему | Как закрыть |
|---|---|---|
| Ответ board-ключом (`attention`, accept/respond на `human_only`) | у Architect нет board-ключа, выпуск не согласован | integration-тест на этапе DevOps/Developer с ключом пользователя на тестовой задаче или mock по этому документу; вопрос пользователю |
| `respond` на `allowOther` в legacy-форме без `questionSet` | лимит в 4 тестовые формы | `[код]`; mock-тест; реальные формы агентов приходят с `questionSet` |
| Supersede комментарием человека | нужен комментарий пользователя или board-ключ | проверено только отрицательное условие: комментарий агента не закрывает карточку `[эксперимент]`; положительное — `[код]` |
| Пагинация `attention` при >100 элементах | столько pending нет | mock-тест на курсор и `400` |
| Governed `toolAction` / `secretProposal`: ответ человеком, формат `result`, `500` из tool gateway | карточки создаёт только сервер (`422 server-owned`, `[запрос]`); в компании нет подключённых инструментов с одобрением и нет secret proposals | `[код]`; contract-тесты на mock по §6.6; ручная проверка на deployment с первым реальным tool action. Тестовые карточки не создавались, отзывать нечего |
| `payload.actionRequestId` в approval после `redactEventPayload` | нет реальных approvals | `[код]`; fallback — совпадение по `invocationId` или `argumentsHash` |

