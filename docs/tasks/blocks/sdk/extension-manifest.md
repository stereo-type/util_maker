# Описать контракт плагинов-расширений в манифесте и SDK

**Статус** — 👁️ На ревью

[ТЗ SDK](../../../blocks/sdk/README.md) · [Контракт плагинов-расширений](../../../blocks/core/extensions.md) · [ТЗ ядра](../../../blocks/core/README.md)

Добавить в схему манифеста точки расширения родителя и вклады дочернего
плагина, а в SDK — типы и метод для вызова операции контракта. Сохранить
совместимость обычных микроприложений: манифесты без новых полей и протокол
действий v1 должны читаться без изменений.

**Зависимости для старта** — можно начинать на текущей ревизии.

**Для сквозной проверки** — [Подключать и вызывать плагины-расширения](../core/nested-extensions.md), [Устанавливать пакеты-расширения](../packages/extension-packages.md).

## Критерии приёмки

- [x] `Manifest` принимает необязательные `extensionPoints` и `provides`. Для родителя проверяются уникальные локальные ID, semver-версия контракта, `selection: single` и наличие возможностей `extensions.read`/`extensions.call`.
- [x] Для плагина проверяются `moduleId + pointId`, совпадение `contract`, совместимость `interfaceVersion`, непустое отображаемое имя и не более одного вклада в одну точку.
- [x] Пакет с `provides` может не иметь `actions`; обычное микроприложение без действий по-прежнему отклоняется. Пакет не может одновременно объявлять `extensionPoints` и `provides`.
- [x] Контракт расширений входит в `hostApi` `1.1.0`; пакеты с `extensionPoints` или `provides` требуют `>=1.1.0 <2.0.0`, а старый хост отклоняет их до запуска.
- [x] Новые машинные коды `EXTENSION_NOT_FOUND`, `EXTENSION_INCOMPATIBLE`, `EXTENSION_NOT_SELECTED`, `EXTENSION_FAILED` и типы `ExtensionInvokeRequest`/`ExtensionInvokeResult` доступны из SDK.
- [x] `Module::extension_invoke` вызывается по JSON-RPC-методу `extension.invoke`; модуль без нового метода получает `METHOD_NOT_FOUND`, старый `invoke` продолжает работать.
- [x] В `examples/` есть эталонный плагин-расширение с манифестом, обработчиком операции и инструкцией сборки; без родителя он проверяется тестовым хостом.
- [x] Неизвестные необязательные поля нового контракта игнорируются в пределах major-версии, несовместимый major отклоняется до запуска процесса.

## Что сделано

- В `Manifest` добавлены необязательные `extensionPoints` и `provides`; проверяются ID, semver контракта, `selection: single`, совместимость `interfaceVersion` и отсутствие вложенности.
- Пакет с `provides` может не иметь `actions`; пакет с `extensionPoints` обязан объявить `extensions.read` и `extensions.call`, а пакеты с расширениями требуют `hostApi >=1.1.0 <2.0.0`.
- `HOST_API_VERSION` поднят до `1.1.0`; добавлены коды `EXTENSION_*` и типы `ExtensionInvokeRequest`/`ExtensionInvokeResult`, `ExtensionListResult`.
- `Module::extension_invoke` обслуживается методом `extension.invoke`; модуль без метода получает `METHOD_NOT_FOUND`, обычный `invoke` не изменился.
- Добавлен пример `examples/echo-extension` с манифестом, обработчиком операции и тестом через `run_stdio`.

## Проверка

Linux: `make check`, тесты `util-maker-sdk` на разбор и отказ манифестов,
round-trip JSON-RPC `extension.invoke`, запуск эталонного плагина через
`run_stdio` с тестовым хостом. Сборка frontend для этой карточки не требуется.

Windows: ручная проверка не нужна, если эталонный плагин и протокол проверены
через stdio на Linux.
