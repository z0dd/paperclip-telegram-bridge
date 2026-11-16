# Доступ bridge к Paperclip API

Перечень построен по `src/paperclip/http.rs` (реальные вызовы клиента) и контракту [paperclip-api.md](paperclip-api.md) (требования сервера, раздел 2). Условные обозначения: колонка «Серверное требование» взята из контракта и выведена исследованием кода Paperclip; на живом сервере с board-ключом **не проверялась** (контракт, §12).

## Тип учётных данных

Bridge использует **board API key** (`pcp_board_…`) пользователя, который отвечает на вопросы. Agent-ключи и run JWT не подходят: они не отвечают на `human_only`-карточки и не видят ленту `attention` (контракт §2, ADR-0002).

У board key нет отдельных scopes: он несёт **полный доступ пользователя-владельца** и в нашем случае бессрочный. «Минимальные права» достигаются не настройкой ключа, а выбором пользователя и тем, что bridge вызывает только операции из таблицы ниже. Организационные меры (отдельный пользователь Paperclip только для bridge с членством в одной компании) — решение пользователя, см. контракт §2. Требования к владельцу ключа:

- это должен быть тот же человек, который отвечает на вопросы, иначе на карточках с `addresseeUserId` будет `403 interaction_addressee_mismatch`;
- роль в компании не ниже участника (не `viewer`), иначе нет права разрешать карточки;
- для `secretProposal` нужно ещё право `agent_config:update` на целевого агента (контракт §2).

Заголовок: `Authorization: Bearer <token>`. Заголовок `X-Paperclip-Run-Id` не отправляется. Подключение к Paperclip идёт без proxy.

## Вызываемые операции

Все пути относительно `PAPERCLIP_URL`, под префиксом `/api`.

| Операция | Метод и путь | Когда | Серверное требование (контракт §2) |
|---|---|---|---|
| Проверка доступности и версии | `GET /api/health` | при старте | без прав компании |
| Лента pending-решений | `GET /api/companies/{companyId}/attention?limit=100&sort=activity[&cursor=…]` | каждый `PAPERCLIP_POLL_INTERVAL`, по каждой компании | доступ к компании, board, userId |
| Карточки задачи | `GET /api/issues/{issueId}/interactions` | для задач с новыми/изменёнными карточками, для сверки открытых карточек, перед и после ответа | чтение задачи |
| Ответ на карточку | `POST /api/issues/{issueId}/interactions/{interactionId}/accept` | подтверждения, чекбоксы, governed-accept (тело `{}` или `{"selectedOptionIds":[…]}`; `rememberAction` не отправляется никогда) | board |
| | `…/reject` | отказ (`{"reason":…}` по желанию) | board |
| | `…/respond` | ответы на вопросы форм (`answers[]` с `questionId`, `optionIds`, `otherText`) | board |
| | `…/verdicts` | вердикты по пунктам (`verdicts[]` с `id`, `verdict`, `reason`) | board |
| Formal approval для `destructive` | `GET /api/issues/{issueId}/approvals` | только при `GOVERNED_DESTRUCTIVE_MODE=confirm` и риске `destructive` | board |
| | `POST /api/approvals/{approvalId}/approve` (тело `{"decisionNote":"Approved via Telegram bridge"}`) | то же | board |
| | `POST /api/approvals/{approvalId}/reject` (`{"decisionNote":"Rejected via Telegram bridge"}`) | после отказа от `destructive`-действия, если approval ещё `pending` | board |
| Задачи для `/status`, `/backlog`, номера задач в сообщениях | `GET /api/companies/{companyId}/issues?status=…&view=compact&limit=…` | по команде или при нехватке идентификатора | доступ к компании |
| Сбои агентов | `GET /api/companies/{companyId}/heartbeat-runs?limit=100&summary=1` | каждый `PAPERCLIP_RUNS_POLL_INTERVAL`, команда `/status` | доступ к компании + `company_scope:read` |
| Активные агенты | `GET /api/companies/{companyId}/live-runs?limit=50` | только команда `/status` | доступ к компании |
| Имена агентов | `GET /api/companies/{companyId}/agents` | для подписей карточек (с кэшем в памяти) | доступ к компании |
| Отзыв ключа | `POST /api/cli-auth/revoke-current` | только подкоманда `bridge revoke-paperclip-key` | отзывает ключ, которым сделан запрос |

Контракт упоминает также `GET /api/companies/{companyId}` и `GET /api/heartbeat-runs/{runId}`; клиент bridge их **не вызывает** (в `http.rs` их нет).

## Что bridge не вызывает

Создание и изменение задач, комментарии, `skip`, `cancel`, `dismiss`, `withdraw`, отмена runs, управление ключами (кроме `revoke-current` по явной команде), endpoints секретов и secret proposals (`/api/companies/{id}/secrets…`), approvals, не связанные с `toolAction` этой карточки. Bridge не пишет комментарии: комментарий board-ключом закрывает часть карточек (контракт §6.5).

## Обработка ответов сервера

См. таблицу в [ARCHITECTURE.md](ARCHITECTURE.md), раздел 4 («Ошибки Paperclip при отправке ответа»). Кратко: 401 и 403 без audience-кода означают «ключ отклонён» (одно сообщение пользователю, healthcheck unhealthy, повтор раз в 5 минут); 403 с audience-кодом закрывает конкретную карточку; 409 `interaction_*` ведёт к сверке по состоянию карточки; 429 учитывает `Retry-After`; 5xx и сетевые ошибки ретраятся бессрочно с backoff.

## Как проверить доступ после настройки

1. `docker compose run --rm bridge check-config` проверяет только конфигурацию и сборку клиентов, **сетевых вызовов не делает**.
2. При старте bridge вызывает `GET /api/health`; итог пишется в лог: `paperclip reachable` (с версией) или `paperclip health check failed, continuing`. Этот вызов не проверяет ключ.
3. Работоспособность ключа видно по первому циклу `sync` (лента `attention`): при 401/403 в Telegram придёт «Ключ Paperclip отклонён (401/403)», а `bridge healthcheck` вернёт unhealthy.

Где именно в UI Paperclip выпускается и отзывается ключ, в репозитории не описано (не проверено); API-способы — в контракте §2.
