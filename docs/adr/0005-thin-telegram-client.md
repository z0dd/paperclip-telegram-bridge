# ADR-0005. Тонкий Telegram-клиент на reqwest с обязательным proxy

- Статус: принято

## Контекст

Telegram доступен только через Xray. Прямой выход в интернет запрещён, fallback недопустим. Токен бота входит в URL Bot API.

## Решение

- Собственный клиент на `reqwest` для шести методов Bot API вместо `teloxide`.
- Единственный конструктор `HttpTelegramClient::new(&TelegramConfig)` всегда ставит `Proxy::all(TELEGRAM_PROXY_URL)`. Тип `ProxyUrl` непустой, без `Option`; без proxy процесс не стартует. Явная настройка proxy отключает чтение `HTTP(S)_PROXY`/`ALL_PROXY` из окружения.
- Клиент Paperclip строится с `.no_proxy()`.
- Ошибки reqwest сводятся к классу (`timeout`, `connect`, …) и никогда не форматируются: URL с токеном не попадает в логи.
- TLS — rustls с провайдером `ring` (`reqwest` без default features, `rustls-no-provider`). `aws-lc-sys` в зависимостях нет.

## Последствия

- Тесты `tests/telegram_proxy.rs` и `tests/env_proxy.rs` доказывают: при недоступном proxy к mock Telegram уходит 0 запросов; через тестовый proxy проходит каждый запрос; env-proxy игнорируется обоими клиентами; токен и пароль proxy не попадают в ошибки и логи.
- Ошибка proxy — временная ошибка (backoff), но не повод менять маршрут.
- Defence in depth на deployment: контейнер подключается только к сетям Paperclip и Xray без маршрута в интернет.
