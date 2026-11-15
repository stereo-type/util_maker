# Пример стороннего плагина Echo

Переносимый пример для сценария 11 приёмки: новый ID `com.example.echo`,
издатель `com.example` и два действия — `hello` и `clock`. Модуль использует
только публичный `util-maker-sdk`, поэтому исходники хоста не меняются.

| Действие | Что делает | Результат |
| --- | --- | --- |
| `hello` | Возвращает приветствие | `callId`, источник вызова, версия модуля |
| `clock` | Читает системное время процесса | `callId`, `unixMs`, PID |

## Сборка и подпись

Из корня репозитория на Ubuntu с Docker:

```sh
make dev-setup
make example-package
```

Цель собирает `echo-plugin`, создаёт тестовый ключ издателя, экспортирует и
проверяет подписанный архив. Результат лежит в `dist/example-plugin/`:

| Файл | Назначение |
| --- | --- |
| `echo-plugin-0.1.0-linux-x64.umpkg` | Подписанный пакет для проверки |
| `keys/com.example-public.json` | Открытый ключ для добавления в доверенные |
| `keys/com.example-private.json` | Закрытый ключ; в пакет и журнал не попадает |
| `verify.json` | Результат проверки подписи, хешей и точки входа |
| `actions.jsonl` | Ответы IPC на `hello` и `clock` |

Для другой версии:

```sh
make example-package EXAMPLE_VERSION=0.2.0
```

Версия попадает в манифест пакета и в ответ `hello.moduleVersion`. ID действий
не меняются, поэтому привязки сочетаний сохраняются при обновлении.

## Установка и обновление

1. Соберите пакет командами выше и передайте пользователю `.umpkg` и
   `com.example-public.json`.
2. В настройках в разделе «Установка пакета» укажите путь к `.umpkg`, издателя
   `com.example` и открытый ключ из `com.example-public.json` (64 hex-символа).
   Нажмите «Проверить», сверьте отпечаток, отметьте подтверждение и нажмите
   «Установить».
3. Назначьте разные сочетания на `hello` и `clock`.
4. Для обновления соберите пакет с `EXAMPLE_VERSION=0.2.0` и установите его
   поверх версии `0.1.0`. Оба ID действий сохраняют назначения.

Закрытый ключ остаётся на машине разработчика. В репозиторий и пакет он не
попадает; `make example-package` кладёт его только в `dist/`, который исключён
из git через `dist/`.

## Ручная проверка

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"hostApi":"1.0.0","moduleId":"com.example.echo"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"invoke","params":{"callId":"h","actionId":"hello","context":{"source":"tray"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"invoke","params":{"callId":"c","actionId":"clock","context":{"source":"hotkey"}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"shutdown","params":null}' \
  | target/example-plugin/release/echo-plugin
```

Ожидаемо: три ответа с `status: ok` (для `hello` и `clock`), затем
`{"stopped":true}`. Оба действия обслуживаются одним процессом до `shutdown`.

## Windows

Для сценария 11 соберите `.exe` и упакуйте его с платформой `windows-x64`.
Нужен Rust с target `x86_64-pc-windows-msvc` либо кросс-сборка в контейнере
`docker/windows` (как в `make build-windows`).

```powershell
$version = "0.1.0"

# ключ создаётся один раз; повторный запуск не перезаписывает его
if (-not (Test-Path dist/example-plugin/keys/com.example-private.json)) {
  cargo run -p util-maker-packages --bin util-maker-package -- keygen `
    --publisher com.example `
    --private-key dist/example-plugin/keys/com.example-private.json `
    --public-key dist/example-plugin/keys/com.example-public.json
}

$env:UTIL_MAKER_MODULE_VERSION = $version
cargo build --release -p util-maker-example-echo --target x86_64-pc-windows-msvc

$stage = "dist/example-plugin/windows-stage-$version"
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target/x86_64-pc-windows-msvc/release/echo-plugin.exe $stage

cargo run -p util-maker-packages --bin util-maker-package -- export `
  --source $stage `
  --manifest examples/echo-plugin/manifest.json `
  --output "dist/example-plugin/echo-plugin-$version-windows-x64.umpkg" `
  --private-key dist/example-plugin/keys/com.example-private.json `
  --version $version `
  --platform windows-x64
```

`UTIL_MAKER_MODULE_VERSION` попадает в ответ `hello.moduleVersion`; без него
`hello` вернёт версию crate `0.1.0`.

Из Ubuntu без Rust на Windows тот же `.exe` собирается в контейнере
`docker/windows`:

```sh
version=0.1.0
docker run --rm --user "$(id -u):$(id -g)" \
  --mount "type=bind,source=$PWD,target=/workspace" \
  --env HOME=/workspace/.cache/windows-build/home \
  --env CARGO_HOME=/workspace/.cache/windows-build/cargo \
  --env CARGO_TARGET_DIR=/workspace/.cache/windows-build/target \
  --env XWIN_CACHE_DIR=/workspace/.cache/windows-build/xwin \
  --env UTIL_MAKER_MODULE_VERSION=$version \
  util-maker-windows:local sh -ec \
  "cd /workspace && cargo xwin build --release --target x86_64-pc-windows-msvc -p util-maker-example-echo --bin echo-plugin"
```

Установите пакет в разделе «Установка пакета»: путь к `.umpkg`, издатель
`com.example`, открытый ключ из `com.example-public.json`. Назначьте `hello` и
`clock` разные сочетания и проверьте оба результата. Для 0.2.0 повторите с
`$version = "0.2.0"`: ID действий и назначения сохранятся.
