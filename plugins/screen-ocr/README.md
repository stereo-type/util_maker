# Текст с экрана

Плагин `org.utilmaker.screen-ocr` распознаёт текст в выбранной области снимка
экрана. Переносимая часть обрезает BGRA8-кадр с учётом `origin` и `stride`,
переводит фрагмент в оттенки серого и запускает локальный Tesseract без
временных файлов.

- Крейт — `util-maker-screen-ocr`;
- процесс — `screen-ocr-module`, действие — `recognize-region`;
- манифест и настройка «Языки распознавания» — в [`manifest.json`](manifest.json);
- режимы — `rus+eng` (по умолчанию), `rus`, `eng`;
- эталон — [`reference/sample.png`](reference/sample.png) и [`sample.json`](reference/sample.json).

## Процесс и манифест

`manifest.json` объявляет издателя `org.utilmaker`, возможности
`screen.capture`, `settings.read`, `clipboard.write` и действие
`recognize-region`. Языки читаются через `settings.read`; неизвестное или
отсутствующее значение заменяется режимом «Русский и английский».

Процесс запрашивает один кадр, читает локальный файл пикселей через
`FrameRef::read_pixels`, копирует байты в память и сразу вызывает
`frame.release`. Если файл пикселей уже истёк или недоступен, действие
возвращает код `2002` с причиной `frame-bytes-unavailable`; следующий вызов
может запросить новый снимок.

## Выбор области

`region.rs` содержит переносимое состояние выбора:

- `RegionFrame` — владеющая копия BGRA8 с геометрией мониторов и `expiresInMs`;
- `RegionPicker::start` открывает одну сессию; повторный вызов возвращает
  `AlreadyActive` и не делает новый захват;
- `RegionSession::begin` / `move_to` / `set_selection` нормализуют
  прямоугольник в любом направлении и прижимают указатель к краям кадра;
- вырожденный прямоугольник оставляет фазу `Selecting`;
- `RegionPicker::cancel` закрывает выбор по Esc и освобождает копию кадра;
- истечение `expiresInMs` не влияет на выбор: он идёт по сохранённым пикселям.

Чтобы прогнать выбор через IPC, действие принимает необязательные `params`:
`anchor` и `cursor` (точки `{x, y}`), `selection` (`{x, y, width, height}`) или
`cancel: true`. Без `params` процесс только фиксирует кадр и возвращает
`awaiting-selection`.

## Проверка

```sh
make screen-ocr-check
make test
```

`make screen-ocr-check` проверяет обрезку, эталонный Tesseract, выбор области и
IPC-границу процесса. `make test` запускает весь workspace.

## Осталось

Оконный overlay и окно редактирования результата подключаются задачами
[«Редактировать результат OCR»](../../docs/tasks/blocks/plugins/screen-ocr/edit-result.md)
и [«Поставлять OCR-пакет»](../../docs/tasks/blocks/plugins/screen-ocr/ship-package.md).
