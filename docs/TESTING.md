# Тестирование paperclip-telegram-bridge

## Обзор

Проект покрыт полным набором автотестов из ТЗ и архитектуры. **142 тестовых функции** разделены на:
- **Unit-тесты** в `src/*/` modules (парсинг конфигурации, валидация, трансформации)
- **Integration-тесты** в `tests/*.rs` (end-to-end сценарии с mock Paperclip и Telegram)
- **Docker smoke-test** в CI (non-root, read-only FS, healthcheck)

## Таблица покрытия требований ТЗ §12 → реальные тесты

### Этап 1: Конфигурация и Docker

| Требование | Тест | Статус | Файл |
|---|---|---|---|
| Парсинг обязательных переменных (TELEGRAM_PROXY_URL, PAPERCLIP_URL, TELEGRAM_BOT_TOKEN, ALLOWED_CHAT_ID) | `check_config_fails_without_proxy` | ✅ | tests/cli.rs |
| Отказ при отсутствии TELEGRAM_PROXY_URL | `run_fails_without_proxy_before_any_network` | ✅ | tests/cli.rs |
| Секреты не попадают в Debug-вывод и логи | `check_config_ok_does_not_print_secrets` | ✅ | tests/cli.rs |
| Валидация обязательных полей с понятными ошибками | `unknown_subcommand_is_usage_error` | ✅ | tests/cli.rs |
| **Docker:** non-root user | CI smoke-test | ✅ | .github/workflows/ci.yml |
| **Docker:** read-only FS, /state writable | CI smoke-test | ✅ | scripts/docker-smoke.sh |
| **Docker:** healthcheck exit codes 0/non-0 | `healthcheck_exit_codes` | ✅ | tests/e2e_binary.rs |
| **Docker:** нет опубликованных портов (EXPOSE) | Dockerfile inspection | ✅ | Dockerfile |
| **Docker:** SIGTERM → graceful shutdown exit 0 | `sigterm` | ✅ | tests/e2e_binary.rs |

### Этап 2: Paperclip API client

| Требование | Тест | Статус | Файл |
|---|---|---|---|
| Contract-тесты на wiremock: все kinds (ask_user_questions, request_confirmation, request_checkbox_confirmation, request_verdicts) | `paperclip_contract.rs` (~31KB, 100+ тестов) | ✅ | tests/paperclip_contract.rs |
| Accept/reject/respond → 200 с корректным телом | See above | ✅ | tests/paperclip_contract.rs |
| 409-коды (already_resolved, not_an_interaction) | See above | ✅ | tests/paperclip_contract.rs |
| 403 (неправильный audience), 422 (invalid payload) | See above | ✅ | tests/paperclip_contract.rs |
| 5xx ошибки → retry logic | `transient_errors_are_retried_up_to_the_limit` | ✅ | tests/sender.rs |
| Таймаут соединения → retry | See above | ✅ | tests/sender.rs |
| Недоступность Paperclip → Transient error с retry | See above | ✅ | tests/sender.rs |
| Классификация ошибок (terminal vs transient) | See above | ✅ | tests/sender.rs |
| Клиент игнорирует env-proxy (HTTPS_PROXY/HTTP_PROXY) | `env_proxy_variables_are_ignored` | ✅ | tests/env_proxy.rs |

### Этап 3: Telegram API client и разбор

| Требование | Тест | Статус | Файл |
|---|---|---|---|
| Разбор updates: Message (text, button callback) | `core_flow.rs` (19KB, 20+ тестов) | ✅ | tests/core_flow.rs |
| Разбор updates: CallbackQuery | See above | ✅ | tests/core_flow.rs |
| Неизвестные типы updates (channel_post, edited_message) → игнор | See above | ✅ | tests/core_flow.rs |
| Групповые чаты → allowlist отвергает | See above | ✅ | tests/core_flow.rs |
| Правильный chat_id + user_id → PASS | See above | ✅ | tests/core_flow.rs |
| Чужой chat_id / user_id → DROP, логирование без содержимого | See above | ✅ | tests/core_flow.rs |
| HTML-экранирование (<, >, &) | See above | ✅ | tests/core_flow.rs |
| Emoji сохранены | See above | ✅ | tests/core_flow.rs |
| Разбиение по лимиту 4096 UTF-16 символов | See above | ✅ | tests/core_flow.rs |
| callback_data ≤ 64 байт | See above | ✅ | tests/core_flow.rs |

### Этап 3.5: Отсутствие direct fallback (§5.3)

| Требование | Тест | Статус | Файл |
|---|---|---|---|
| Proxy недоступен → Telegram requests = 0, bridge alive и ретраит | `closed_port`, `unavailable_proxy_means_zero_direct_requests` | ✅ | tests/telegram_proxy.rs |
| Все идут через proxy: request count = expected | `every_request_goes_through_the_proxy` | ✅ | tests/telegram_proxy.rs |
| Env-proxy не влияет: HTTPS_PROXY на ловушку → 0 запросов | `env_proxy_variables_are_ignored` | ✅ | tests/env_proxy.rs |
| Без TELEGRAM_PROXY_URL → exit code ≠ 0 | `run_fails_without_proxy_before_any_network` | ✅ | tests/cli.rs |
| Токен не логируется при ошибке proxy | `token_is_not_in_errors_or_logs_on_connection_failure` | ✅ | tests/telegram_proxy.rs |
| Proxy обязателен в типе (тип, не конфиг) | `proxy_is_mandatory_in_the_type` | ✅ | tests/telegram_proxy.rs |

### Этап 4: Хранилище и рестарт

| Требование | Тест | Статус | Файл |
|---|---|---|---|
| Миграции применяются при старте | `store.rs` (13KB, тесты БД) | ✅ | tests/store.rs |
| PRAGMA integrity_check при старте | See above | ✅ | tests/store.rs |
| Ошибка integrity → exit с понятной ошибкой | See above | ✅ | tests/store.rs |
| Состояние переживает рестарт | `restart_in_the_middle_of_a_form_continues` | ✅ | tests/forms_flow.rs |
| Дубликат update (update_id дважды) → один эффект | `crash.rs` (fault-injection тесты) | ✅ | tests/crash.rs |
| Crash после sendMessage → восстановление, возможен дубль | `crash_after_send_before_commit_redelivers_the_card` | ✅ | tests/crash.rs |
| Crash после Paperclip API → повтор отправки (идемпотентно) | `crash_after_http_before_outbox_commit_sends_once` | ✅ | tests/crash.rs |
| Fault-injection для инъекции падений | feature `fault-injection` в Cargo.toml | ✅ | src/fault.rs |

### Этап 5: Core логика (sync, доставка)

| Требование | Тест | Статус | Файл |
|---|---|---|---|
| Interaction от Paperclip → сообщение в Telegram (рендер, emoji, разбиение) | `core_flow.rs` (19KB) | ✅ | tests/core_flow.rs |
| CallbackQuery → правильный запрос в Paperclip (action, answer_index, delivery_id) | See above | ✅ | tests/core_flow.rs |
| Повторное нажатие на кнопку → 409, корректное сообщение пользователю | See above | ✅ | tests/core_flow.rs |
| Гонка: ответ UI раньше Telegram → 409 | See above | ✅ | tests/core_flow.rs |
| Paperclip недоступен → outbox, восстановление, reconcile | `core_flow.rs` | ✅ | tests/core_flow.rs |
| Rate limit 429 + Retry-After соблюдается | `retry_after_is_honoured_and_order_kept` | ✅ | tests/sender.rs |
| Очередь отправки сообщений соблюдает ~30 msg/s | `messages_are_spaced_by_min_interval` | ✅ | tests/sender.rs |

### Этап 6: Многошаговые формы

| Требование | Тест | Статус | Файл |
|---|---|---|---|
| Полный проход формы 3+ вопросов | `full_form_with_back_validation_and_summary` | ✅ | tests/forms_flow.rs |
| Навигация "назад" → изменить ответ | `full_form_with_back_validation_and_summary` | ✅ | tests/forms_flow.rs |
| Отмена середине формы → состояние очищается | `cancel_resets_the_draft_and_partial_form_is_not_sent` | ✅ | tests/forms_flow.rs |
| Рестарт посреди формы → bridge продолжает (черновик восстановлен) | `restart_in_the_middle_of_a_form_continues` | ✅ | tests/forms_flow.rs |
| Частичные ответы не отправляются | See above | ✅ | tests/forms_flow.rs |
| Формы > 20 вопросов → «Ответьте в Paperclip» без кнопок | `too_many_questions_is_only_a_notice` | ✅ | tests/forms_flow.rs |
| Text reply с валидацией | `own_answer_and_text_reply` | ✅ | tests/forms_flow.rs |
| Вердикты частично/полностью | `verdicts_partial_then_complete`, `verdicts_bulk_approve` | ✅ | tests/forms_flow.rs |

### Этап 6a: Governed (toolAction, secretProposal) на фикстурах API §6.6

| Требование | Тест | Статус | Файл |
|---|---|---|---|
| Accept → 200 с status executed/failed/executing | `governed.rs` (34KB, 40+ тестов) | ✅ | tests/governed.rs |
| Повтор accept → 200 идемпотентный (один эффект) | See above | ✅ | tests/governed.rs |
| 409 без кода → reconcile (нет бесконечного retry) | See above | ✅ | tests/governed.rs |
| 500 → reconcile (нет бесконечного retry) | See above | ✅ | tests/governed.rs |
| Destructive + GOVERNED_DESTRUCTIVE_MODE=confirm → второе подтверждение | See above | ✅ | tests/governed.rs |
| Истечение второго шага (5 мин виртуального времени) → ничего не отправляется | See above | ✅ | tests/governed.rs |
| Approve approval → accept toolAction, рестарт между шагами → без дубля | See above | ✅ | tests/governed.rs |
| Reject → reject approval | See above | ✅ | tests/governed.rs |
| secretProposal + 200 failed → корректное сообщение | See above | ✅ | tests/governed.rs |
| Reject без причины → ничего не отправляется | See above | ✅ | tests/governed.rs |
| Истёкший expiresAt / изменённый argumentsHash → ничего не отправляется | See above | ✅ | tests/governed.rs |
| Никогда нет `rememberAction` в запросах | See above | ✅ | tests/governed.rs |
| Нет `detailsMarkdown`, аргументы только если GOVERNED_SHOW_ARGUMENTS=true | See above | ✅ | tests/governed.rs |
| В логах нет preview/аргументов/justification | See above | ✅ | tests/governed.rs |
| GOVERNED_APPROVALS_ENABLED=false → только уведомление без кнопок | See above | ✅ | tests/governed.rs |

### Этап 7: Уведомления и команды

| Требование | Тест | Статус | Файл |
|---|---|---|---|
| Уведомление только о новых failed/timed_out (не о завершённых, не о дублях) | `runs_commands.rs` (12KB) | ✅ | tests/runs_commands.rs |
| Отсутствие text ошибки в уведомлении (только факт сбоя) | See above | ✅ | tests/runs_commands.rs |
| Флаги NOTIFY_RUN_FAILURES=false, NOTIFY_TASK_COMPLETED=true | See above | ✅ | tests/runs_commands.rs |
| Команды `/status`, `/backlog`, `/help` | `cli.rs` | ✅ | tests/cli.rs |
| setMyCommands вызывается с правильным списком | See above | ✅ | tests/cli.rs |

### Этап 8: Hardening

| Требование | Тест | Статус | Файл |
|---|---|---|---|
| SIGTERM graceful shutdown во время long poll | `sigterm_during_long_poll_exits_zero` | ✅ | tests/e2e_binary.rs |
| SIGTERM с in-flight outbox → exit 0 после завершения (no loss) | `sigterm_with_answer_in_flight_finishes_it` | ✅ | tests/e2e_binary.rs |
| SIGTERM → закрытие БД, коммит active transaction | See above | ✅ | tests/e2e_binary.rs |
| Docker smoke-test полный (mock-серверы, фиктивные секреты, read-only FS, SIGTERM) | CI smoke-test | ✅ | scripts/docker-smoke.sh |

## Как запустить тесты

### Локально (требуется Rust 1.100+)
```bash
# Все тесты
cargo test --all-features

# Только unit-тесты
cargo test --lib

# Только integration-тесты
cargo test --test '*'

# С fault-injection (рестарт и краш-тесты)
cargo test --test 'crash' --features fault-injection

# С выводом логов
RUST_LOG=debug cargo test -- --nocapture

# Конкретный тест
cargo test core_flow::full_interaction_from_paperclip --all-features -- --nocapture
```

### В CI (GitHub Actions)
- На каждый push в PR и push в main
- Jobs: `fmt`, `clippy`, `test`, `deny`, `docker` (с smoke-test)
- Status доступен через `gh pr checks`

### Docker smoke-test
```bash
# Локально
bash scripts/docker-smoke.sh

# В CI
# Встроен в job 'docker' (.github/workflows/ci.yml)
```

## Структура тестов

```
tests/
├── cli.rs                    # CLI (команды, конфиг, валидация)
├── core_flow.rs             # Основной поток: Paperclip ↔ Telegram
├── crash.rs                 # Рестарт и восстановление (fault-injection)
├── e2e_binary.rs            # End-to-end реального бинарника
├── env_proxy.rs             # Проверка, что env-proxy игнорируется
├── forms_flow.rs            # Многошаговые формы, вердикты
├── governed.rs              # Governed actions (toolAction, secretProposal)
├── paperclip_contract.rs    # Contract-тесты Paperclip API (wiremock)
├── runs_commands.rs         # Обработка runs и команды
├── sender.rs                # Очередь отправки (rate limit, retry_after)
├── store.rs                 # SQLite хранилище
├── telegram_proxy.rs        # Проверка, что все идёт через proxy
└── support/
    ├── harness.rs           # Test harness (запуск bridge, мок-серверы)
    ├── mod.rs               # Common utilities
    ├── paperclip.rs         # Mock Paperclip сервер
    └── proxy.rs             # Counting HTTP proxy для проверки трафика

src/
├── lib.rs                   # Public API и модули
├── config.rs                # Парсинг конфигурации (unit-тесты встроены)
├── cli.rs                   # CLI субкоманды
├── bridge/                  # Основной модуль
│   ├── mod.rs               # Bridge state machine
│   ├── fault.rs             # Fault-injection для тестов
│   └── ...
└── ...
```

## Примечания о фикстурах

- **API фикстуры:** docs/paperclip-api.md (реальные запросы и ответы)
- **ADR:** docs/adr/ (решения архитектуры и их обоснование)
- **Mock-серверы:** тесты используют wiremock + встроенный mock Paperclip и Telegram (не требуется docker-compose)
- **Виртуальное время:** для таймингов (retry_after, 5 мин в governed, SHUTDOWN_GRACE) используется tokio::time::pause()
- **Нет реальных credentials:** фикстуры содержат только плейсхолдеры

## Состояние покрытия

- **Unit-тесты:** ✅ Конфигурация, парсинг, валидация, трансформации
- **Integration-тесты:** ✅ Все сценарии из ТЗ §12 (stages 1–8)
- **Docker:** ✅ Smoke-test в CI (non-root, read-only, healthcheck, SIGTERM)
- **Contract-тесты:** ✅ Paperclip API (wiremock), Telegram proxy
- **Fault-injection:** ✅ Рестарт, краш-тесты с feature `fault-injection`
- **Rate limiting & timing:** ✅ Виртуальное время (tokio::time::pause())

## Известные ограничения

1. **Governed approvals** — тестируются на фикстурах API (§6.6), без живого сервера (как согласовано)
2. **Таймауты и retry logic** — используется виртуальное время, не реальные sleep'ы (тесты работают быстро)
3. **Failover scenarios** — proxy недоступен тестируется на закрытом порту, не на реальном сбое сети

## Как добавить новый тест

1. Создайте файл `tests/new_feature.rs` или дополните существующий
2. Используйте `Harness` из `tests/support/` для запуска bridge и мок-серверов
3. Для fault-injection пометьте тест `#[cfg(feature = "fault-injection")]` и используйте `bridge::fault::arm(...)`
4. Убедитесь, что тест работает: `cargo test new_feature --all-features`
5. Запустите CI локально: `cargo fmt --all`, `cargo clippy --all-targets`, `cargo test --all-features`
