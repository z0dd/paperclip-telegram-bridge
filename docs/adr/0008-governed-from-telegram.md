# ADR-0008. Governed-подтверждения из Telegram

- Статус: заменено [ADR-0010](0010-v2-forum-bridge.md) (v2.0.0). В v2 карточек и governed-подтверждений в Telegram нет, переменные `GOVERNED_*` игнорируются с WARN. Текст ниже сохранён как история v1. Исходный статус: принято (решение пользователя: Approve/Reject из Telegram разрешены, `GOVERNED_DESTRUCTIVE_MODE=confirm` по умолчанию)
- Контекст: [docs/paperclip-api.md](../paperclip-api.md) §6.6

## Контекст

Карточки `request_confirmation` с блоком `toolAction` (действие инструмента подключённого сервиса) или `secretProposal` (привязка существующего секрета к агенту) создаёт сервер. Повтор того же решения сервер возвращает с `200`, а конечные состояния tool gateway — как `409` без кода или `500`.

## Решение

- Своё отображение: в Telegram уходят только `toolDisplayName`, `appDisplayName`, `risk`, имя агента, задача, срок и `previewMarkdown` (≤ 1500). Аргументы — только при `GOVERNED_SHOW_ARGUMENTS=true` и в версии, отредактированной сервером. `detailsMarkdown`, `toolName`, ID connection/application, `rememberActionScope` не отправляются. Карточка всегда в одном сообщении.
- `rememberAction` никогда не отправляется: постоянных правил доверия из Telegram нет.
- Перед отправкой решения карточка перечитывается (при нажатии и в outbox перед запросом). Не `pending`, истёк срок или изменились `argumentsHash`/`target.revisionId` — решение не отправляется.
- write/read: accept, затем дочитывание `result.toolAction.status` до `executed`/`failed`/`expired`, не дольше `GOVERNED_RESULT_FOLLOW`.
- destructive при `confirm`: второй шаг в том же сообщении (живёт 2 минуты), затем approve связанного formal approval (по `actionRequestId`, fallback — `invocationId`/`argumentsHash`, только `source=tool_gateway`) и accept карточки. Оба шага идемпотентны и повторяются после рестарта. Approval не найден — «решите в Paperclip».
- destructive при `ui_only`: карточка без кнопок; write по-прежнему из Telegram. Режим переключается переменной окружения и рестартом.
- Отказ: reject карточки, затем reject связанного approval, если он `pending`. Для `secretProposal` причина обязательна.
- После любого не-2xx решение принимается по перечитанному состоянию: наше — успех, чужое — «решено в Paperclip», неясно — не больше 3 попыток, затем «ответьте в Paperclip».
- `GOVERNED_APPROVALS_ENABLED=false` — аварийный выключатель: governed-карточки приходят только уведомлением.
- Журнал решений в логе `info`: `update_id`, `interaction_id`, kind, risk, действие — без содержимого.

## Последствия

- Захват Telegram-аккаунта позволяет одобрять действия инструментов и привязку секретов (остаточный риск №2, принят пользователем; ограничивается флагами и runbook).
- Поведение проверено contract-тестами на фикстурах API §6.6 (`tests/governed.rs`). На живом сервере governed-карточки не проверялись: нужна ручная проверка при deployment.
