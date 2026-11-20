# Тестирование paperclip-telegram-bridge

Документ описывает набор тестов версии 2.0.0 и составлен по `tests/` и `src/` на `main`. Тесты v1 (карточки, формы, governed, команды) удалены вместе с функциями; их описание осталось в истории git, тег `v1.1.0`.

## Состав

| Что | Где | Количество |
|---|---|---|
| Интеграционные тесты (mock Paperclip на wiremock, fake Telegram, реальный SQLite, реальный бинарь) | `tests/*.rs`, общая обвязка в `tests/support/` | 104 |
| Unit-тесты | `#[cfg(test)]` в `src/**` | 66 |
| Docker smoke-test | `scripts/docker-smoke.sh`, job `docker` в `.github/workflows/ci.yml` | 11 шагов |
| Скрипты deployment (compose, секреты, gitignore) | `scripts/compose-config-check.sh`, `scripts/compose-smoke.sh`, `scripts/secrets-rootless-test.sh`, `scripts/gitignore-check-test.sh` | см. раздел «Deployment» |

Интеграционные тесты по файлам: `cli.rs` 9, `crash.rs` 3, `e2e_binary.rs` 8, `env_proxy.rs` 1, `flow.rs` 22, `gaps.rs` 15, `notices.rs` 7, `paperclip_contract.rs` 12, `rule.rs` 5, `sender.rs` 2, `store.rs` 11, `telegram_errors.rs` 4, `telegram_proxy.rs` 5.

Что в каком файле:

| Файл | Что проверяет |
|---|---|
| `flow.rs` | основные потоки на обвязке: обнаружение компаний и бесед, темы, Paperclip → Telegram, Telegram → Paperclip, рестарт, ошибки тем, недоступный Paperclip, отозванный ключ |
| `gaps.rs` | рестарт с работой в обе стороны, отсутствие эха, `retry_after`, границы длины сообщения Telegram, экранирование, проверки супергруппы при старте, классификатор ошибок тем сквозным путём |
| `notices.rs` | правило ⚠️ на обвязке: какие комментарии получают пометку |
| `rule.rs` | таблицы правил `domain::comment`: что пересылается, что служебное сообщение |
| `crash.rs` | краши с feature `fault-injection` |
| `store.rs` | хранилище: миграция v1 → v2, атомарные коммиты, идемпотентность, восстановление |
| `paperclip_contract.rs`, `telegram_errors.rs` | HTTP-клиенты и классификация ошибок на mock-серверах |
| `telegram_proxy.rs`, `env_proxy.rs` | Telegram только через proxy, `HTTP(S)_PROXY` игнорируется, токен не попадает в ошибки |
| `sender.rs` | темп очереди Telegram и `retry_after` |
| `cli.rs`, `e2e_binary.rs` | подкоманды бинаря, SIGTERM, проверки при старте, проверка версии Paperclip |

Цифры считаются командой `grep -cE '^\s*#\[(tokio::)?test' <файл>`. Ссылки в таблицах ниже проверяет `scripts/check-testing-refs.sh`.

## Как запускать

```bash
cargo test --all-features             # всё, включая crash-тесты (feature fault-injection)
cargo test --test crash --features fault-injection
cargo test --test e2e_binary          # запускает реальный бинарь, нужен свободный loopback
bash scripts/docker-smoke.sh          # нужны docker и jq; образ paperclip-telegram-bridge:test должен быть собран
bash scripts/check-testing-refs.sh    # каждая ссылка «файл::тест» из этого документа существует
```

Crash-тесты без feature `fault-injection` не компилируются (`#![cfg(feature = "fault-injection")]`). Точки отказа глобальные, поэтому тесты `crash.rs` берут общий мьютекс и идут по одному.

Образ для smoke-test: `docker build -t paperclip-telegram-bridge:test .`. Внешняя сеть и настоящие credentials не нужны: токены фиктивные (`TEST-DUMMY-TOKEN`, `pcp_dummy`), upstream-адреса указывают на закрытый порт `127.0.0.1:9`.

CI (`gh pr checks`): `fmt`, `clippy`, `test`, `deny`, `gitignore-secrets`, `compose-config`, `rootless-secrets`, `docker` (сборка с `load: true` + smoke-test + compose smoke) на каждый push в PR.

## Обвязка

`tests/support/` содержит три заменителя внешнего мира:

- `PaperclipMock` (`support/paperclip.rs`): компании, агенты, беседы и комментарии в памяти. Якорь курсора `after` возвращается в ответе, как у живого сервера. `POST` комментария считается по `clientRequestId`: тот же id и тот же текст возвращает существующий комментарий, другой текст даёт `409`. Можно принудительно вернуть ошибку (`Forced`).
- `FakeTelegram` (`support/harness.rs`): записывает отправленные сообщения по темам, создаёт темы с растущим `message_thread_id`, умеет возвращать ошибки Bot API.
- `Harness`: собирает ядро bridge на реальном SQLite и этих заменителях, позволяет прокручивать циклы (`sync`, `outbox`, `drain`) и перезапускать процесс на том же файле состояния (`restart`).
- `ForwardProxy`/`CountingListener` (`support/proxy.rs`): тестовый proxy для проверки, что запросы к Telegram идут только через него.

## Покрытие: ADR-0010 §3.3, обязательные сценарии

Статус: **покрыто** / **слабо** (есть тест, но часть требования проверена косвенно) / **не покрыто**.

| № | Сценарий | Тесты | Статус |
|---|---|---|---|
| 1 | Две компании: обе темы созданы, ответы идут в свои темы | `tests/flow.rs::two_companies_get_their_topics_and_replies`, `tests/flow.rs::several_keys_see_their_companies` | покрыто |
| 2 | Новая беседа появляется на ходу: тема без рестарта | `tests/flow.rs::new_conversation_gets_a_topic_without_restart` | покрыто |
| 3 | Фильтр: уходят ответы агента, квитанции и служебные сообщения с ⚠️; сообщения пользователя нет | `tests/flow.rs::agent_replies_receipts_and_system_notices_go_to_telegram`, `tests/flow.rs::service_notices_are_marked_and_replies_are_not`, `tests/rule.rs::should_forward_table`, `tests/rule.rs::author_types_are_parsed_strictly`, `tests/gaps.rs::own_comment_is_not_echoed_back_to_telegram`, `src/domain/comment.rs::agent_replies_and_receipts_are_forwarded`, `src/domain/comment.rs::user_foreign_and_deleted_comments_are_not` | покрыто. Системные комментарии пересылаются с ⚠️, это решение пользователя (ARCHITECTURE §4.3, «Отличие от ADR-0010 §6.3») |
| 4 | История до обнаружения не пересылается | `tests/flow.rs::history_before_discovery_is_not_forwarded`, `tests/store.rs::conversation_is_inserted_once_with_its_watermark_seen` | покрыто |
| 5 | Сообщение из темы → один POST с ожидаемым `clientRequestId`; повтор `update_id` не даёт второго | `tests/flow.rs::telegram_message_becomes_one_comment`, `tests/gaps.rs::same_text_in_two_messages_gives_two_comments_and_the_same_update_one`, `tests/store.rs::duplicate_update_queues_one_message_and_offset_moves_once`, `src/core/inbound.rs::client_request_id_is_a_stable_uuid_v5`, `tests/paperclip_contract.rs::post_comment_sends_client_request_id_without_run_id` | покрыто |
| 6а | Крах после POST, до коммита `pc_outbox`: после рестарта один комментарий | `tests/crash.rs::crash_after_post_gives_exactly_one_comment` | покрыто (feature `fault-injection`) |
| 6б | Крах после `sendMessage`, до коммита `sent`: дубль допустим, потерь нет | `tests/crash.rs::crash_after_send_repeats_the_piece_without_loss` | покрыто (feature `fault-injection`) |
| 6в | Крах после чтения комментариев, до коммита курсора: ни потерь, ни дублей | `tests/crash.rs::crash_before_cursor_commit_loses_and_duplicates_nothing` | покрыто (feature `fault-injection`) |
| 6г | Рестарт с работой в очереди в обе стороны | `tests/gaps.rs::restart_with_work_pending_in_both_directions_loses_and_repeats_nothing`, `tests/store.rs::in_flight_message_returns_to_pending_after_a_crash` | покрыто |
| 7 | Рестарт: темы не создаются заново, курсоры и offset сохраняются | `tests/flow.rs::restart_keeps_topics_cursor_and_offset`, `tests/store.rs::topics_are_created_lost_and_recreated` | покрыто |
| 8 | Allowlist и чужой чат | `tests/flow.rs::foreign_chat_user_general_and_media_are_not_forwarded`, `src/core/allowlist.rs::admits_listed_users_in_the_group`, `src/core/allowlist.rs::rejects_foreign_chat_and_user`, `src/core/allowlist.rs::rejection_log_is_rate_limited`, `src/core/allowlist.rs::topic_messages_are_parsed` | покрыто |
| 9 | Нарезка и Markdown → HTML; fallback на простой текст | `tests/flow.rs::long_reply_is_split_in_order`, `tests/flow.rs::very_long_reply_is_cut_to_max_message_chunks`, `tests/flow.rs::broken_html_falls_back_to_plain_text`, `tests/telegram_proxy.rs::plain_text_is_sent_without_parse_mode`, `src/render/markdown.rs::inline_markup`, `src/render/markdown.rs::blocks`, `src/render/mod.rs::chunks_fit_the_telegram_limit`, `src/render/mod.rs::long_comment_is_cut_with_a_pointer` | покрыто. Лимит куска 3500 UTF-16 (`render/text.rs: CHUNK_LIMIT`), а не 4096: запас на экранирование и теги |
| 10 | Удалённая тема → пересоздание и доставка; закрытая → reopen | `tests/flow.rs::deleted_topic_is_recreated_and_the_reply_delivered`, `tests/flow.rs::closed_topic_is_reopened`, `tests/gaps.rs::topic_error_texts_end_to_end_on_the_real_classifier_outcomes`, `tests/telegram_errors.rs::topic_calls_use_the_same_classification` | покрыто на mock. Реальные тексты ошибок Telegram вживую не проверены ([paperclip-api.md](paperclip-api.md) §8) |
| 11 | Курсор с якорем не даёт дублей; пустой ответ при сдвинутом `updatedAt` → fallback по `desc` | `tests/flow.rs::anchor_returned_again_gives_no_duplicates_in_both_server_modes`, `tests/flow.rs::vanished_anchor_is_recovered_from_the_newest_page`, `tests/paperclip_contract.rs::comments_cursor_and_latest_queries`, `tests/store.rs::pieces_cursor_and_seen_commit_together_and_keep_order`, `tests/store.rs::seen_purge_keeps_cursor_and_newest` | покрыто |
| 12 | Проверки супергруппы при старте (не форум, нет прав) | `tests/gaps.rs::start_checks_accept_only_a_forum_supergroup_with_an_admin_bot`, `tests/gaps.rs::start_check_errors_split_into_setup_and_try_again`, `tests/e2e_binary.rs::wrong_group_is_an_error_without_exit`, `tests/e2e_binary.rs::start_checks_go_through_the_proxy` | покрыто |

## Покрытие по областям

### Конфигурация и CLI

| Требование | Тесты | Статус |
|---|---|---|
| парсинг конфигурации, значения по умолчанию, длительности | `src/config.rs::parses_minimal_config_with_defaults`, `src/config.rs::optional_values_are_parsed`, `src/config.rs::durations`, `src/config.rs::invalid_values_name_the_variable` | покрыто |
| несколько ключей, список пользователей, взаимоисключение `PAPERCLIP_TOKEN_FILE` и `PAPERCLIP_TOKEN_FILES` | `src/config.rs::several_keys_and_users`, `src/config.rs::key_variables_are_mutually_exclusive` | покрыто |
| `*_FILE` секреты | `src/config.rs::secret_file_trims_trailing_newline`, `src/config.rs::empty_secret_file_is_rejected`, `src/config.rs::missing_secret_file_is_rejected` | покрыто |
| отказ без proxy, неверная схема proxy | `src/config.rs::missing_proxy_is_rejected`, `src/config.rs::proxy_scheme_is_validated_without_echoing_value`, `tests/cli.rs::check_config_fails_without_proxy`, `tests/cli.rs::run_fails_without_proxy_before_any_network` | покрыто |
| секрет не печатается в Debug и логах | `src/config.rs::secrets_are_redacted_in_debug`, `src/config.rs::proxy_debug_hides_credentials`, `src/core/mod.rs::key_ref_has_no_key_material`, `tests/cli.rs::check_config_ok_does_not_print_secrets`, `src/logging.rs::hash_is_short_and_stable` | покрыто |
| переменные v1: `PAPERCLIP_COMPANY_IDS`, `TELEGRAM_USER_ID` отклоняют старт, остальные дают WARN | `src/config.rs::removed_v1_variables_stop_the_start_with_a_hint`, `src/config.rs::obsolete_v1_variables_are_only_reported`, `tests/cli.rs::v1_configuration_is_refused_with_a_migration_hint`, `tests/cli.rs::obsolete_v1_variables_only_warn` | покрыто |
| неизвестная подкоманда | `tests/cli.rs::unknown_subcommand_is_usage_error` | покрыто |
| `revoke-paperclip-key` | `tests/cli.rs::revoke_paperclip_key_calls_revoke_current`, `tests/cli.rs::revoke_failure_is_reported_without_secrets`, `tests/paperclip_contract.rs::revoke_current_key` | покрыто |
| exit-коды `healthcheck` | `tests/cli.rs::healthcheck_exit_codes`, `src/health.rs::unhealthy_cases`, `src/health.rs::fresh_file_is_healthy`, `src/health.rs::grace_after_start_before_first_success` | покрыто |
| heartbeat-файл, чтение файла v1 | `src/health.rs::tracker_snapshot`, `src/health.rs::v1_health_file_is_still_readable` | покрыто |
| признаки unhealthy: ключ отклонён, Agent Chat выключен, группа не прошла проверку, Telegram `403` | `src/health.rs::unhealthy_cases`; сквозные случаи — `tests/flow.rs::rejected_key_is_reported_and_unhealthy`, `tests/flow.rs::agent_chat_disabled_is_reported_once_and_unhealthy` | покрыто. Сквозного теста на `telegram_send_ok=false` и на `telegram_group_ok=false` через сервис нет, оба признака проверены только в `unhealthy_cases` и `gaps::start_check_errors_split_into_setup_and_try_again` |
| `docker run` non-root, read-only FS, нет портов | `scripts/docker-smoke.sh`, шаги 1, 2, 4, 6, 7, 8 | покрыто |
| healthcheck healthy в штатной работе | `scripts/docker-smoke.sh` шаг 5 (только окно «после старта»), `src/health.rs::fresh_file_is_healthy`, `src/health.rs::tracker_snapshot` | слабо: в smoke-test нет живых mock-серверов, поэтому «healthy после успешных вызовов» проверяется только на уровне библиотеки; unhealthy-ветка проверена и в smoke (шаг 9) |

### Клиент Paperclip

| Требование | Тесты | Статус |
|---|---|---|
| `GET /api/health`, версия | `tests/paperclip_contract.rs::health_is_parsed` | покрыто |
| компании: `scope=accessible`, Bearer-токен, архивные отбрасываются | `tests/paperclip_contract.rs::companies_use_scope_accessible_and_bearer_token`, `src/paperclip/dto.rs::companies_are_parsed` | покрыто |
| беседы и агенты; беседа без `conversationAgentId` не берётся | `tests/paperclip_contract.rs::chats_and_agents_are_parsed`, `src/paperclip/dto.rs::chats_require_a_conversation_agent` | покрыто |
| комментарии: курсор, `order`, `limit`, разбор видов | `tests/paperclip_contract.rs::comments_cursor_and_latest_queries`, `src/paperclip/dto.rs::comment_kinds_are_parsed` | покрыто |
| `heartbeat-runs/{id}`: разбираются только `status` и `error` | `src/paperclip/dto.rs::runs_are_parsed`, `tests/notices.rs` | покрыто |
| `POST` комментария с `clientRequestId`, без `X-Paperclip-Run-Id` | `tests/paperclip_contract.rs::post_comment_sends_client_request_id_without_run_id` | покрыто |
| классификация ответов на `POST` (`409` с кодом и без, `403`, `404`, `422`, `5xx`) | `src/core/outbox.rs::answers_are_classified_as_in_the_adr_table`, `tests/paperclip_contract.rs::errors_keep_the_server_text_for_classification` | покрыто |
| `429 Retry-After` | `tests/paperclip_contract.rs::rate_limit_carries_retry_after` | покрыто |
| нечитаемый ответ списка | `tests/paperclip_contract.rs::malformed_lists_are_decode_errors` | покрыто |
| таймаут, недоступный Paperclip, секреты не в ошибках | `tests/paperclip_contract.rs::timeout_is_transient`, `tests/paperclip_contract.rs::unreachable_paperclip_is_transient_and_leaks_nothing` | покрыто |
| путь-префикс в `PAPERCLIP_URL` | `tests/paperclip_contract.rs::base_url_with_path_prefix_is_respected` | покрыто |
| клиент игнорирует `HTTP(S)_PROXY` | `tests/env_proxy.rs::env_proxy_variables_are_ignored` | покрыто |
| ошибки по компаниям: `Agent Chat is disabled`, `401`, `403`/`404` одной компании не останавливают остальные | `tests/flow.rs::agent_chat_disabled_is_reported_once_and_unhealthy`, `tests/flow.rs::rejected_key_is_reported_and_unhealthy`, `tests/flow.rs::several_keys_see_their_companies` | слабо: пропуск компании по `403`/`404` на `/chats` и независимый бэкофф компаний отдельным тестом не проверены |
| ключи разных пользователей: беседы чужого владельца пропускаются | | **не покрыто**: автотеста нет (THREAT_MODEL, строка «ключи разных пользователей») |

### Клиент Telegram, render

| Требование | Тесты | Статус |
|---|---|---|
| классификация ошибок Bot API (в том числе тем) | `src/telegram/http.rs::classification`, `tests/telegram_errors.rs::send_message_errors_are_classified_by_code_and_text`, `tests/telegram_errors.rs::topic_calls_use_the_same_classification`, `tests/telegram_errors.rs::non_json_answers_are_classified_by_status`, `tests/telegram_errors.rs::bad_request_description_is_cut_and_never_holds_the_token` | покрыто |
| все запросы идут через proxy, нет direct fallback | `tests/telegram_proxy.rs::unavailable_proxy_means_zero_direct_requests`, `tests/telegram_proxy.rs::every_request_goes_through_the_proxy`, `tests/telegram_proxy.rs::proxy_is_mandatory_in_the_type` | покрыто |
| env-proxy не влияет на Telegram | `tests/env_proxy.rs::env_proxy_variables_are_ignored` (Telegram-часть внутри `support::env_proxy_telegram_checks`) | покрыто |
| токен не попадает в логи и ошибки | `tests/telegram_proxy.rs::token_is_not_in_errors_or_logs_on_connection_failure` | покрыто |
| HTML-экранирование, длина без тегов | `src/render/text.rs::escapes_html_special_chars`, `src/render/text.rs::visible_length_ignores_tags_and_counts_entities_once`, `tests/gaps.rs::markup_is_escaped_and_not_executed_in_telegram`, `tests/gaps.rs::escaped_characters_count_as_one_and_pieces_do_not_cut_entities` | покрыто |
| суррогатные пары UTF-16, emoji и кириллица | `src/render/text.rs::utf16_counts_surrogate_pairs`, `src/render/text.rs::split_never_breaks_surrogate_pairs`, `tests/gaps.rs::emoji_and_cyrillic_are_counted_in_utf16_and_never_cut_in_half` | покрыто |
| нарезка и лимит куска | `src/render/text.rs::split_prefers_paragraphs_then_lines_then_words`, `src/render/text.rs::split_never_cuts_a_word_when_a_space_follows_the_limit`, `src/render/text.rs::split_pieces_fit_telegram_limit_after_escaping`, `src/render/text.rs::truncation`, `tests/gaps.rs::messages_around_the_limit_fit_telegram_and_keep_every_character` | покрыто |
| Markdown → HTML: разметка, нераспознанные маркеры, блоки, код, баланс тегов | `src/render/markdown.rs::inline_markup`, `src/render/markdown.rs::unmatched_or_ambiguous_markers_stay_literal`, `src/render/markdown.rs::blocks`, `src/render/markdown.rs::markup_inside_code_block_is_not_rendered`, `src/render/markdown.rs::tags_are_balanced_on_every_line`, `tests/gaps.rs::bold_lines_keep_their_tags_balanced_across_pieces` | покрыто |
| блок кода между кусками, обрезка с указателем на Paperclip, пустой комментарий | `src/render/mod.rs::code_fence_is_reopened_across_pieces`, `src/render/mod.rs::long_comment_is_cut_with_a_pointer`, `src/render/mod.rs::empty_comment_has_no_chunks` | покрыто |
| названия тем | `src/render/mod.rs::topic_names` | покрыто |

### Хранилище и рестарт

| Требование | Тесты | Статус |
|---|---|---|
| схема, версия, pragma | `tests/store.rs::schema_is_versioned_and_pragmas_are_set` | покрыто |
| миграция v1 → v2: сохраняется только offset | `tests/store.rs::v1_state_is_migrated_and_offset_kept` | покрыто |
| `integrity_check`, повреждённая БД отвергается | `tests/store.rs::corrupt_database_is_refused` | покрыто |
| беседа вставляется один раз вместе с водяным знаком | `tests/store.rs::conversation_is_inserted_once_with_its_watermark_seen` | покрыто |
| темы: создание, потеря, пересоздание | `tests/store.rs::topics_are_created_lost_and_recreated` | покрыто |
| куски, курсор и `seen_comments` фиксируются одной транзакцией, порядок сохраняется | `tests/store.rs::pieces_cursor_and_seen_commit_together_and_keep_order` | покрыто |
| дубль `update_id`: одно сообщение в очереди, offset сдвигается один раз | `tests/store.rs::duplicate_update_queues_one_message_and_offset_moves_once`, `tests/flow.rs::telegram_message_becomes_one_comment` | покрыто |
| восстановление `in_flight` после краха | `tests/store.rs::in_flight_message_returns_to_pending_after_a_crash` | покрыто |
| очистка текстов после доставки | `tests/store.rs::finished_messages_wipe_the_body` | покрыто |
| очистка `seen_comments`, окно «горячей» беседы | `tests/store.rs::seen_purge_keeps_cursor_and_newest`, `tests/store.rs::hot_window_only_grows` | покрыто |
| расписание повторов | `src/retry.rs::ceiling_grows_exponentially_and_caps`, `src/retry.rs::delay_is_within_bounds`, `src/retry.rs::tiny_base_still_waits_minimum`, `src/retry.rs::jitter_is_at_most_one_second` | покрыто |
| крах-сценарии | см. сценарии 6а–6г выше | покрыто |

### Ядро: потоки данных

| Требование | Тесты | Статус |
|---|---|---|
| обнаружение компаний и бесед, несколько компаний и ключей | `tests/flow.rs::two_companies_get_their_topics_and_replies`, `tests/flow.rs::several_keys_see_their_companies`, `tests/flow.rs::new_conversation_gets_a_topic_without_restart` | покрыто |
| беседа пропала из списка: тема отвязывается | `tests/flow.rs::removed_conversation_unlinks_its_topic` | покрыто |
| ответы агента и квитанции → тема; история не пересылается | `tests/flow.rs::agent_replies_receipts_and_system_notices_go_to_telegram`, `tests/flow.rs::history_before_discovery_is_not_forwarded` | покрыто |
| правило ⚠️: системные комментарии, неуспешный запуск, обычный ответ без пометки | `tests/notices.rs::system_comment_is_marked_without_a_run_lookup`, `tests/notices.rs::failed_run_notice_is_marked_whatever_the_status_or_case`, `tests/notices.rs::ordinary_reply_of_a_failed_run_is_not_marked`, `tests/notices.rs::text_equal_to_the_error_of_a_successful_run_is_not_marked`, `tests/notices.rs::long_multiline_and_run_less_comments_are_never_looked_up`, `tests/notices.rs::notice_and_replies_keep_their_order_in_one_pass`, `tests/rule.rs::is_service_notice_table`, `tests/rule.rs::system_comment_is_a_notice_by_itself`, `tests/rule.rs::pre_check_before_the_run_lookup`, `src/domain/comment.rs::system_notices_are_forwarded`, `src/domain/comment.rs::service_notices_repeat_the_error_of_a_failed_run`, `src/domain/comment.rs::ordinary_replies_are_not_service_notices` | покрыто |
| запуск нельзя прочитать: сообщение приходит без ⚠️ и не теряется | `tests/notices.rs::run_lookup_failure_forwards_without_the_mark_and_loses_nothing` | покрыто |
| текст из темы → один комментарий; не текст, General, тема без беседы → подсказка, в Paperclip ничего не уходит | `tests/flow.rs::telegram_message_becomes_one_comment`, `tests/flow.rs::foreign_chat_user_general_and_media_are_not_forwarded`, `tests/flow.rs::removed_conversation_unlinks_its_topic` | покрыто |
| беседа на паузе (`409 Task is paused`) | `tests/flow.rs::paused_conversation_is_reported_in_the_topic` | покрыто |
| недоступный Paperclip: сообщение в очереди, одно уведомление, доставка позже | `tests/flow.rs::paperclip_outage_keeps_the_message_and_tells_once` | покрыто |
| отклонённое сообщение не блокирует следующие | `tests/gaps.rs::rejected_message_is_not_retried_and_does_not_block_the_next_one` | покрыто |
| ключ отклонён (`401`): одно сообщение в General, unhealthy, снимается успешным вызовом | `tests/flow.rs::rejected_key_is_reported_and_unhealthy` | покрыто |
| Agent Chat выключен | `tests/flow.rs::agent_chat_disabled_is_reported_once_and_unhealthy` | покрыто |
| сообщение bridge в удалённую тему уходит в General | `tests/gaps.rs::service_notice_to_a_deleted_topic_goes_to_general` | покрыто |
| `retry_after` Telegram: очередь на паузе, порядок сохраняется, темы создаются по одной | `tests/gaps.rs::retry_after_on_send_message_keeps_every_piece_in_order`, `tests/gaps.rs::retry_after_on_two_topics_creates_each_once`, `tests/flow.rs::topic_creation_waits_for_retry_after`, `tests/sender.rs::retry_after_pauses_the_queue` | покрыто |
| темп очереди Telegram не выше `TELEGRAM_GROUP_MIN_INTERVAL` | `tests/sender.rs::calls_are_spaced_by_the_group_interval` | покрыто |
| отсутствие эха собственных комментариев | `tests/gaps.rs::own_comment_is_not_echoed_back_to_telegram` | покрыто |

### Остановка и бинарь

| Требование | Тесты | Статус |
|---|---|---|
| SIGTERM во время long poll → exit 0 | `tests/e2e_binary.rs::sigterm_during_long_poll_exits_zero` | покрыто |
| SIGTERM с сообщением в работе → exit 0, без потерь | `tests/e2e_binary.rs::sigterm_with_message_in_flight_finishes_it` | покрыто |
| запросы к Telegram при старте идут через proxy | `tests/e2e_binary.rs::start_checks_go_through_the_proxy` | покрыто |
| Docker smoke-test: non-root, read-only, healthcheck, порты, SIGTERM | `scripts/docker-smoke.sh` | покрыто (см. замечание про healthy выше) |
| Docker smoke-test с mock-серверами в docker network | не делался | слабо: smoke использует закрытые порты вместо mock-серверов; сквозные сценарии с mock Paperclip/Telegram покрыты `tests/e2e_binary.rs` (реальный бинарь) вне контейнера |

## Deployment и версия Paperclip (ADR-0009)

Источник критериев: ADR-0009 (`docs/adr/0009-v1.1.0-improvements.md`), пункты 3–5; они остаются в силе в v2. Тесты Rust выполняет job `test` (`cargo test --all-features`, включая crash-тесты с feature `fault-injection`); скрипты — отдельные job'ы.

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

Ручные проверки, которых CI не заменяет: живой деплой на сервере пользователя (реальный Paperclip, Xray, rootless Docker с `network_mode: host`), живой smoke-тест пробуждения агента комментарием board-ключа (ADR-0010 §1.3), тексты ошибок Telegram для удалённой и закрытой темы, внешний вид тем и сообщений в клиенте Telegram.

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

## Известные ограничения набора

- Тесты таймингов `tests/sender.rs` работают на коротких реальных интервалах, а не на `tokio::time::pause()`. Они проходят в CI, но по принципу остаются потенциально чувствительными к нагрузке раннера.
- Telegram и Paperclip в тестах заменены mock-серверами по документации и коду Paperclip. Живой Paperclip с board-ключом и живой Telegram в набор не входят.
- Нет автотеста на ключи разных пользователей и на пропуск компании по `403`/`404` (см. таблицу клиента Paperclip).
- Контейнерный smoke-test не поднимает mock-серверы; см. строки «слабо» выше.
