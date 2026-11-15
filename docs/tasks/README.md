# Сводный список задач Util Maker

[План развития](../roadmap.md) определяет, для каких результатов предстоит завести карточки. Здесь собраны очереди уже поставленных задач.

Количество незавершённых задач и задач на ревью по каждой очереди смотрите в [сводке по разделам](open-tasks.md).

Результаты проверки 0.4.5 — [windows-0.4.5-findings.md](windows-0.4.5-findings.md).

Единый чек-лист приёмки Windows — [windows-acceptance.md](windows-acceptance.md).

План ручной проверки оставшихся задач — [windows-review-plan.md](windows-review-plan.md).

По команде «бери следующую задачу» начните с общих задач ниже, затем обходите очереди компонентов в порядке таблицы. В каждой очереди берите первую карточку со статусом 📥 **К выполнению**, зависимости которой допускают начало работы. По команде «бери следующую задачу по `plugins/translator/providers`» проверяйте только указанную очередь.

### С чего начать параллельную работу

Для расширения штатной поставки три агента могут независимо взять [распознавание текста](blocks/plugins/screen-ocr/recognize-crop.md), [кодирование QR](blocks/plugins/qr-code/encode-text.md) и [чтение цвета пикселя](blocks/plugins/color-picker/sample-pixel.md). Для старта достаточно эталонных данных. Параллельно в очереди SDK доступна [передача пикселей дочернему процессу](blocks/sdk/plugin-frame-access.md). Она понадобится для проверки всех трёх плагинов на реальном рабочем столе.

Перед назначением сверяйте актуальную строку в очереди. Раздел «Зависимости для старта» указывает, какие результаты уже должны быть доступны в checkout агента. Раздел «Для сквозной проверки» указывает, какие реализации понадобятся позже, чтобы подтвердить работу компонентов вместе.

## Общие задачи

Обе общие задачи относятся к этапу 4. Перед началом проверьте зависимости в карточке. Пока обязательные результаты недоступны, переходите к очередям компонентов.

| Задача | Статус |
| --- | --- |
| [Включить два штатных пакета в установщик Windows](windows-distribution.md) | ✅ Выполнена |
| [Принять первую поставку по 11 сценариям Windows](windows-release-acceptance.md) | ✅ Выполнена |

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
| `plugins/screen-ocr` | [Текст с экрана](blocks/plugins/screen-ocr/README.md) | [ТЗ](../blocks/plugins/screen-ocr/README.md) |
| `plugins/qr-code` | [QR-коды](blocks/plugins/qr-code/README.md) | [ТЗ](../blocks/plugins/qr-code/README.md) |
| `plugins/color-picker` | [Пипетка цвета](blocks/plugins/color-picker/README.md) | [ТЗ](../blocks/plugins/color-picker/README.md) |

Все задачи первого этапа, включая прежние общие задачи, сохранены в [очереди ядра](blocks/core/README.md). Результаты ревью прототипа собраны в [протоколе версии 0.4.1](prototype-review.md).

Статусы и зависимости конкретной задачи ведутся в карточке и её очереди. Правила создания карточки, параллельной работы и передачи на ревью описаны в [руководстве](../task-workflow.md).
