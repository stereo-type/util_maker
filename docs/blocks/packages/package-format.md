# Формат пакета `.umpkg`

Пакет — ZIP-архив без вложенного каталога:

```text
manifest.json
signature.ed25519
bin/control-module.exe
assets/icon.png
...
```

## Манифест

`manifest.json` — UTF-8 JSON из [контракта микроприложения](../sdk/README.md).
Дополнительно к обязательным полям он перечисляет файлы полезной нагрузки:

```json
{
  "manifestVersion": 1,
  "id": "org.utilmaker.control",
  "publisher": "org.utilmaker",
  "name": "Контрольный модуль",
  "version": "0.2.0",
  "hostApi": ">=1.0.0 <2.0.0",
  "entrypoints": { "windows-x64": "bin/control-module.exe" },
  "capabilities": [],
  "actions": [{ "id": "ping", "name": "Проверить связь" }],
  "files": [
    { "path": "bin/control-module.exe", "sha256": "<64 hex>" }
  ]
}
```

`files` содержит каждый файл архива, кроме `manifest.json` и
`signature.ed25519`. Точка входа платформы обязана быть в этом списке.

## Подпись

`signature.ed25519` — отсоединённая подпись Ed25519 (64 байта) ровно над
байтами `manifest.json`. Открытый ключ издателя хранится в доверенном наборе
хоста и сопоставляется с полем `publisher`; обновление принимается только тем
же ключом.

## Проверки до активации

1. ZIP читается, манифест и подпись присутствуют.
2. Манифест проходит схему и проверку относительных путей.
3. Издатель есть в доверенном наборе, подпись манифеста верна.
4. Для целевой платформы есть точка входа.
5. Диапазон `hostApi` совместим с текущим хостом.
6. Каждый файл из `files` присутствует, его SHA-256 совпадает; лишних файлов
   нет.
7. Абсолютные пути, `..`, обратные слэши, буквы диска и символические ссылки
   отклоняются.

Проверка ничего не распаковывает в каталог установки и не запускает модуль.
`util-maker-packages` возвращает `PackagePreview` для показа пользователю;
активная версия при отказе не меняется.

## Экспорт

Подготовьте каталог только с файлами полезной нагрузки: собранной точкой входа,
иконкой и другими ресурсами. `manifest.json` и `signature.ed25519` создаются
экспортёром заново; файлы с такими именами в корне исходного каталога
игнорируются. Манифест-основа может лежать отдельно.

Закрытый ключ создаётся один раз для издателя и хранится вне исходного
каталога, например в `dist/keys`. Не коммитьте его и не передавайте вместе с
пакетом. Экспорт отклоняет ключ внутри `PACKAGE_SOURCE` и выходной файл,
совпадающий с ключом. В журнал и JSON-результат попадают только путь пакета,
SHA-256, ID, версия, точка входа и размеры.

Новый ключ создаётся командой `util-maker-package keygen`; в dev-контейнере:

```sh
make dev-run CMD="cargo run --quiet -p util-maker-packages --bin util-maker-package -- \
  keygen --publisher org.utilmaker \
  --private-key dist/keys/org.utilmaker-private.json \
  --public-key dist/keys/org.utilmaker-public.json"
```

Сборка через Makefile из корня репозитория:

```sh
make package-export \
  PACKAGE_SOURCE=dist/control-stage \
  PACKAGE_MANIFEST=plugins/control/manifest.json \
  PACKAGE_OUTPUT=dist/control-0.4.1-linux-x64.umpkg \
  PACKAGE_PRIVATE_KEY=dist/keys/org.utilmaker-private.json \
  PACKAGE_VERSION=0.4.1 \
  PACKAGE_PLATFORM=linux-x64
```

- `PACKAGE_SOURCE` — каталог полезной нагрузки; файлы вне его, включая
  символические ссылки, отклоняются.
- `PACKAGE_MANIFEST` — исходный `manifest.json`; поле `files` пересчитывается.
- `PACKAGE_OUTPUT` — итоговый `.umpkg`; не должен лежать внутри источника.
- `PACKAGE_PRIVATE_KEY` — JSON закрытого ключа издателя из `keygen`.
- `PACKAGE_VERSION` и `PACKAGE_PLATFORM` необязательны: первый заменяет версию
  в копии манифеста, второй оставляет только одну точку входа.

Ту же операцию можно выполнить без Makefile через
`util-maker-package export --source … --manifest … --output … --private-key …`.
Для тестовых ключей и готовых образцов используйте `make control-samples` или
`make example-package`.

Экспорт хеширует каждый файл, записывает SHA-256 в `files`, подписывает итоговые
байты манифеста Ed25519 и проверяет собранный архив тем же кодом, что и
установка. Запись идёт через временный файл с атомарной заменой: при ошибке
прежний выходной файл остаётся на месте и продолжает проходить проверку.

## Установка в профиль

`util-maker-packages` читает пакет один раз, проверяет подпись, издателя,
диапазон API, платформу, пути и SHA-256 и распаковывает ровно те байты, которые
проверил. Профиль пользователя устроен так:

```text
<профиль>/installed.json
<профиль>/<id>/<версия>/manifest.json
<профиль>/<id>/<версия>/signature.ed25519
<профиль>/<id>/<версия>/bin/...
```

`installed.json` хранит активную версию, издателя, состояние включения и
SHA-256 исходного пакета. Содержимое версии сначала пишется во временный
каталог, затем переименовывается, и только после этого атомарно обновляется
индекс. Ошибка проверки, распаковки или записи индекса не публикует частичную
установку. Повторная установка тех же байтов возвращает существующую запись;
пакет с тем же ID от другого издателя отклоняется.

```sh
make package-install \
  PACKAGE_OUTPUT=dist/control-0.4.3-linux-x64.umpkg \
  PACKAGE_PUBLIC_KEY=dist/keys/org.utilmaker-public.json \
  PACKAGE_PROFILE=dist/profile \
  PACKAGE_PLATFORM=linux-x64
```

Без Makefile:

```sh
util-maker-package install --package dist/control-0.4.3-linux-x64.umpkg \
  --public-key dist/keys/org.utilmaker-public.json --profile dist/profile \
  --platform linux-x64
util-maker-package list --profile dist/profile
```

Повторное открытие профиля тем же `PackageStore` возвращает манифест, активную
версию, состояние и путь к исполняемому файлу, не запуская модуль.
