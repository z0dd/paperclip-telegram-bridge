# Runbook

Эксплуатация bridge в Docker Compose по образцу `deploy/compose.example.yml` или `deploy/compose.host-network.example.yml` (сервис `bridge`; различия — в разделе 6).

**Важно про образ.** Runtime-образ — distroless: в нём **нет shell, `sqlite3`, `tar`, `curl`, `ps`**. Внутри контейнера можно запускать только бинарь `/usr/local/bin/bridge` (`docker compose exec bridge /usr/local/bin/bridge <подкоманда>` или `docker compose run --rm bridge <подкоманда>`). Любые `docker exec … sh`, `… sqlite3` не работают. Работа с файлами состояния — через отдельный контейнер ([BACKUP_RESTORE.md](BACKUP_RESTORE.md)).

Команды ниже не прогонялись на реальном развёртывании (живой деплой не выполнялся); поведение бинаря подтверждено кодом и тестами.

## 1. Статус и логи

```bash
docker compose ps                       # колонка STATUS: healthy / unhealthy
docker compose logs --tail=200 bridge   # логи в stderr, JSON по умолчанию (LOG_FORMAT)
docker compose exec bridge /usr/local/bin/bridge healthcheck   # "healthy" или "unhealthy: <причина>"
```

Команд в чате v2 нет. Длина очереди в Telegram — поле `queue_depth` в `HEALTH_FILE`; при очереди больше 60 в логе WARN `telegram queue is long`.

## 2. Диагностика

| Симптом | Причина (по коду) | Что делать |
|---|---|---|
| Контейнер не стартует, в stderr `configuration error: <VAR> …`, exit 2 | неверная или отсутствующая переменная, пустой/нечитаемый файл секрета, схема proxy не из `http, https, socks5, socks5h` | исправить переменную; проверить `docker compose run --rm bridge check-config` |
| лог `bridge failed` с `error: cannot open state: state database failed the integrity check` | повреждён `bridge.db` | остановить bridge и восстановить из backup ([BACKUP_RESTORE.md](BACKUP_RESTORE.md)); автопочинки нет |
| лог `bridge failed` с `error: cannot open state: … schema version N is newer than supported` | том использовался более новой версией bridge | запустить образ той же или более новой версии; откат версии не поддерживается |
| `healthcheck`: `health file unreadable` или `health file is stale (Ns old)` | процесс не запущен или завис (файл обновляется раз в 15 с, предел 60 с) | смотреть логи, перезапустить |
| `unhealthy: Paperclip key rejected (401)` | ключ отозван, истёк или неверный | раздел 3 (ротация); в General приходит «🔑 Paperclip отклонил ключ бриджа (401)». Повтор раз в 5 минут |
| В логе ERROR `keys belong to different Paperclip users`, `healthcheck` пишет `Paperclip key rejected (401)`, хотя ключи не отзывались | в `PAPERCLIP_TOKEN_FILES` ключи разных пользователей: беседы второго владельца пропускаются, код выставляет `paperclip_auth_ok=false` (`core/discovery.rs: owner_matches`) | оставить ключи одного пользователя и перезапустить; публиковать в чужую беседу Paperclip всё равно не даёт (`403`) |
| `unhealthy: Agent Chat is disabled in Paperclip` | в Paperclip выключен experimental-флаг Agent Chat (`404 Agent Chat is disabled`) | включить Agent Chat; в General приходит одно сообщение, после включения bridge продолжит сам |
| `unhealthy: Telegram supergroup check failed …`, в логе ERROR `telegram supergroup check failed` с `reason` | `TELEGRAM_CHAT_ID` не супергруппа, темы выключены, бот не администратор или без права «Управление темами» | исправить настройку группы по `reason`; проверка повторяется раз в 5 минут, рестарт не нужен |
| `unhealthy: Telegram refuses messages to the supergroup (403)` | бота удалили из группы или лишили прав | вернуть бота администратором; отправка повторяется раз в 5 минут |
| `unhealthy: no successful telegram call for Ns` | proxy/Telegram недоступны дольше `HEALTH_MAX_STALE` (5 мин) | проверить proxy и сеть контейнера; bridge продолжает ретраи |
| `unhealthy: no successful paperclip call for Ns` | Paperclip недоступен дольше `HEALTH_MAX_STALE` | проверить Paperclip и сеть; сообщения из Telegram ждут в очереди |
| В логах `getUpdates conflict: another bridge instance is running or a webhook is set for this bot` | второй экземпляр на этом боте или у бота установлен webhook (Telegram отвечает 409 на `getUpdates`) | остановить второй экземпляр; webhook bridge сам **не снимает**, удалите его средствами Bot API (`deleteWebhook`). Bridge повторяет попытки с паузой до 5 минут |
| В логах `telegram rejected the bot token` | токен бота неверен или отозван в BotFather | записать новый токен в файл секрета и перезапустить. Повтор раз в 5 минут |
| В логах WARN `paperclip version differs from the tested one`, `paperclip version is missing` или `paperclip version is not recognized` | версия Paperclip отличается от проверенной, не отдана или не формата `YYYY.MMDD.N` | **запуск не блокируется**. API бесед мог измениться: проверьте обмен сообщениями в одной теме; проверенная версия — константа в `src/paperclip/version.rs` |
| Бот не стартует, `configuration error` про файл секрета, при этом файл существует | контейнер (UID 65532) не может прочитать файл: у файла владелец хост-пользователь и права `0400`/`0600`/`0640` (rootless Docker) | пересоздать файл по рецепту из раздела 3.5 или поставить `0644` в каталоге `0700` |
| `configuration error: PAPERCLIP_COMPANY_IDS is no longer supported in v2` или то же про `TELEGRAM_USER_ID` | конфигурация v1 | README, раздел «Переход с v1» |
| В логах WARN `variable of v1 is no longer used, remove it` | заданы `GOVERNED_*`, `NOTIFY_*` и т. п. | убрать переменную, на работу не влияет |
| В логах `paperclip health check failed, continuing` при старте | `GET /api/health` не ответил при запуске | проверить `PAPERCLIP_URL` и сеть; bridge продолжает работу и ретраит |
| В логах `ignored update from outside the allowlist` (`reason`: `chat_mismatch`, `user_mismatch`) | пишут из другого чата или пользователь не в `TELEGRAM_ALLOWED_USER_IDS` | если это вы: сверить `TELEGRAM_CHAT_ID` и `TELEGRAM_ALLOWED_USER_IDS`. Чужим бот не отвечает. Запись не чаще раза в 10 секунд |
| Бот отвечает «Пишите в тему нужного агента» / «Эта тема не связана с беседой» / «передаётся только текст» | сообщение в General, в тему без беседы (беседа удалена в Paperclip) или не текст | написать текстом в тему нужного агента; в Paperclip ничего не ушло |
| В теме «⏳ Paperclip недоступен, сообщение в очереди» | Paperclip недоступен или отвечает 5xx/429; сообщение хранится в очереди и отправляется бессрочно | дождаться восстановления |
| В теме «⏸ Беседа на паузе» | беседа в Paperclip на паузе (`409 Task is paused`) | возобновить беседу в Paperclip и отправить сообщение снова |
| В теме «❌ Paperclip не принял сообщение…» | `400`/`422`/`403`/`404` от Paperclip | по тексту ошибки; при «ключ бриджа принадлежит не владельцу беседы» ключ выпущен не тем пользователем |
| Тема беседы появилась заново, старая пропала | тему удалили в Telegram: bridge создаёт её заново (WARN `forum topic not found, recreating`) | ожидаемо |
| Ответ агента пришёл в тему дважды | падение процесса между отправкой в Telegram и записью в SQLite (документированный компромисс) | ничего |
| Новая беседа не получила тему сразу | темы создаются через общую очередь (20 вызовов в минуту на группу); при первом запуске с многими беседами — минуты | подождать; `queue_depth` в `HEALTH_FILE` |
| В логах `tasks did not stop within the grace period` или `pc_outbox did not finish within the grace period` | остановка заняла весь `SHUTDOWN_GRACE` | сообщение вернётся в очередь при старте и отправится с тем же `clientRequestId`; проверьте, что `stop_grace_period` > 2 × `SHUTDOWN_GRACE` (README) |

## 3. Ротация и отзыв ключей

Board key **бессрочный** (решение пользователя) и даёт полный доступ пользователя во всех его компаниях. Ключей может быть несколько (`PAPERCLIP_TOKEN_FILES`, все одного пользователя). Секреты читаются из файлов только при старте, поэтому после замены файла нужен перезапуск. Ни ключа, ни токена бота нет в состоянии `/state` и в его backup: в базе лежит только `key_ref` (номер ключа и 8 hex от `sha256`).

Telegram-аккаунт из `TELEGRAM_ALLOWED_USER_IDS` пишет агентам от имени владельца ключа во всех компаниях, а одобрять действия из Telegram в v2 нельзя (карточек нет). Что может атакующий в каждом случае — [THREAT_MODEL](THREAT_MODEL.md).

### 3.1. Утечка или подозрение (потерян или взломан Telegram-аккаунт, сервер, backup, лог)

Сначала отзовите board key (все ключи, если их несколько), любым из способов:

1. В интерфейсе Paperclip (раздел board API keys; точное расположение пунктов меню в репозитории не описано, не проверено).
2. По API: `DELETE /api/board-api-keys/{keyId}` (`docs/paperclip-api.md` §2).
3. С сервера, где запущен bridge, тем же ключом:

   ```bash
   docker compose run --rm bridge revoke-paperclip-key
   # или в уже запущенном контейнере:
   docker compose exec bridge /usr/local/bin/bridge revoke-paperclip-key
   ```

   Подкоманда вызывает `POST /api/cli-auth/revoke-current`. Ей нужны `PAPERCLIP_URL` и `PAPERCLIP_TOKEN_FILE` (читается только эта переменная, `PAPERCLIP_TOKEN_FILES` игнорируется). Успех — exit 0 и сообщение `Paperclip key revoked…`; при ошибке — exit 1 и подсказка отозвать ключ в UI. Бинарь вызывается по пути `/usr/local/bin/bridge` (пути `/bridge` в образе нет).

   **Если ключей несколько**, задайте путь к отзываемому ключу явно и повторите для каждого файла (имя файла — пример):

   ```bash
   docker compose run --rm -e PAPERCLIP_TOKEN_FILE=/run/secrets/pc_company_a bridge revoke-paperclip-key
   ```

   Эту форму на реальном развёртывании не проверяли: в compose-примере `PAPERCLIP_TOKEN_FILE` задан в `environment`, а с `PAPERCLIP_TOKEN_FILES` вы заменяете его своим файлом секрета.

После отзыва ключ получает `401`. Bridge пишет ERROR `paperclip rejected the board key (401)`, присылает в General одно сообщение «🔑 Paperclip отклонил ключ бриджа (401)…» на каждый отклонённый ключ, `healthcheck` становится unhealthy, повторы идут раз в 5 минут. Беседы компаний отозванного ключа перестают читаться, сообщения из Telegram в них остаются в очереди (`pending`). Это ожидаемо. Пока вы не заменили ключ, остановите контейнер: `docker compose stop bridge`.

### 3.2. При компрометации Telegram дополнительно

1. В BotFather выполнить `/revoke` для бота: токен бота сразу перестаёт работать, канал к bridge закрыт.
2. Остановить контейнер: `docker compose stop bridge`.
3. Убрать из группы посторонних. Если взломан аккаунт из allowlist, удалить его из `TELEGRAM_ALLOWED_USER_IDS`. Завершить сессии в Telegram, включить или сменить облачный пароль (2FA).
4. Проверить в Paperclip беседы с агентами за период компрометации: комментарии пользователя, которых вы не писали, и неожиданные `/new` (начало новой сессии агента). Источник комментария (UI или bridge) Paperclip не сохраняет. По логам bridge можно сопоставить время: `message queued for paperclip` (`update_id`, `issue_id`, `thread_id`), `message delivered to paperclip`, `message rejected by paperclip` (`class`). Telegram id автора в лог не пишется, кто из разрешённых пользователей написал, видно только в истории темы в Telegram. Комментарий пользователя снимает карточки с `supersedeOnUserComment`: проверьте, не «просрочены» ли нужные запросы подтверждения, и при необходимости создайте их заново.
5. Если токен бота утёк, но Paperclip не затронут, отзывать board key не нужно: с токеном бота нельзя писать в Paperclip (`from.id` выставляет Telegram, [THREAT_MODEL](THREAT_MODEL.md)).
6. Выпустить новый токен бота (и новый ключ, если он отзывался), обновить файлы секретов, запустить (3.3, 3.4).

### 3.3. Плановая ротация board key (рекомендация: раз в 90 дней, срока у ключа нет)

1. Создать новый board key Paperclip (UI/CLI, `docs/paperclip-api.md` §2). Ключ должен принадлежать тому же пользователю: ключ другого пользователя bridge не примет (беседы чужого владельца пропускаются, в логе ERROR `keys belong to different Paperclip users`).
2. Заменить файл секрета (`secrets/paperclip_token` или один из файлов `PAPERCLIP_TOKEN_FILES`): удалить старый, записать новый и выставить владельца и права по рецепту из раздела 3.5. Обычный `chmod 0600`/`0400` у файла хост-пользователя в rootless Docker не подходит.
3. `docker compose restart bridge` (или `up -d --force-recreate`).
4. Убедиться: `docker compose exec bridge /usr/local/bin/bridge healthcheck` печатает `healthy`; подождите один цикл обнаружения (`PAPERCLIP_POLL_INTERVAL`, 5 с) и убедитесь, что в General нет сообщения об отклонённом ключе, а в логе нет ERROR `paperclip rejected the board key`. Темы и беседы остаются прежними: связь хранится по `issue_id` и `topic_thread_id`, а не по ключу. Беседа находит свой ключ по `key_ref`: сначала по хешу, затем по номеру в списке, поэтому и перестановка файлов в `PAPERCLIP_TOKEN_FILES`, и замена содержимого файла на том же месте работают.
5. Отозвать старый ключ (способы 1–2 из 3.1; `revoke-paperclip-key` после смены файла отозвал бы уже **новый** ключ).

### 3.4. Ротация токена бота

В BotFather: `/revoke` (выдаёт новый токен) → записать в `secrets/telegram_bot_token` → `docker compose restart bridge`. Бот остаётся тем же, поэтому его членство и права администратора в супергруппе сохраняются, группу перенастраивать не нужно. Состояние сохраняется: связи «тема ↔ беседа», курсоры комментариев, offset Telegram и очереди. Если вы создали **нового** бота вместо перевыпуска токена, добавьте его в супергруппу администратором с правом «Управление темами» (`can_manage_topics`): при старте это проверяется (`telegram supergroup check failed`), темы остаются прежними.

### 3.5. Права файлов секретов (rootless Docker)

Контейнер работает с UID 65532. В rootless Docker он соответствует host-UID `subuid_start + 65531` (проверено в CI, job `rootless-secrets`), поэтому права файла, который принадлежит хост-пользователю, не работают так, как привычно. Каталог `secrets/` в `.gitignore`.

| Вариант | Файл | Читает контейнер | Читает хост-пользователь |
|---|---|---|---|
| **Рекомендуемый (строгий):** владелец 65532 (через контейнер), `0400`, каталог `0700` | `chown 65532:65532` + `chmod 0400` | да | нет |
| Допустимый: каталог `0700` у хост-пользователя, файл `0644` | `chmod 0644` | да | да |
| Не работает: `0400`, `0600`, `0640` у файла хост-пользователя | | нет | да |

Рецепт строгого варианта (он же при ротации: сначала удалить старый файл):

```bash
install -d -m 0700 secrets
( umask 077; printf '%s' '<telegram bot token>' > secrets/telegram_bot_token )
( umask 077; printf '%s' '<paperclip token>'     > secrets/paperclip_token )
docker run --rm --network none -v "$PWD/secrets:/s" busybox:1.37 \
  sh -c 'chown 65532:65532 /s/telegram_bot_token /s/paperclip_token && chmod 0400 /s/telegram_bot_token /s/paperclip_token'
```

После строгого варианта хост-пользователь не может прочитать файл, а root хоста может в любом варианте. Секреты с владельцем 65532 удаляются и пересоздаются, а не правятся на месте. Rootful Docker не проверялся. Содержимое секретов нигде не печатайте.

## 4. Остановка, обновление

```bash
docker compose stop bridge      # SIGTERM; ждёт до stop_grace_period (в примере 30s)
docker build -t paperclip-telegram-bridge:local .   # новая версия
docker compose up -d
```

При первом старте v2 миграция схемы 2 удаляет состояние карточек v1 (offset Telegram сохраняется); перед обновлением поменяйте конфигурацию по README, раздел «Переход с v1». Bridge при старте сам применяет миграции схемы; откат на более старую версию с уже мигрированной базой не поддерживается. Перед обновлением делайте backup ([BACKUP_RESTORE.md](BACKUP_RESTORE.md)). Правило `stop_grace_period` > 2 × `SHUTDOWN_GRACE` — в README, раздел «Остановка».

## 5. Что известно только по документации, а не по реальному запуску

Работа на живом Paperclip с board-ключом, тексты ошибок Telegram для удалённой и закрытой темы, поведение proxy Xray и потеря питания не проверялись (см. ограничения в README и `docs/paperclip-api.md` §8).

## 6. Развёртывание на хосте с Paperclip и Xray на loopback (`network_mode: host`)

Файл `deploy/compose.host-network.example.yml`: Paperclip на `127.0.0.1:3100`, Xray HTTP proxy на `127.0.0.1:10808` (значения-плейсхолдеры, подставьте свои). Шаги:

```bash
docker build -t paperclip-telegram-bridge:local .
cp deploy/compose.host-network.example.yml compose.yml   # проверить адреса PAPERCLIP_URL и TELEGRAM_PROXY_URL
cp .env.example .env                                     # TELEGRAM_CHAT_ID, TELEGRAM_ALLOWED_USER_IDS
# файлы секретов: раздел 3.5
docker compose run --rm bridge check-config
docker compose up -d
```

Контейнер не публикует портов и не слушает входящих соединений. Hardening тот же, что в изолированном примере (`read_only`, `65532:65532`, `cap_drop: [ALL]`, `no-new-privileges`, `tmpfs /tmp`, том `bridge-state`, `stop_grace_period: 30s`).

**Компромисс.** С `network_mode: host` у контейнера есть прямой маршрут в интернет и доступ ко всем loopback-сервисам хоста; изоляция сетями (`paperclip_internal`, `xray_egress`) не действует. В коде Telegram-клиент идёт только через `TELEGRAM_PROXY_URL` и не имеет обходного пути: недоступный proxy означает отсутствие запросов к Telegram, а не прямой выход. Ограничить исходящий трафик контейнера можно только firewall хоста, это вне поставки и не проверялось. Если proxy или Paperclip не отвечают, проверьте, что демон Docker видит loopback хоста: в rootless Docker это так при запуске rootlesskit с `--detach-netns` (Docker 29.10.0, проверено в CI); в старых установках `127.0.0.1` внутри контейнера не ведёт на хост. Живой деплой на сервер пользователя не проверялся.
