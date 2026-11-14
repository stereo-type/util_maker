# Плагин «Переводчик»

Микроприложение перевода: состояния выбора текста до и после шорткея и
HTTP-адаптеры Яндекса и Google внутри одного каталога плагина.

- [`src/lib.rs`](src/lib.rs) — `SelectionController` и адаптеры источника/приёмника.
- [`src/main.rs`](src/main.rs) — процесс `translator-module` и действие `translate`.
- [`providers/`](providers/src/lib.rs) — профили, запросы, ошибки и разбор
  ответов обоих сервисов.
- Крейты — `util-maker-translator`, `util-maker-providers`.
- Манифест — [`manifest.json`](manifest.json).

## Контракт процесса

| Поле | Значение |
| --- | --- |
| ID модуля | `org.utilmaker.translator` |
| Действие | `translate` — «Перевести текст» |
| Точки входа | `translator-module.exe` для `windows-x64`, `translator-module` для `linux-x64` |
| Возможности | пока нет; доступ к выделению добавит задача UI Automation |

Процесс подтверждает ID на `initialize`, принимает `invoke` через stdin/stdout и
штатно завершается по `shutdown`. Обработчик использует существующий
`SelectionController`. Пока снимок источника не подключён к UI Automation,
действие возвращает `outcome = "blocked"`, `reason = "context-lost"` и
`httpRequests = 0`: провайдер не получает текст и сетевой запрос не возникает.
Неизвестное действие возвращает `ACTION_NOT_FOUND`.

## Сборка и проверка на Linux

Из корня репозитория:

```sh
make test
```

Цель собирает workspace, включая процесс переводчика, и запускает
`crates/core/tests/translator_module_ipc.rs`. Тест проверяет манифест,
рукопожатие, явную причину отсутствия источника, нулевое число HTTP-запросов,
`ACTION_NOT_FOUND` и завершение по `shutdown`. Для точечного запуска:

```sh
make dev-run CMD="cargo test -p util-maker-core --test translator_module_ipc"
```

Локальная сборка только процесса:

```sh
make dev-run CMD="cargo build -p util-maker-translator --bin translator-module"
```

UI Automation, окно результата и проверка реальными ключами относятся к
следующим задачам. Настройка ключей —
[инструкция](../../docs/blocks/plugins/translator/providers/providers-setup.md).
