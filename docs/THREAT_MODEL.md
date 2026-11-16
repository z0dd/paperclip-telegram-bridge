# Threat model (STRIDE)

Основа — архитектурный набросок (раздел 10 архитектуры этапа 1), приведённый в соответствие с реализацией. Для каждой меры указано, где она в коде или тестах. Если мера не проверена, это написано.

## Активы

- Board API key Paperclip: полный доступ пользователя-владельца, **бессрочный** (решение пользователя, [ADR-0002](adr/0002-board-api-key.md)). Сузить его по правам нельзя (`paperclip-api.md` §2).
- Токен бота Telegram. Он входит в URL Bot API.
- Содержание вопросов и ответов, право подтверждать действия агентов (включая write/destructive-действия инструментов и привязку существующих секретов к агентам).
- Файл состояния `/state/bridge.db` (в нём тексты карточек, ответы в очереди, но не ключи).

## Границы доверия

Telegram (облако) и proxy — недоверенные; Paperclip — доверенный, но приватный; контейнер bridge — доверенный, минимально привилегированный: non-root `65532`, read-only rootfs, `cap_drop: [ALL]`, `no-new-privileges`, без портов, writable только `/state` (`Dockerfile`, `deploy/compose.example.yml`; проверка non-root, портов и read-only — шаги 1, 2, 4, 6–8 `scripts/docker-smoke.sh`).

## Угрозы

| Угроза | Вектор | Меры и где они реализованы |
|---|---|---|
| **S** — подделка пользователя | чужой аккаунт пишет боту или нажимает кнопку | allowlist по chat и user, только private-чат (`src/core/allowlist.rs`); `from.id` выставляет Telegram; чужие updates молча пропускаются, offset продвигается (`dialog.rs: handle_update`). Тесты: `tests/core_flow.rs::foreign_updates_are_ignored_silently`, unit-тесты в `allowlist.rs` |
| **S** — подделка callback | изменённый клиент шлёт свой `callback_data` | `callback_data` (`v1:<op>:<did>:<gen>…`) не содержит ID Paperclip, каждое нажатие проверяется по состоянию (доставка существует и открыта, `gen` текущий, индексы в пределах snapshot), иначе «Устарело» (`render/callback.rs`, `dialog.rs: on_callback`, ADR-0006). Окончательную авторизацию выполняет Paperclip |
| **S** — компрометация аккаунта Telegram владельца | кража сессии, SIM-swap, открытое чужое устройство | **Главный остаточный риск №2.** Атакующий делает всё, что владелец делает через bridge: отвечает на вопросы, одобряет write-действия инструментов, привязывает существующие секреты, при `GOVERNED_DESTRUCTIVE_MODE=confirm` — и destructive-действия (второй шаг защищает только от случайного нажатия). Не может: прочитать значение секрета, создать задачу или произвольное действие, «запомнить» разрешение (`rememberAction` не отправляется, `http.rs: governed_accept`), одобрить действие после срока или с изменёнными аргументами (сверка `argumentsHash`/`target.revisionId`, `governed.rs: changed`, повторно в outbox). Меры: 2FA и контроль сессий (обязательная рекомендация в README); `GOVERNED_APPROVALS_ENABLED=false` или `ui_only`; `/revoke` бота в BotFather, затем отзыв ключа и аудит ([RUNBOOK](RUNBOOK.md)); журнал решений (`decision from telegram queued`: `update_id`, `interaction_id`, `kind`, `risk`, `action`, без содержимого). PIN в чате не используется: он виден тому, кто читает чат |
| **T** — подмена трафика к Telegram | MITM | TLS (rustls) клиентом bridge до `api.telegram.org` сквозь proxy; proxy TLS не терминирует. Отдельная проверка сертификатов своим тестом не покрыта (не проверено) |
| **T** — подмена содержания governed-карточки между показом и решением | агент или сервер пересоздал действие с другими аргументами | перед отправкой bridge перечитывает карточку и сверяет `argumentsHash` и `target.revisionId` со snapshot (`governed.rs`); сервер дополнительно проверяет подпись аргументов (по контракту, `paperclip-api.md` §6.6; на живом сервере не проверялось) |
| **T** — порча состояния | диск, ручная правка | `integrity_check` при старте, версия схемы, отказ старта без автопочинки (`sqlite.rs`, `tests/store.rs::corrupt_database_is_refused`), backup ([BACKUP_RESTORE](BACKUP_RESTORE.md)) |
| **R** — «кто ответил» | ответы идут от имени владельца ключа | Paperclip фиксирует `resolvedByUserId` и activity (`paperclip-api.md` §6, §11); bridge логирует `interaction_id` и исход. По Paperclip UI отличить «из Telegram» нельзя, только по журналу bridge |
| **I** — утечка board key | логи, образ, окружение, backup, файл секрета | ключ только из файла `PAPERCLIP_TOKEN_FILE`, переменной с токеном нет; хранится как `SecretString` (`config.rs`), в `Debug` печатается `[REDACTED]` (`config.rs::secrets_are_redacted_in_debug`); не логируется; в состояние не пишется. **Ключ бессрочный**: украденный ключ действует до отзыва, главная мера — быстрый отзыв ([RUNBOOK](RUNBOOK.md)), рекомендация плановой ротации раз в 90 дней |
| **I** — утечка токена бота | логи (токен в URL), образ | ошибки reqwest сводятся к классу и никогда не форматируются с URL (`.without_url()`, `telegram/http.rs`, `paperclip/http.rs`); тест `tests/telegram_proxy.rs::token_is_not_in_errors_or_logs_on_connection_failure`. Утекший токен позволяет читать updates и писать в чат от имени бота, но не отвечать в Paperclip. Отзыв — `/revoke` в BotFather |
| **I** — учётные данные proxy | URL proxy может содержать логин и пароль | тип `ProxyUrl` хранит URL как секрет, печатает только схему (`config.rs`, `config.rs::proxy_debug_hides_credentials`) |
| **I** — содержание вопросов в Telegram | облако Telegram, proxy | принимаемый риск продукта. В логах содержания нет. Ссылки на UI по умолчанию не отправляются (`PAPERCLIP_UI_URL` пуст). Аргументы governed-действий по умолчанию не отправляются (`GOVERNED_SHOW_ARGUMENTS=false`) |
| **I** — приватные адреса | ссылки, сообщения об ошибках | `PAPERCLIP_UI_URL` пуст по умолчанию; в сообщениях о сбоях runs только статус и `errorCode` (`runs.rs`), без `error`/`stderr` |
| **D** — флуд updates | чужие сообщения | отказ до разбора, журнал не чаще 1 записи в 10 с с счётчиком подавленных (`allowlist.rs: LOG_INTERVAL`) |
| **D** — недоступность proxy или Paperclip | сбой сети | backoff, очередь ответов в SQLite, прямого пути к Telegram нет (`tests/telegram_proxy.rs::unavailable_proxy_means_zero_direct_requests`) |
| **D** — рассинхронизация при двух экземплярах | два bridge на один бот | Telegram возвращает 409 на `getUpdates`; bridge пишет ошибку и ждёт до 5 минут (`poller.rs`). Защиты от двух экземпляров на одном томе нет (не проверено) |
| **E** — эскалация через bridge | bridge выполняет только ответы на карточки и связанный formal approval | нет произвольных вызовов API; неподдерживаемые виды карточек не отвечаются; approvals, не связанные с `toolAction` этой карточки, не трогаются; перечень вызовов — [PAPERCLIP_PERMISSIONS](PAPERCLIP_PERMISSIONS.md) |
| **E** — побег из контейнера | — | non-root, read-only rootfs, `cap_drop: [ALL]`, `no-new-privileges`, без портов (в compose-примере; в smoke-тесте проверяются non-root, read-only, отсутствие портов) |
| Цепочка поставки | зависимости, базовые образы | `Cargo.lock` в репозитории, `cargo deny` (advisories, licenses, sources; job `deny` в CI), базовые образы закреплены по digest (`Dockerfile`). Автоматического обновления зависимостей (Dependabot/Renovate) в репозитории нет (не проверено) |

## Остаточные риски

1. **Бессрочный board key с полным доступом пользователя.** При компрометации контейнера атакующий получает всё, что может пользователь Paperclip, до отзыва ключа. Принято пользователем.
2. **Захват Telegram-аккаунта** позволяет одобрять действия инструментов и привязку секретов (строка S выше). Принято пользователем; ограничивается флагами `GOVERNED_*`, 2FA и runbook.
3. **Governed-ветка не проверена на живом Paperclip** (README, «Известные ограничения»). До первой реальной проверки поведение подтверждено только contract-тестами.
4. **Состояние на диске не шифруется**: `bridge.db` содержит тексты карточек; защита — права на том и хост.
5. Содержание вопросов проходит через Telegram.
