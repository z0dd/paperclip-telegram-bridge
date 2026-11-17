# Тестирование paperclip-telegram-bridge

## Состав

| Что | Где | Количество |
|---|---|---|
| Интеграционные тесты (mock Paperclip на wiremock, fake Telegram, реальный SQLite, реальный бинарь) | `tests/*.rs`, общая обвязка в `tests/support/` | 134 |
| Unit-тесты | `#[cfg(test)]` в `src/**` | 52 |
| Docker smoke-test | `scripts/docker-smoke.sh`, job `docker` в `.github/workflows/ci.yml` | 11 шагов |
| Скрипты PET-12 (compose, секреты, gitignore) | `scripts/compose-config-check.sh`, `scripts/compose-smoke.sh`, `scripts/secrets-rootless-test.sh`, `scripts/gitignore-check-test.sh` | см. раздел «PET-12» |

Интеграционные тесты по файлам: `chat_text.rs` 16, `cli.rs` 7, `core_flow.rs` 19, `crash.rs` 7, `draft_migration.rs` 3, `e2e_binary.rs` 7, `env_proxy.rs` 1, `forms_flow.rs` 10, `governed.rs` 18, `paperclip_contract.rs` 21, `runs_commands.rs` 7, `sender.rs` 3, `store.rs` 11, `telegram_proxy.rs` 4.

Цифры считаются командой `grep -cE '^\s*#\[(tokio::)?test' <файл>`. Ссылки таблицы ниже проверяет `scripts/check-testing-refs.sh`.

## Как запускать

```bash
cargo test --all-features             # всё, включая crash-тесты (feature fault-injection)
cargo test --test crash --features fault-injection
cargo test --test e2e_binary          # запускает реальный бинарь, нужен свободный loopback
bash scripts/docker-smoke.sh          # нужны docker и jq; образ paperclip-telegram-bridge:test должен быть собран
bash scripts/check-testing-refs.sh    # каждая ссылка «файл::тест» из этого документа существует
```

Образ для smoke-test: `docker build -t paperclip-telegram-bridge:test .`. Внешняя сеть и настоящие credentials не нужны: токены фиктивные (`TEST-DUMMY-TOKEN`, `pcp_dummy`), upstream-адреса указывают на закрытый порт `127.0.0.1:9`.

CI (`gh pr checks`): `fmt`, `clippy`, `test`, `deny`, `gitignore-secrets`, `compose-config`, `rootless-secrets`, `docker` (сборка с `load: true` + smoke-test + compose smoke) на каждый push в PR.

## Docker smoke-test

Скрипт использует `docker` CLI, `jq`, `ps` на хосте раннера и `docker exec ... /usr/local/bin/bridge healthcheck` (в distroless-образе нет shell, больше ничего внутри не запускается).

| Шаг | Проверка |
|---|---|
| 1 | `Config.User` не пуст и не `0`/`root` |
| 2 | `Config.ExposedPorts` пуст |
| 3 | `docker run --rm ... check-config` с теми же env: exit 0, в выводе нет токенов |
| 4 | Сервис стартует штатной командой с `--read-only`, `--cap-drop ALL`, `no-new-privileges`, state в named volume |
| 5 | `bridge healthcheck` сразу после старта → exit 0 (отсчёт идёт от старта процесса, `src/health.rs`) |
| 6 | Через ≥5 с `State.Running == true`, `RestartCount == 0`, `ReadonlyRootfs == true` |
| 7 | `docker port` пуст |
| 8 | uid процесса на хосте (`ps -o uid= -p <State.Pid>`) не `0` |
| 9 | Через ≥3 с `docker exec -e HEALTH_MAX_STALE=1s ... healthcheck` → exit ≠ 0 и в выводе `no successful` (upstream на закрытом порту) |
| 10 | В `docker logs` нет `TEST-DUMMY-TOKEN` и `pcp_dummy` |
| 11 | `docker stop -t 15`: `ExitCode == 0` и длительность < 12 с (`SHUTDOWN_GRACE=10s`) |

## Покрытие: ТЗ, раздел «Тесты» → тесты

Статус: **покрыто** / **слабо** (есть тест, но часть требования проверена косвенно) / **не покрыто**.

| Требование ТЗ | Тесты | Статус |
|---|---|---|
| unit-тесты разбора Telegram updates (message / callback / неизвестные) | `src/core/allowlist.rs::updates_are_parsed` | покрыто |
| allowlist chat/user (чужой chat, чужой user, группа) | `src/core/allowlist.rs::admits_owner`, `src/core/allowlist.rs::rejects_foreign_chat_user_and_groups`, `tests/core_flow.rs::foreign_updates_are_ignored_silently` | покрыто |
| чужие updates журналируются без содержимого | `src/core/allowlist.rs::rejection_log_is_rate_limited`, `tests/core_flow.rs::foreign_updates_are_ignored_silently` | покрыто |
| interaction → Telegram-сообщение | `tests/core_flow.rs::interaction_becomes_a_message_with_buttons`, `src/render/cards.rs::confirmation_card_is_escaped_and_has_buttons`, `src/render/cards.rs::checkbox_card_marks_selection` | покрыто |
| callback → ответ в Paperclip | `tests/core_flow.rs::callback_sends_accept_once_and_updates_the_card`, `tests/core_flow.rs::checkbox_selection_is_sent`, `tests/paperclip_contract.rs::resolution_request_bodies_match_contract` | покрыто |
| multi-question формы | `tests/forms_flow.rs::full_form_with_back_validation_and_summary`, `tests/forms_flow.rs::cancel_resets_the_draft_and_partial_form_is_not_sent`, `tests/forms_flow.rs::too_many_questions_is_only_a_notice`, `tests/forms_flow.rs::single_question_single_select_submits_on_press` | покрыто |
| идемпотентность | `tests/store.rs::duplicate_update_has_one_effect`, `tests/store.rs::at_most_one_outbox_entry_per_card`, `tests/store.rs::duplicate_interaction_is_not_inserted_twice`, `tests/core_flow.rs::callback_sends_accept_once_and_updates_the_card`, `tests/governed.rs::accept_with_failed_result_and_repeat_is_one_effect` | покрыто |
| восстановление состояния после рестарта | `tests/store.rs::state_survives_restart`, `tests/store.rs::crash_in_flight_and_sending_are_recovered`, `tests/forms_flow.rs::restart_in_the_middle_of_a_form_continues` | покрыто |
| duplicate Telegram update | `tests/core_flow.rs::duplicate_update_is_processed_once`, `tests/store.rs::duplicate_update_has_one_effect`, `tests/crash.rs::crash_before_commit_of_update_loses_nothing` | покрыто |
| недоступный Paperclip | `tests/paperclip_contract.rs::unreachable_paperclip_is_transient_and_leaks_nothing`, `tests/core_flow.rs::paperclip_down_answer_waits_and_is_delivered_later`, `tests/runs_commands.rs::commands_report_paperclip_unavailable` | покрыто |
| недоступный Xray (proxy) | `tests/telegram_proxy.rs::unavailable_proxy_means_zero_direct_requests`, `tests/core_flow.rs::telegram_down_delivery_is_retried_next_cycle` | покрыто |
| отсутствие direct fallback к Telegram | `tests/telegram_proxy.rs::unavailable_proxy_means_zero_direct_requests`, `tests/telegram_proxy.rs::every_request_goes_through_the_proxy`, `tests/telegram_proxy.rs::proxy_is_mandatory_in_the_type`, `tests/e2e_binary.rs::sigterm_during_long_poll_exits_zero` (число запросов через proxy равно числу запросов к mock) | покрыто |
| integration с mock Paperclip и mock Telegram | `tests/core_flow.rs`, `tests/forms_flow.rs`, `tests/governed.rs`, `tests/runs_commands.rs`, `tests/e2e_binary.rs::sigterm_with_answer_in_flight_finishes_it` | покрыто |
| Docker smoke-test без настоящих credentials | `scripts/docker-smoke.sh` (шаги 1–11), job `docker` в CI | покрыто |

## Покрытие: architecture §12, колонка «Test Engineer проверяет»

### Этап 1. Конфигурация и Docker

| Требование | Тесты | Статус |
|---|---|---|
| парсинг конфигурации | `src/config.rs::parses_minimal_config_with_defaults`, `src/config.rs::optional_values_are_parsed`, `src/config.rs::invalid_values_name_the_variable` | покрыто |
| `*_FILE` секреты | `src/config.rs::secret_file_trims_trailing_newline`, `src/config.rs::empty_secret_file_is_rejected`, `src/config.rs::missing_secret_file_is_rejected` | покрыто |
| отказ без proxy | `src/config.rs::missing_proxy_is_rejected`, `tests/cli.rs::check_config_fails_without_proxy`, `tests/cli.rs::run_fails_without_proxy_before_any_network` | покрыто |
| отказ с неверной схемой proxy | `src/config.rs::proxy_scheme_is_validated_without_echoing_value` | покрыто |
| секрет не печатается в Debug/логах | `src/config.rs::secrets_are_redacted_in_debug`, `src/config.rs::proxy_debug_hides_credentials`, `tests/cli.rs::check_config_ok_does_not_print_secrets`, `src/logging.rs::hash_is_short_and_stable` | покрыто |
| exit-коды `healthcheck` | `tests/cli.rs::healthcheck_exit_codes`, `src/health.rs::unhealthy_cases`, `src/health.rs::fresh_file_is_healthy`, `src/health.rs::grace_after_start_before_first_success` | покрыто |
| `docker run` non-root, read-only FS, нет портов | `scripts/docker-smoke.sh`, шаги 1, 2, 4, 6, 7, 8 | покрыто |
| healthcheck healthy в штатной работе | `scripts/docker-smoke.sh` шаг 5 (только окно «после старта»); healthy после успешных вызовов: `src/health.rs::fresh_file_is_healthy`, `src/health.rs::tracker_snapshot` | слабо: в smoke-test нет живых mock-серверов, поэтому «healthy после успешных вызовов» проверяется только на уровне библиотеки; unhealthy-ветка проверена и в smoke (шаг 9) |

### Этап 2. Paperclip API client

| Требование | Тесты | Статус |
|---|---|---|
| все kinds interactions | `tests/paperclip_contract.rs::question_set_form_is_parsed`, `tests/paperclip_contract.rs::legacy_questions_are_parsed`, `tests/paperclip_contract.rs::confirmation_checkbox_and_verdicts_are_parsed`, `tests/paperclip_contract.rs::governed_cards_are_parsed`, `tests/paperclip_contract.rs::unsupported_kinds_are_marked` | покрыто |
| accept / reject / respond | `tests/paperclip_contract.rs::resolution_request_bodies_match_contract`, `tests/paperclip_contract.rs::governed_accept_sends_empty_body_without_remember_action` | покрыто |
| все 409-коды | `tests/paperclip_contract.rs::conflict_codes_are_classified` | покрыто |
| 403 audience | `tests/paperclip_contract.rs::auth_and_audience_errors_are_classified` | покрыто |
| 422 и 5xx | `tests/paperclip_contract.rs::validation_and_server_errors_are_classified` | покрыто |
| 429 `retry_after` в клиенте | `tests/paperclip_contract.rs::rate_limit_carries_retry_after` | покрыто |
| таймаут | `tests/paperclip_contract.rs::timeout_is_transient` | покрыто |
| недоступный Paperclip | `tests/paperclip_contract.rs::unreachable_paperclip_is_transient_and_leaks_nothing` | покрыто |
| лента attention, issues, runs, agents, revoke | `tests/paperclip_contract.rs::attention_maps_all_source_kinds`, `tests/paperclip_contract.rs::attention_follows_cursor_and_restarts_on_stale_cursor`, `tests/paperclip_contract.rs::issues_runs_agents_and_revoke`, `tests/paperclip_contract.rs::approvals_are_listed_and_approved`, `tests/paperclip_contract.rs::base_url_with_path_prefix_is_respected` | покрыто |
| Bearer без run-id | `tests/paperclip_contract.rs::sends_bearer_token_and_no_run_id_header` | покрыто |
| клиент игнорирует `HTTP(S)_PROXY` | `tests/env_proxy.rs::env_proxy_variables_are_ignored` | покрыто |

### Этап 3. Telegram client, разбор, render

| Требование | Тесты | Статус |
|---|---|---|
| разбор updates, allowlist | см. таблицу ТЗ выше | покрыто |
| классификация ошибок Telegram | `src/telegram/http.rs::classification` | покрыто |
| HTML-экранирование (`<`, `&`, emoji) | `src/render/text.rs::escapes_html_special_chars`, `src/render/text.rs::visible_length_ignores_tags_and_counts_entities_once` | покрыто |
| суррогатные пары UTF-16 | `src/render/text.rs::utf16_counts_surrogate_pairs`, `src/render/text.rs::split_never_breaks_surrogate_pairs` | покрыто |
| разбиение и лимит 4096 UTF-16 | `src/render/text.rs::split_prefers_paragraphs_then_lines_then_words`, `src/render/text.rs::split_pieces_fit_telegram_limit_after_escaping`, `src/render/text.rs::truncation`, `src/render/cards.rs::long_details_are_split_and_capped` | покрыто |
| callback_data ≤ 64 байт для максимальных индексов | `src/render/callback.rs::maximum_values_fit_in_64_bytes`, `src/render/callback.rs::roundtrip_all_ops`, `src/render/callback.rs::example_from_architecture`, `src/render/callback.rs::rejects_garbage` | покрыто |
| §5.3: недоступный Xray, нет direct fallback | `tests/telegram_proxy.rs::unavailable_proxy_means_zero_direct_requests`, `tests/telegram_proxy.rs::every_request_goes_through_the_proxy` | покрыто |
| §5.3: env-proxy не влияет на Telegram | `tests/env_proxy.rs::env_proxy_variables_are_ignored` (проверка Telegram-клиента внутри `support::env_proxy_telegram_checks`) | покрыто |
| §5.3: токен не попадает в логи при ошибке | `tests/telegram_proxy.rs::token_is_not_in_errors_or_logs_on_connection_failure` | покрыто |

### Этап 4. Хранилище и рестарт

| Требование | Тесты | Статус |
|---|---|---|
| схема, миграции, pragma | `tests/store.rs::schema_is_versioned_and_pragmas_are_set` | покрыто |
| `integrity_check`, повреждённая БД отвергается | `tests/store.rs::corrupt_database_is_refused` | покрыто |
| state переживает рестарт | `tests/store.rs::state_survives_restart` | покрыто |
| один `update_id` дважды → один эффект | `tests/store.rs::duplicate_update_has_one_effect`, `tests/core_flow.rs::duplicate_update_is_processed_once` | покрыто |
| crash после sendMessage до коммита | `tests/crash.rs::crash_after_send_before_commit_redelivers_the_card` | покрыто |
| crash после HTTP до коммита outbox | `tests/crash.rs::crash_after_http_before_outbox_commit_sends_once` | покрыто |
| crash до коммита update | `tests/crash.rs::crash_before_commit_of_update_loses_nothing` | покрыто |
| восстановление in-flight/sending | `tests/store.rs::crash_in_flight_and_sending_are_recovered` | покрыто |
| расписание retry, очистка тел | `tests/store.rs::retry_schedule_and_removal`, `tests/store.rs::finished_outbox_wipes_body_and_draft`, `src/retry.rs::ceiling_grows_exponentially_and_caps`, `src/retry.rs::delay_is_within_bounds` | покрыто |

### Этап 5. Core

| Требование | Тесты | Статус |
|---|---|---|
| interaction → сообщение | `tests/core_flow.rs::interaction_becomes_a_message_with_buttons` | покрыто |
| callback → правильный запрос | `tests/core_flow.rs::callback_sends_accept_once_and_updates_the_card` | покрыто |
| повторное нажатие → один запрос | `tests/core_flow.rs::callback_sends_accept_once_and_updates_the_card`, `tests/core_flow.rs::stale_generation_answers_outdated` | покрыто |
| ответ в UI раньше Telegram → 409 → сообщение | `tests/core_flow.rs::answered_in_ui_first_reports_resolved_elsewhere`, `tests/core_flow.rs::card_resolved_in_ui_is_edited`, `tests/core_flow.rs::resolved_elsewhere_edit_can_be_disabled` | покрыто |
| гонка: потерянный ответ распознаётся как наш | `tests/core_flow.rs::lost_response_is_recognised_as_our_answer`, `tests/core_flow.rs::agent_resolution_is_not_ours` | покрыто |
| Paperclip недоступен → ответ в очереди → доставлен | `tests/core_flow.rs::paperclip_down_answer_waits_and_is_delivered_later` | покрыто |
| ошибка валидации возвращает карточку пользователю | `tests/core_flow.rs::validation_error_returns_the_card_to_the_user` | покрыто |
| reject с причиной, отмена | `tests/core_flow.rs::reject_with_required_reason_waits_for_text`, `tests/core_flow.rs::optional_reason_and_cancel`, `src/render/cards.rs::reject_reason_prompt_buttons` | покрыто |
| неподдерживаемые kinds → только уведомление | `tests/core_flow.rs::unsupported_kinds_are_notifications_without_buttons`, `src/render/cards.rs::unsupported_card_has_no_buttons` | покрыто |
| много новых карточек → сводка | `tests/core_flow.rs::many_new_cards_get_a_summary_first` | покрыто |
| не-текстовые сообщения отвергаются | `tests/core_flow.rs::non_text_messages_are_refused` | покрыто |

### Этап 6. Формы

| Требование | Тесты | Статус |
|---|---|---|
| полный проход, назад, валидация | `tests/forms_flow.rs::full_form_with_back_validation_and_summary` | покрыто |
| рестарт посреди формы | `tests/forms_flow.rs::restart_in_the_middle_of_a_form_continues` | покрыто |
| отмена, частичные ответы не уходят | `tests/forms_flow.rs::cancel_resets_the_draft_and_partial_form_is_not_sent` | покрыто |
| более 20 вопросов → только уведомление | `tests/forms_flow.rs::too_many_questions_is_only_a_notice` | покрыто |
| text reply, свой вариант | `tests/forms_flow.rs::own_answer_and_text_reply`, `tests/forms_flow.rs::legacy_free_text_option_is_sent_with_its_id` | покрыто |
| `textValidation` | `src/core/forms.rs::text_validation_rules`, `tests/forms_flow.rs::server_validation_error_keeps_the_form` | покрыто |
| verdicts частично / полностью | `tests/forms_flow.rs::verdicts_card`, `tests/forms_flow.rs::verdicts_partial_then_complete`, `tests/forms_flow.rs::verdicts_bulk_approve` | покрыто |
| состояние ожидания текста | `src/domain/draft.rs::awaiting_roundtrip`, `src/core/forms.rs::text_validation_rules` | покрыто |

### Этап 6a. Governed

| Требование | Тесты | Статус |
|---|---|---|
| accept → `executed`/`failed`/`executing`, дочитывание статуса | `tests/governed.rs::write_action_accept_executes_and_follows_result`, `tests/governed.rs::accept_with_failed_result_and_repeat_is_one_effect` | покрыто |
| повтор accept → один эффект | `tests/governed.rs::accept_with_failed_result_and_repeat_is_one_effect` | покрыто |
| `409` без кода и `500` → без бесконечного retry | `tests/governed.rs::conflict_without_code_and_500_are_not_retried_forever` | покрыто |
| destructive: второе шаг, approval, accept | `tests/governed.rs::destructive_confirm_mode_two_steps_then_approval_then_accept` | покрыто |
| истечение второго шага | `tests/governed.rs::destructive_second_step_expires_after_two_minutes` | покрыто |
| рестарт между шагами → без дубля | `tests/governed.rs::destructive_restart_between_steps_repeats_without_duplicate` | покрыто |
| approval не найден → «решите в UI» | `tests/governed.rs::destructive_without_approval_asks_to_decide_in_ui`, `tests/governed.rs::approval_found_by_invocation_id_fallback` | покрыто |
| reject → reject approval | `tests/governed.rs::reject_destructive_rejects_linked_approval`, `tests/governed.rs::write_reject_without_reason` | покрыто |
| `secretProposal`: failed, reject без причины | `tests/governed.rs::secret_proposal_bind_and_failed_binding`, `tests/governed.rs::secret_proposal_reject_requires_reason` | покрыто |
| истёкший `expiresAt` / изменённый `argumentsHash` | `tests/governed.rs::changed_arguments_or_expired_send_nothing`, `tests/governed.rs::hash_changed_between_queue_and_send_is_not_sent` | покрыто |
| нет `rememberAction` в запросах | `tests/governed.rs::write_action_accept_executes_and_follows_result` (прямой assert: в телах запросов нет `rememberAction`), `tests/paperclip_contract.rs::governed_accept_sends_empty_body_without_remember_action` | покрыто |
| нет `detailsMarkdown`/`toolName`; аргументы только по флагу | `tests/governed.rs::arguments_shown_only_with_flag_and_card_fits_one_message` | покрыто |
| в логах нет preview/аргументов/justification | `tests/governed.rs::logs_contain_decisions_but_no_content` | покрыто |
| `GOVERNED_APPROVALS_ENABLED=false` → только уведомление | `tests/governed.rs::governed_disabled_is_notification_only`, `tests/governed.rs::ui_only_mode_has_no_buttons_and_sends_nothing_but_write_works` | покрыто |

### Этап 7. Уведомления и команды

| Требование | Тесты | Статус |
|---|---|---|
| только новые `failed/timed_out`, без дублей | `tests/runs_commands.rs::only_new_failures_are_reported_once`, `tests/runs_commands.rs::failed_run_from_the_feed_is_reported` | покрыто |
| без текста ошибки | `tests/runs_commands.rs::only_new_failures_are_reported_once` (assert на `SECRET-ERROR-TEXT`) | покрыто |
| флаги `NOTIFY_RUN_FAILURES`, `NOTIFY_TASK_COMPLETED` | `tests/runs_commands.rs::run_failure_notifications_can_be_disabled`, `tests/runs_commands.rs::completed_tasks_only_with_flag` | покрыто |
| `/status`, `/backlog`, `/help` на фикстурах | `tests/runs_commands.rs::status_and_backlog_commands`, `tests/runs_commands.rs::commands_report_paperclip_unavailable` | покрыто |
| отозванный ключ: одно уведомление, health падает | `tests/runs_commands.rs::revoked_key_is_reported_once_and_makes_health_fail` | покрыто |
| `setMyCommands` вызывается с правильным списком | **добавлен** `tests/e2e_binary.rs::set_my_commands_registers_the_command_list_via_proxy` | было не покрыто: fake Telegram запоминал только факт вызова (`TgCall::Commands`), без содержимого; `/help` по тексту проверен, а список из `bot_commands()` нет |

### Этап 8. Hardening

| Требование | Тесты | Статус |
|---|---|---|
| `429 retry_after` соблюдается | `tests/sender.rs::retry_after_is_honoured_and_order_kept` | покрыто |
| лимит очереди отправки | `tests/sender.rs::messages_are_spaced_by_min_interval` | покрыто |
| повтор временных ошибок до лимита | `tests/sender.rs::transient_errors_are_retried_up_to_the_limit` | покрыто |
| SIGTERM во время long poll → exit 0 | `tests/e2e_binary.rs::sigterm_during_long_poll_exits_zero` | покрыто |
| SIGTERM при in-flight outbox → exit 0, без потерь | `tests/e2e_binary.rs::sigterm_with_answer_in_flight_finishes_it` | покрыто |
| Docker smoke-test: non-root, read-only, healthcheck, порты, SIGTERM | `scripts/docker-smoke.sh` | покрыто (см. замечание про healthy выше) |
| Docker smoke-test с mock-серверами в docker network | не делался | слабо: smoke использует закрытые порты вместо mock-серверов; сквозные сценарии с mock Paperclip/Telegram покрыты `tests/e2e_binary.rs` (реальный бинарь) вне контейнера |

## PET-12 (v1.1.0): критерий приёмки → тест → job CI

Источник критериев: ADR-0009 (`docs/adr/0009-v1.1.0-improvements.md`). Тесты Rust выполняет job `test` (`cargo test --all-features`, включая crash-тесты с feature `fault-injection`); скрипты — отдельные job'ы. Строки «№» — строки таблицы ADR-0009 §1.

### Пункт 1. Текст как чат (ADR-0009 §1)

| Сценарий | Тест | Job CI |
|---|---|---|
| Карточка однотекстового вопроса без кнопки «Ответить текстом», только подсказка | `tests/chat_text.rs::card_has_no_text_button_only_a_hint` | test |
| №6: reply на карточку, ответ уходит сразу, без подтверждения | `tests/chat_text.rs::reply_to_card_is_queued_at_once`, `tests/forms_flow.rs::own_answer_and_text_reply` | test |
| №12: без reply, одна подходящая карточка | `tests/chat_text.rs::message_without_reply_answers_the_only_card` | test |
| №13: без reply, несколько карточек, ничего не отправляется | `tests/chat_text.rs::several_cards_ask_for_a_reply_and_send_nothing`, `tests/chat_text.rs::several_cards_in_different_issues_are_listed` | test |
| №14: нет подходящей карточки, ничего не уходит в Paperclip | `tests/chat_text.rs::no_suitable_card_sends_nothing_to_paperclip`, `tests/core_flow.rs::reject_with_required_reason_waits_for_text` | test |
| №7: невалидный текст, вопрос остаётся открытым | `tests/chat_text.rs::invalid_text_keeps_the_question_open` | test |
| №5, №11: ожидающий сценарий (причина reject и т. п.) забирает текст первым | `tests/chat_text.rs::waiting_scenario_takes_the_text_first`, `tests/chat_text.rs::waiting_without_reply_does_not_answer_the_text_card` | test |
| №8: reply на неподходящую карточку работает как раньше | `tests/chat_text.rs::reply_to_an_unsuitable_card_works_as_before`, `tests/chat_text.rs::multi_question_form_still_uses_the_text_button` | test |
| №9, №15: повторный текст не даёт второго ответа | `tests/chat_text.rs::second_text_after_the_answer_is_not_sent` | test |
| №10: reply на сообщение, не являющееся карточкой | `tests/chat_text.rs::reply_to_a_message_that_is_not_a_card_is_routed_like_plain_text` | test |
| №3: повторная доставка того же update | `tests/chat_text.rs::repeated_update_is_ignored`, `tests/core_flow.rs::duplicate_update_is_processed_once` | test |
| №16: старая кнопка → «Устарело» | `tests/chat_text.rs::old_text_button_is_stale` | test |
| №17: отказ сервера (422) возвращает вопрос | `tests/chat_text.rs::server_rejection_reopens_the_question` | test |
| №18: рестарт с непустым outbox, без reply | `tests/crash.rs::chat_text_answer_survives_a_restart_and_is_sent_once` | test (feature `fault-injection`) |
| №18: рестарт с непустым outbox, reply | `tests/crash.rs::chat_text_reply_survives_a_restart_and_is_sent_once` | test (feature `fault-injection`) |
| №18: падение после HTTP до коммита, reply: нет второго ответа | `tests/crash.rs::chat_text_crash_after_http_reconciles_without_a_second_answer` | test (feature `fault-injection`) |
| №18: падение после HTTP до коммита, без reply: нет второго ответа | `tests/crash.rs::chat_text_without_reply_crash_after_http_reconciles_without_a_second_answer` | test (feature `fault-injection`) |
| Регрессия неизменённых карточек: варианты, формы | `tests/forms_flow.rs::full_form_with_back_validation_and_summary`, `tests/forms_flow.rs::single_question_single_select_submits_on_press`, `tests/forms_flow.rs::legacy_free_text_option_is_sent_with_its_id` | test |
| Регрессия: Approve/Reject | `tests/core_flow.rs::callback_sends_accept_once_and_updates_the_card`, `tests/core_flow.rs::reject_with_required_reason_waits_for_text`, `tests/core_flow.rs::optional_reason_and_cancel` | test |
| Регрессия: destructive | `tests/governed.rs::destructive_confirm_mode_two_steps_then_approval_then_accept`, `tests/governed.rs::destructive_restart_between_steps_repeats_without_duplicate`, `tests/governed.rs::reject_destructive_rejects_linked_approval` | test |

Правки старых тестов в PET-15 (`git diff main...pet-12-improvements -- tests/`) соответствуют ADR и ожиданий не ослабляют: `tests/core_flow.rs::reject_with_required_reason_waits_for_text` меняет только текст подсказки «Не жду текста» → «Сейчас нет вопросов, на которые можно ответить текстом» (строка 14); `tests/forms_flow.rs::own_answer_and_text_reply` убирает нажатие «Отправить» после reply, потому что подтверждения больше нет (строка 6), при этом проверка тела `respond` сохранена.

### Пункт 2. Миграция старых draft (ADR-0009 §2)

| Сценарий | Тест | Job CI |
|---|---|---|
| Первый старт: сброшены только draft однотекстовых карточек (A, D), B и C не тронуты, outbox пуст, к Paperclip нет POST | `tests/draft_migration.rs::first_start_resets_only_text_card_drafts` | test |
| Второй старт ничего не меняет | `tests/draft_migration.rs::second_start_changes_nothing` | test |
| Отказ перерисовки не останавливает старт, старая кнопка → «Устарело» | `tests/draft_migration.rs::failed_rerender_does_not_stop_the_start` | test |

### Пункт 3. Проверка версии Paperclip (ADR-0009 §3)

| Сценарий | Тест | Job CI |
|---|---|---|
| Классификация версий (tested / other / missing / unrecognized) | `src/paperclip/version.rs::classify_table` | test |
| Совпала: один INFO, без WARN | `src/paperclip/version.rs::tested_version_is_one_info_without_warn`, `tests/e2e_binary.rs::tested_paperclip_version_is_info_only` (бинарь, запуск продолжается) | test |
| Другая версия: WARN с обеими версиями, запуск продолжается | `src/paperclip/version.rs::other_version_warns_with_both_versions`, `tests/e2e_binary.rs::other_paperclip_version_warns_and_starts` | test |
| Версию не удалось получить: WARN, запуск продолжается | `src/paperclip/version.rs::missing_version_warns`, `tests/e2e_binary.rs::missing_paperclip_version_warns_and_starts` | test |
| Нераспознанный формат: WARN, запуск продолжается | `src/paperclip/version.rs::unrecognized_version_warns_and_is_truncated`, `tests/e2e_binary.rs::unrecognized_paperclip_version_warns_and_starts` | test |

### Пункт 4. Deployment (ADR-0009 §4, §5)

| Сценарий | Скрипт | Job CI |
|---|---|---|
| Оба compose-примера рендерятся, hardening совпадает, host-пример без `networks`/`ports` | `scripts/compose-config-check.sh` | `compose-config` |
| Compose smoke обоих примеров (host: реальный образ и busybox с тем же hardening; isolated) | `scripts/compose-smoke.sh` | `docker` (шаг «Compose smoke») |
| Права secret-файлов в rootless Docker: 0400 strict и 0644+каталог 0700 читаются; 0400/0600/0640 владельцем хоста не читаются контейнером; несовпадение → exit 1 | `scripts/secrets-rootless-test.sh` | `rootless-secrets` |
| Секреты в `.gitignore` (позитивная проверка) | шаг «Check that secrets paths are gitignored» | `gitignore-secrets` |
| Проверка gitignore падает при удалении правил (негативная, на временной копии) | `scripts/gitignore-check-test.sh` | `gitignore-secrets` |

Ручные проверки, которых CI не заменяет: живой деплой на сервере пользователя (реальный Paperclip, Xray, rootless Docker с `network_mode: host`) и внешний вид карточек в клиенте Telegram.

## Известные ограничения набора

- Тесты таймингов `tests/sender.rs` работают на коротких реальных интервалах, а не на `tokio::time::pause()`. Они проходят в CI, но по принципу остаются потенциально чувствительными к нагрузке раннера. Переписывать на виртуальное время не стал: это правка обвязки, не покрытия.
- Governed-сценарии проверены только на фикстурах контракта (API §6.6): в компании нет реальных инструментов с approval. Живая проверка выполняется при деплое.
- Контейнерный smoke-test не поднимает mock-серверы; см. строки «слабо» выше.
