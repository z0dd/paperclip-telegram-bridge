# ADR-0007. Healthcheck через heartbeat-файл

- Статус: принято

## Контекст

У bridge нет входящих портов и HTTP-listener, а runtime-образ distroless без shell.

## Решение

- Задача `health_writer` раз в 15 секунд атомарно (temp + fsync + rename) пишет `HEALTH_FILE` (по умолчанию `/state/health.json`): `pid`, `started_at`, `written_at`, `last_telegram_ok_at`, `last_paperclip_ok_at`, `paperclip_auth_ok`.
- `bridge healthcheck` (`HEALTHCHECK CMD ["/usr/local/bin/bridge","healthcheck"]`) не делает сетевых запросов. Exit 0, если файл свежее 60 секунд, ключ Paperclip не отклонён и оба `last_*_ok_at` не старше `HEALTH_MAX_STALE` (до первого успеха отсчёт идёт от старта). Иначе exit 1 и причина в stdout без секретов.

## Последствия

- Недоступный Telegram/Xray или Paperclip дольше `HEALTH_MAX_STALE` делает контейнер unhealthy, но bridge продолжает ретраить.
- `start-period` HEALTHCHECK в Dockerfile — 30 секунд.
