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
2. Добавьте открытый ключ `com.example` в доверенные через будущий экран
   настроек. Сейчас установка из UI ещё не реализована; проверка архива
   выполняется командой `util-maker-package verify`.
3. Установите пакет, назначьте разные сочетания на `hello` и `clock`.
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

Для проверки сценария 11 на Windows соберите пример и упакуйте его с
платформой `windows-x64`:

```powershell
cargo build --release -p util-maker-example-echo --target x86_64-pc-windows-msvc
$stage = "dist/example-plugin/windows-stage"
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target/x86_64-pc-windows-msvc/release/echo-plugin.exe $stage
cargo run -p util-maker-packages --bin util-maker-package -- export `
  --source $stage `
  --manifest examples/echo-plugin/manifest.json `
  --output dist/example-plugin/echo-plugin-0.1.0-windows-x64.umpkg `
  --private-key dist/example-plugin/keys/com.example-private.json `
  --platform windows-x64
```

Установите пакет через UI, добавьте открытый ключ, назначьте `hello` и `clock`
разные сочетания и проверьте оба результата. Затем повторите с версией `0.2.0`
и убедитесь, что назначения сохранились.
