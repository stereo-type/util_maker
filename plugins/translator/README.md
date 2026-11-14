# Плагин «Переводчик»

Будущее микроприложение перевода: состояния выбора текста до и после шорткея и
HTTP-адаптеры Яндекса и Google внутри одного каталога плагина.

- [`src/`](src/lib.rs) — `SelectionController` и адаптеры источника/приёмника.
- [`providers/`](providers/src/lib.rs) — профили, запросы, ошибки и разбор
  ответов обоих сервисов.
- Крейты — `util-maker-translator`, `util-maker-providers`.
- Проверка — `make test`.

Процесс, UI Automation и окно результата ещё не упакованы. Настройка ключей —
[инструкция](../../docs/blocks/plugins/translator/providers/providers-setup.md).
