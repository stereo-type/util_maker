# Протокол первой поставки Windows

Версия: **0.4.23-dev**. Установщик: `dist/UtilMaker-Setup-0.4.23-dev.exe`.
Пакеты: `org.utilmaker.screenshot-0.4.23-windows-x64.umpkg`,
`org.utilmaker.translator-0.4.23-windows-x64.umpkg`.
Стенд: Windows 11 x64, два монитора 2560×1440 слева и 3440×1440 основной.

## Сценарии

| № | Сценарий | Linux | Windows | Источник |
| --- | --- | --- | --- | --- |
| 01 | Установка, первый запуск, три микроприложения | — | Частично: 0.4.23 запускается, настройки открываются | журнал `logs/util-maker.log` |
| 02 | Менеджер пакетов: проверка, установка, откат, удаление | `cargo test -p util-maker-packages` — 23/23; CLI install/list/rollback/activate/remove | Ожидает | — |
| 03 | Сочетания: изменение, второе, конфликт, пауза, перезапуск | `persistent_shortcuts` — 4/4 | Частично: `Ctrl+Shift+S/U/T` зарегистрированы | `shortcuts.json` |
| 04 | Трей и карточки | — | Ожидает | — |
| 05 | Скриншоты: выбор, редактор, копирование, Esc | `screenshot-check` | Частично: выбор без затемнения, редактор, Esc, PNG-сохранение | снимки 0.4.23 |
| 06 | Переводчик: два профиля, ключи, ошибки | провайдеры | Ожидает ключей Яндекса/Google | — |
| 07 | Настройки по схеме | `cargo test -p util-maker-platform` — 23/23 | Частично: форма, секрет вне JSON | — |
| 08 | Сбой плагина | `failure_isolation` — 5/5 | Частично: `kill-plugin.ps1` даёт `module_crashed` | журнал |
| 09 | Метрики: 50 холодных вызовов, p95 | `action_metrics` — 2/2 | 50 вызовов с `-Hotkey Ctrl+Shift+9`; p95 по control | `util-maker.jsonl` |
| 10 | Сон, разблокировка, Explorer | — | Ожидает | — |
| 11 | Клавиатура, темы, масштабы | — | Проверено пользователем | `accessible-settings` |

## Linux-проверки

- `make test` — все тесты проходят;
- `cargo test -p util-maker-packages` — 23/23;
- `cargo test -p util-maker-platform` — 23/23;
- `cargo test -p util-maker-core --test failure_isolation` — 5/5;
- `cargo test -p util-maker-core --test action_metrics` — 2/2.

## Открытые пункты

- 02: повторная установка, откат и удаление на чистом профиле Windows.
- 04: иконки трея, исчезновение/возврат пункта при отключении.
- 06: реальный перевод обоих профилей, ошибки провайдера.
- 08: кнопка «Отключить плагин» из сообщения о сбое.
- 10: сон/разблокировка/перезапуск Explorer.
- 01: чистая установка без прошлого профиля (clean install).
