# Runbook

Эксплуатация bridge в Docker Compose по образцу `deploy/compose.example.yml` (сервис `bridge`).

**Важно про образ.** Runtime-образ — distroless: в нём **нет shell, `sqlite3`, `tar`, `curl`, `ps`**. Внутри контейнера можно запускать только бинарь `/usr/local/bin/bridge` (`docker compose exec bridge /usr/local/bin/bridge <подкоманда>` или `docker compose run --rm bridge <подкоманда>`). Любые `docker exec … sh`, `… sqlite3` не работают. Работа с файлами состояния — через отдельный контейнер ([BACKUP_RESTORE.md](BACKUP_RESTORE.md)).

Команды ниже не прогонялись на реальном развёртывании (deployment в рамках v1 не выполнялся); поведение бинаря подтверждено кодом и тестами.

## 1. Статус и логи

```bash
docker compose ps                       # колонка STATUS: healthy / unhealthy
docker compose logs --tail=200 bridge   # логи в stderr, JSON по умолчанию (LOG_FORMAT)
docker compose exec bridge /usr/local/bin/bridge healthcheck   # "healthy" или "unhealthy: <причина>"
```

В самом чате: `/status` показывает число карточек, ожидающих ответа, задачи в работе, активных агентов, сбои за 24 часа и **длину очереди ответов** (`очередь ответов: N`).

## 2. Диагностика

| Симптом | Причина (по коду) | Что делать |
|---|---|---|
| Контейнер не стартует, в stderr `configuration error: <VAR> …`, exit 2 | неверная или отсутствующая переменная, пустой/нечитаемый файл секрета, схема proxy не из `http, https, socks5, socks5h` | исправить переменную; проверить `docker compose run --rm bridge check-config` |
| лог `bridge failed` с `error: cannot open state: state database failed the integrity check` | повреждён `bridge.db` | остановить bridge и восстановить из backup ([BACKUP_RESTORE.md](BACKUP_RESTORE.md)); автопочинки нет |
| лог `bridge failed` с `error: cannot open state: … schema version N is newer than supported` | том использовался более новой версией bridge | запустить образ той же или более новой версии; откат версии не поддерживается |
| `healthcheck`: `health file unreadable` или `health file is stale (Ns old)` | процесс не запущен или завис (файл обновляется раз в 15 с, предел 60 с) | смотреть логи, перезапустить |
| `unhealthy: Paperclip key rejected (401/403)` | ключ отозван, истёк или неверный | раздел 3 (ротация), в чате будет сообщение «🔑 Ключ Paperclip отклонён» |
| `unhealthy: no successful telegram call for Ns` | proxy/Telegram недоступны дольше `HEALTH_MAX_STALE` (5 мин) | проверить proxy и сеть контейнера; bridge продолжает ретраи |
| `unhealthy: no successful paperclip call for Ns` | Paperclip недоступен дольше `HEALTH_MAX_STALE` | проверить Paperclip и сеть. В чате после 3 подряд неудач приходит «⚠️ Paperclip недоступен с ЧЧ:ММ», после восстановления — «✅ Связь с Paperclip восстановлена» |
| В логах `getUpdates conflict: another bridge instance is running or a webhook is set for this bot` | второй экземпляр на этом боте или у бота установлен webhook (Telegram отвечает 409 на `getUpdates`) | остановить второй экземпляр; webhook bridge сам **не снимает**, удалите его средствами Bot API (`deleteWebhook`). Bridge повторяет попытки с паузой до 5 минут |
| В логах `telegram rejected the bot token` | токен бота неверен или отозван в BotFather | записать новый токен в файл секрета и перезапустить. Повтор раз в 5 минут |
| В логах `paperclip health check failed, continuing` при старте | `GET /api/health` не ответил при запуске | проверить `PAPERCLIP_URL` и сеть; bridge продолжает работу и ретраит |
| В логах `ignored update from outside the allowlist` (`reason`: `chat_mismatch`, `user_mismatch`, `not_private`) | кто-то пишет боту или неверно заданы `TELEGRAM_CHAT_ID`/`TELEGRAM_USER_ID` | если это вы: сверить ID. Чужим бот не отвечает. Запись не чаще раза в 10 секунд |
| Ответ нажат, но в Paperclip его нет; в чате «⏳ ответ … в очереди» | Paperclip недоступен или отвечает 5xx/429/409 без кода; ответ хранится в outbox и отправляется бессрочно | дождаться восстановления; длина очереди видна в `/status` |
| «❌ Paperclip не принял ответ…» | Paperclip ответил 400/422 | исправить ответ и отправить снова (текст ошибки сервера в сообщении) или ответить в Paperclip |
| «— Решено в Paperclip … ваш ответ не отправлен» | карточку успели решить в UI или агентом | ничего, ответ не нужен |
| «— Paperclip не разрешает этому ключу ответить на эту карточку, ответьте в UI» | 403 с audience-кодом (`interaction_human_only`, `interaction_addressee_mismatch` и др.): ключ принадлежит не тому пользователю или карточка не для него | ответить в UI; проверить, что ключ выпущен владельцем вопросов |
| Карточка пришла дважды | падение процесса между отправкой в Telegram и записью в SQLite (документированный компромисс) | ответить на любую, второй ответ невозможен |
| В логах `tasks did not stop within the grace period` или `outbox did not finish within the grace period` | остановка заняла весь `SHUTDOWN_GRACE` | запись outbox вернётся в очередь при старте; проверьте, что `stop_grace_period` > 2 × `SHUTDOWN_GRACE` (README) |
| Много карточек пришло сразу после первого старта | при первом запуске доставляется вся текущая лента `attention` | ожидаемо; при более 10 новых сначала приходит сводка |

## 3. Ротация и отзыв ключей

Board key **бессрочный** (решение пользователя) и даёт полный доступ пользователя. Секреты читаются из файлов только при старте, поэтому после замены файла нужен перезапуск. Ни ключа, ни токена бота нет в состоянии `/state` и в его backup.

### 3.1. Утечка или подозрение (потерян или взломан Telegram-аккаунт, сервер, backup, лог)

Сначала отзовите board key, любым из способов:

1. В интерфейсе Paperclip (раздел board API keys; точное расположение пунктов меню в репозитории не описано, не проверено).
2. По API: `DELETE /api/board-api-keys/{keyId}` (`docs/paperclip-api.md` §2).
3. С сервера, где запущен bridge, тем же ключом из `PAPERCLIP_TOKEN_FILE`:

   ```bash
   docker compose run --rm bridge revoke-paperclip-key
   # или в уже запущенном контейнере:
   docker compose exec bridge /usr/local/bin/bridge revoke-paperclip-key
   ```

   Подкоманда вызывает `POST /api/cli-auth/revoke-current`; ей нужны `PAPERCLIP_URL` и `PAPERCLIP_TOKEN_FILE`. Успех — exit 0 и сообщение `Paperclip key revoked…`; при ошибке — exit 1 и подсказка отозвать ключ в UI. Бинарь вызывается по пути `/usr/local/bin/bridge` (пути `/bridge` в образе нет).

После отзыва bridge получит 401, пришлёт «🔑 Ключ Paperclip отклонён (401/403)», `healthcheck` станет unhealthy, повторы раз в 5 минут. Это ожидаемо.

### 3.2. При компрометации Telegram дополнительно

1. В BotFather выполнить `/revoke` для бота: токен бота сразу перестаёт работать, канал к bridge закрыт.
2. Остановить контейнер: `docker compose stop bridge`.
3. Проверить в Paperclip activity за период: `issue.thread_interaction_accepted`, `issue.thread_interaction_rejected`, `approval.approved` (`docs/paperclip-api.md` §11) и сверить с логами bridge по `interaction_id`. В логах bridge решения из Telegram помечены сообщениями `decision from telegram queued` (`update_id`, `delivery_id`, `interaction_id`, `kind`, `risk`, `action`), `answer delivered to Paperclip`, `governed decision applied` (`outcome`). По Paperclip UI отличить решение «из Telegram» нельзя, только по этому журналу.
4. Завершить сессии в Telegram, включить/сменить облачный пароль (2FA).
5. Выпустить новый ключ и новый токен бота, обновить файлы секретов, запустить (3.3).

### 3.3. Плановая ротация (рекомендация: раз в 90 дней, срока у ключа нет)

1. Создать новый board key Paperclip (UI/CLI, `docs/paperclip-api.md` §2).
2. Записать его в файл секрета (`secrets/paperclip_token`), права `0600`/`0400`.
3. `docker compose restart bridge` (или `up -d --force-recreate`).
4. Убедиться: `docker compose exec bridge /usr/local/bin/bridge healthcheck` печатает `healthy`; для проверки ключа дождитесь одного цикла `sync` (15 с по умолчанию) и убедитесь, что нет сообщения об отклонённом ключе.
5. Отозвать старый ключ (способы 1–2 из 3.1; `revoke-paperclip-key` после смены файла отозвал бы уже **новый** ключ).

### 3.4. Ротация токена бота

В BotFather: `/revoke` (выдаёт новый токен) → записать в `secrets/telegram_bot_token` → `docker compose restart bridge`. Состояние (offset) сохраняется.

## 4. Остановка, обновление

```bash
docker compose stop bridge      # SIGTERM; ждёт до stop_grace_period (в примере 30s)
docker build -t paperclip-telegram-bridge:local .   # новая версия
docker compose up -d
```

Bridge при старте сам применяет миграции схемы; откат на более старую версию с уже мигрированной базой не поддерживается. Перед обновлением делайте backup ([BACKUP_RESTORE.md](BACKUP_RESTORE.md)). Незаконченные формы переживают рестарт: bridge сообщит в чате, какие формы не завершены, и продолжит с того же шага. Правило `stop_grace_period` > 2 × `SHUTDOWN_GRACE` — в README, раздел «Остановка».

## 5. Что известно только по документации, а не по реальному запуску

Работа на живом Paperclip с board-ключом, governed-карточки, поведение proxy Xray и потеря питания не проверялись (см. ограничения в README).
