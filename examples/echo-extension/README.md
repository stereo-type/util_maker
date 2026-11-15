# Пример плагина-расширения

Пакет `org.example.echo-extension` не объявляет собственных действий, но
предоставляет вклад в точку `echo` родителя `org.example.echo` по контракту
`util-maker.echo` версии `1.0.0`. Он показывает минимальный набор полей
манифеста и обработчик `extension.invoke`.

Сборка и проверка:

```sh
cargo test -p util-maker-example-echo-extension
cargo build -p util-maker-example-echo-extension --bin echo-extension
```

Ручная проверка протокола без установленного родителя выполняется тестом
`answers_echo_operation_without_actions`: он подаёт `initialize`,
`extension.invoke`, обычный `invoke` и `shutdown` через stdio.
