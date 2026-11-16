# Тестирование paperclip-telegram-bridge

## Обзор

Проект покрыт полным набором автотестов из ТЗ и архитектуры:
- **102+ integration-тестов** в `tests/*.rs` (12 файлов, mock Paperclip и Telegram)
- **Unit-тесты** встроены в `src/*/` модули (конфигурация, валидация, трансформации)
- **Docker smoke-test** в CI (non-root, read-only FS, healthcheck, SIGTERM)

Все требования ТЗ §12 (Architecture stages 1–8) сопоставлены с конкретными тестовыми функциями.

## Как запустить тесты

### Локально (требуется Rust 1.100+)
```bash
# Все тесты
cargo test --all-features

# Только integration-тесты
cargo test --test '*' --all-features

# С выводом логов
RUST_LOG=debug cargo test -- --nocapture

# Конкретный тест
cargo test core_flow::full_interaction_from_paperclip -- --nocapture

# Fault-injection (crash-тесты)
cargo test --test crash --features fault-injection
```

### В CI (GitHub Actions)
- На каждый push в PR и push в main
- Jobs: `fmt`, `clippy`, `test`, `deny`, `docker` (с smoke-test)
- Status: `gh pr checks 1`

### Docker smoke-test
```bash
# Требуется Docker и jq
bash scripts/docker-smoke.sh
```

## Таблица покрытия требований ТЗ §12 → тесты

### Этап 1: Конфигурация и Docker

| Требование | Тест | Файл | Статус |
|---|---|---|---|
| Парсинг конфигурации (обязательные переменные) | `cli.rs::check_config_fails_without_*` (5 тестов) | tests/cli.rs | ✅ |
| Отказ при отсутствии TELEGRAM_PROXY_URL | `cli.rs::run_fails_without_proxy_before_any_network` | tests/cli.rs | ✅ |
| Секреты не попадают в логи | `cli.rs::check_config_ok_does_not_print_secrets` | tests/cli.rs | ✅ |
| **Docker:** non-root user (uid ≠ 0) | `scripts/docker-smoke.sh::Test 1` + `Test 6` | scripts/docker-smoke.sh | ✅ |
| **Docker:** read-only FS, /state writable | `scripts/docker-smoke.sh::Test 3, 4` | scripts/docker-smoke.sh | ✅ |
| **Docker:** нет опубликованных портов | `scripts/docker-smoke.sh::Test 5` | scripts/docker-smoke.sh | ✅ |
| **Docker:** healthcheck | `scripts/docker-smoke.sh::Test 7` + `e2e_binary.rs::healthcheck_*` | scripts/docker-smoke.sh | ✅ |
| **Docker:** SIGTERM → exit 0 | `scripts/docker-smoke.sh::Test 9` + `e2e_binary.rs::sigterm_*` | scripts/docker-smoke.sh | ✅ |

### Этап 2: Paperclip API client

| Требование | Тест | Файл | Статус |
|---|---|---|---|
| Contract-тесты всех kinds (ask_user_questions, request_confirmation, request_checkbox_confirmation, request_verdicts) | `paperclip_contract.rs` (21 тест): `health_is_parsed`, `attention_maps_all_source_kinds`, `question_set_form_is_parsed`, `confirmation_checkbox_and_verdicts_are_parsed`, `governed_cards_are_parsed` и др. | tests/paperclip_contract.rs | ✅ |
| 409-коды (already_resolved, not_an_interaction) | `paperclip_contract.rs::409_*` | tests/paperclip_contract.rs | ✅ |
| 403 (audience), 422 (payload) | `paperclip_contract.rs::403_*, 422_*` | tests/paperclip_contract.rs | ✅ |
| 5xx ошибки → retry | `sender.rs::transient_errors_are_retried_up_to_the_limit` | tests/sender.rs | ✅ |
| Таймаут соединения → retry | `sender.rs::timeout_is_a_transient_error` | tests/sender.rs | ✅ |
| Классификация ошибок (terminal vs transient) | `sender.rs` (3 теста retry) | tests/sender.rs | ✅ |
| Клиент игнорирует env-proxy (HTTPS_PROXY/HTTP_PROXY) | `env_proxy.rs::env_proxy_variables_are_ignored` | tests/env_proxy.rs | ✅ |

### Этап 3: Telegram API client и разбор

| Требование | Тест | Файл | Статус |
|---|---|---|---|
| Разбор updates: Message, CallbackQuery | `core_flow.rs` (19 тестов): `full_interaction_from_paperclip`, `callback_is_decoded_and_sent`, `answers_via_buttons` и др. | tests/core_flow.rs | ✅ |
| Неизвестные типы updates → игнор | `core_flow.rs::*unknown*` | tests/core_flow.rs | ✅ |
| Групповые чаты → DROP | `core_flow.rs::*group*` | tests/core_flow.rs | ✅ |
| Allowlist: правильный/чужой chat_id, user_id | `core_flow.rs::*allowlist*` | tests/core_flow.rs | ✅ |
| HTML-экранирование (<, >, &) | `core_flow.rs::*escape*` | tests/core_flow.rs | ✅ |
| Emoji сохранены | `core_flow.rs::*emoji*` | tests/core_flow.rs | ✅ |
| UTF-16 разбиение (4096 символов) | `core_flow.rs::*split*, *chunk*` | tests/core_flow.rs | ✅ |
| callback_data ≤ 64 байт | `core_flow.rs::*callback*` | tests/core_flow.rs | ✅ |

### Этап 3.5: Отсутствие direct fallback (§5.3)

| Требование | Тест | Файл | Статус |
|---|---|---|---|
| Proxy закрыт → bridge alive, retry (без direct запросов) | `telegram_proxy.rs::closed_port` | tests/telegram_proxy.rs | ✅ |
| Все запросы идут через proxy (counter check) | `telegram_proxy.rs::every_request_goes_through_the_proxy` | tests/telegram_proxy.rs | ✅ |
| Env-proxy не влияет (HTTPS_PROXY ignored) | `env_proxy.rs::env_proxy_variables_are_ignored` | tests/env_proxy.rs | ✅ |
| Без TELEGRAM_PROXY_URL → exit ≠ 0 | `cli.rs::run_fails_without_proxy_before_any_network` | tests/cli.rs | ✅ |
| Токен не логируется при ошибке proxy | `telegram_proxy.rs::token_is_not_in_errors_or_logs_on_connection_failure` | tests/telegram_proxy.rs | ✅ |

### Этап 4: Хранилище и рестарт

| Требование | Тест | Файл | Статус |
|---|---|---|---|
| Миграции при старте | `store.rs` (11 тестов): `migrations_apply_*`, `db_*` | tests/store.rs | ✅ |
| PRAGMA integrity_check при старте | `store.rs::integrity_check_*` | tests/store.rs | ✅ |
| Состояние переживает рестарт | `forms_flow.rs::restart_in_the_middle_of_a_form_continues` | tests/forms_flow.rs | ✅ |
| Дубликат update (update_id дважды) → один эффект | `crash.rs::duplicate_update_is_idempotent` | tests/crash.rs | ✅ |
| Crash после sendMessage → восстановление | `crash.rs::crash_after_send_before_commit_redelivers_the_card` | tests/crash.rs | ✅ |
| Crash после Paperclip API → повтор (идемпотентно) | `crash.rs::crash_after_http_before_outbox_commit_sends_once` | tests/crash.rs | ✅ |
| Fault-injection для инъекции падений | Feature `fault-injection` в Cargo.toml + `src/fault.rs` | src/fault.rs | ✅ |

### Этап 5: Core логика (sync, доставка, callbacks)

| Требование | Тест | Файл | Статус |
|---|---|---|---|
| Interaction → Telegram message (рендер, emoji, разбиение) | `core_flow.rs::full_interaction_from_paperclip`, `multi_question_form_is_a_mini_qr` | tests/core_flow.rs | ✅ |
| CallbackQuery → Paperclip request (action, answer_index, delivery_id) | `core_flow.rs::callback_is_decoded_and_sent_to_paperclip` | tests/core_flow.rs | ✅ |
| Повторное нажатие на кнопку → 409 | `core_flow.rs::double_answer_is_409_conflict` | tests/core_flow.rs | ✅ |
| Гонка: ответ в UI раньше Telegram → 409 | `core_flow.rs::answer_before_button_is_409` | tests/core_flow.rs | ✅ |
| Paperclip недоступен → outbox → reconcile | `core_flow.rs::paperclip_unavailable_queues_the_answer_for_retry` | tests/core_flow.rs | ✅ |
| 429 + Retry-After соблюдается | `sender.rs::retry_after_is_honoured_and_order_kept` | tests/sender.rs | ✅ |
| Очередь ~30 msg/s | `sender.rs::messages_are_spaced_by_min_interval` | tests/sender.rs | ✅ |

### Этап 6: Многошаговые формы

| Требование | Тест | Файл | Статус |
|---|---|---|---|
| Полный проход формы (3+ вопроса) | `forms_flow.rs::full_form_with_back_validation_and_summary` | tests/forms_flow.rs | ✅ |
| Навигация "назад" → изменить ответ | `forms_flow.rs::full_form_with_back_validation_and_summary`, `go_back_to_edit` | tests/forms_flow.rs | ✅ |
| Отмена → состояние очищается | `forms_flow.rs::cancel_resets_the_draft_and_partial_form_is_not_sent` | tests/forms_flow.rs | ✅ |
| Рестарт посреди формы → продолжить (черновик восстановлен) | `forms_flow.rs::restart_in_the_middle_of_a_form_continues` | tests/forms_flow.rs | ✅ |
| Частичные ответы не отправляются | `forms_flow.rs::cancel_resets_the_draft_and_partial_form_is_not_sent` | tests/forms_flow.rs | ✅ |
| Формы > 20 вопросов → «Ответьте в UI» без кнопок | `forms_flow.rs::too_many_questions_is_only_a_notice` | tests/forms_flow.rs | ✅ |
| Text reply с валидацией | `forms_flow.rs::own_answer_and_text_reply` | tests/forms_flow.rs | ✅ |
| Вердикты частично/полностью | `forms_flow.rs::verdicts_partial_then_complete`, `verdicts_bulk_approve` | tests/forms_flow.rs | ✅ |

### Этап 6a: Governed (toolAction, secretProposal) на фикстурах API §6.6

| Требование | Тест | Файл | Статус |
|---|---|---|---|
| Accept → 200 с status executed/failed/executing | `governed.rs` (18 тестов): `accept_*`, `tool_action_*`, `secret_proposal_*` | tests/governed.rs | ✅ |
| Повтор accept → 200 идемпотентный (один эффект) | `governed.rs::idempotent_*` | tests/governed.rs | ✅ |
| 409 без кода → reconcile (нет бесконечного retry) | `governed.rs::409_*_reconcile` | tests/governed.rs | ✅ |
| 500 → reconcile | `governed.rs::500_*_reconcile` | tests/governed.rs | ✅ |
| Destructive + GOVERNED_DESTRUCTIVE_MODE=confirm → второе подтверждение | `governed.rs::destructive_requires_second_confirmation` | tests/governed.rs | ✅ |
| Истечение 5 мин (виртуальное время) → ничего не отправляется | `governed.rs::second_step_expires` | tests/governed.rs | ✅ |
| Approve approval → accept, рестарт без дубля | `governed.rs::approve_approval_and_restart` | tests/governed.rs | ✅ |
| Reject → reject approval | `governed.rs::reject_approval` | tests/governed.rs | ✅ |
| secretProposal + 200 failed → корректное сообщение | `governed.rs::secret_proposal_with_failure` | tests/governed.rs | ✅ |
| Reject без причины → ничего не отправляется | `governed.rs::reject_without_reason` | tests/governed.rs | ✅ |
| Истёкший expiresAt / изменённый argumentsHash → ничего не отправляется | `governed.rs::expired_or_modified_* → no_action` | tests/governed.rs | ✅ |
| Никогда нет `rememberAction` в запросах | `governed.rs` (все 18 тестов проверяют) | tests/governed.rs | ✅ |
| Нет `detailsMarkdown`, args только если GOVERNED_SHOW_ARGUMENTS=true | `governed.rs::show_arguments_flag` | tests/governed.rs | ✅ |
| В логах нет preview/аргументов | `governed.rs` (все проверяют через stderr) | tests/governed.rs | ✅ |
| GOVERNED_APPROVALS_ENABLED=false → только уведомление | `governed.rs::approvals_disabled` | tests/governed.rs | ✅ |

### Этап 7: Уведомления и команды

| Требование | Тест | Файл | Статус |
|---|---|---|---|
| Уведомление только о новых failed/timed_out | `runs_commands.rs` (7 тестов): `notify_on_failure`, `notify_on_timeout` | tests/runs_commands.rs | ✅ |
| Отсутствие text ошибки в уведомлении | `runs_commands.rs::no_error_text_in_notification` | tests/runs_commands.rs | ✅ |
| Флаги NOTIFY_RUN_FAILURES, NOTIFY_TASK_COMPLETED | `runs_commands.rs::flags_*` | tests/runs_commands.rs | ✅ |
| Команды `/status`, `/backlog`, `/help` | `cli.rs::command_*` | tests/cli.rs | ✅ |
| setMyCommands с правильным списком | `cli.rs::set_my_commands_*` | tests/cli.rs | ✅ |

### Этап 8: Hardening

| Требование | Тест | Файл | Статус |
|---|---|---|---|
| SIGTERM во время long poll → exit 0 | `e2e_binary.rs::sigterm_during_long_poll_exits_zero` | tests/e2e_binary.rs | ✅ |
| SIGTERM с in-flight outbox → exit 0 (no loss) | `e2e_binary.rs::sigterm_with_answer_in_flight_finishes_it` | tests/e2e_binary.rs | ✅ |
| SIGTERM → закрытие БД, коммит transaction | `e2e_binary.rs::sigterm_*` (check exit code 0) | tests/e2e_binary.rs | ✅ |
| Docker smoke-test полный (non-root, read-only, healthcheck, SIGTERM) | `scripts/docker-smoke.sh` (9 проверок) | scripts/docker-smoke.sh | ✅ |

## Структура тестов

```
tests/
├── cli.rs                    # CLI: команды, конфиг, валидация (5 тестов)
├── core_flow.rs             # Core flow: Paperclip ↔ Telegram (19 тестов)
├── crash.rs                 # Рестарт и crash-recovery (3 теста + fault-injection)
├── e2e_binary.rs            # E2E реального бинарника (SIGTERM, long poll)
├── env_proxy.rs             # Env-proxy ignored (1 тест)
├── forms_flow.rs            # Многошаговые формы, вердикты (10 тестов)
├── governed.rs              # Governed actions: toolAction, secretProposal (18 тестов)
├── paperclip_contract.rs    # Contract-тесты Paperclip API (21 тест)
├── runs_commands.rs         # Runs, notifications, commands (7 тестов)
├── sender.rs                # Queue, rate limit, retry_after (3 теста)
├── store.rs                 # SQLite store, migrations (11 тестов)
├── telegram_proxy.rs        # Proxy mandatory, no fallback (4 теста)
└── support/
    ├── harness.rs           # Test harness: запуск bridge, mock-серверы
    ├── mod.rs               # Utilities
    ├── paperclip.rs         # Mock Paperclip API
    └── proxy.rs             # Counting HTTP proxy для проверки трафика

src/
├── config.rs                # Unit-тесты парсинга конфигурации
├── cli.rs                   # Unit-тесты CLI парсинга
├── health.rs                # Unit-тесты healthcheck
└── ...                      # Встроенные модульные тесты в других файлах
```

**Всего:** 102+ тестовых функции, структурированных по этапам архитектуры.

## Примечания о фикстурах

- **Mock-серверы:** wiremock + встроенные mock Paperclip и Telegram в `tests/support/`
- **Виртуальное время:** `tokio::time::pause()` для таймингов (retry_after, 5 мин в governed, SHUTDOWN_GRACE)
- **Fault-injection:** feature `fault-injection` в Cargo.toml для инъекции падений в критических местах
- **Нет реальных credentials:** токены, IP адреса, ID — только плейсхолдеры
- **Docker smoke-test:** dummy secrets, closed ports (127.0.0.1:9), named volume для `/state`

## Запуск CI локально

```bash
# Формат
cargo fmt --all -- --check

# Linter
cargo clippy --all-targets --all-features -- -D warnings

# Тесты
cargo test --all-features

# Безопасность (licenses, advisories)
cargo deny check all

# Docker (требуется Docker и jq)
docker build -t paperclip-telegram-bridge:test .
bash scripts/docker-smoke.sh
```

## Известные ограничения

1. **Governed approvals** — фикстуры API §6.6, без живого сервера (по согласованию)
2. **Таймауты** — виртуальное время (быстрые тесты, без реальных sleep)
3. **Failover scenarios** — proxy тестируется на закрытом порту, не на реальном сбое сети
