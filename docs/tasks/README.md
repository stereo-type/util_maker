# Сводный список задач Util Maker

[План развития](../roadmap.md) определяет, для каких результатов предстоит завести карточки. Здесь собраны очереди уже поставленных задач.

По команде «бери следующую задачу» начните с общих задач ниже, затем обходите очереди компонентов в порядке таблицы. В каждой очереди берите первую карточку со статусом 📥 **К выполнению**, зависимости которой допускают начало работы. По команде «бери следующую задачу по `plugins/translator/providers`» проверяйте только указанную очередь.

## Общие задачи

Здесь ведутся задачи со сквозной приёмкой, например проверка всей поставки на чистой Windows. Сейчас открытых общих задач нет. Для первой карточки добавьте строку с названием, ссылкой и статусом.

## Очереди компонентов

| Блок | Задачи и статусы | ТЗ |
| --- | --- | --- |
| `plugins/screenshot` | [Скриншоты](blocks/plugins/screenshot/README.md) | [ТЗ](../blocks/plugins/screenshot/README.md) |
| `plugins/translator` | [Переводчик](blocks/plugins/translator/README.md) | [ТЗ](../blocks/plugins/translator/README.md) |
| `packages` | [Пакеты](blocks/packages/README.md) | [ТЗ](../blocks/packages/README.md) |
| `core` | [Ядро](blocks/core/README.md) | [ТЗ](../blocks/core/README.md) |
| `desktop` | [Desktop-хост](blocks/desktop/README.md) | [ТЗ](../blocks/desktop/README.md) |
| `platform/windows` | [Адаптер Windows](blocks/platform/windows/README.md) | [ТЗ](../blocks/platform/windows/README.md) |
| `sdk` | [SDK и протокол](blocks/sdk/README.md) | [ТЗ](../blocks/sdk/README.md) |
| `plugins/control` | [Контрольный плагин](blocks/plugins/control/README.md) | [ТЗ](../blocks/plugins/control/README.md) |
| `plugins/translator/providers` | [Провайдеры переводчика](blocks/plugins/translator/providers/README.md) | [ТЗ](../blocks/plugins/translator/providers/README.md) |

Все задачи первого этапа, включая прежние общие задачи, сохранены в [очереди ядра](blocks/core/README.md). Результаты ревью прототипа собраны в [протоколе версии 0.4.1](prototype-review.md).

Статусы и зависимости конкретной задачи ведутся в карточке и её очереди. Правила создания карточки, параллельной работы и передачи на ревью описаны в [руководстве](../task-workflow.md).
