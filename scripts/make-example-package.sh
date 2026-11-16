#!/bin/sh
# Собирает и подписывает пример стороннего плагина с новым ID и двумя
# действиями. Работает внутри dev-контейнера из `make example-package`.
set -eu

root=/workspace
samples=${EXAMPLE_PACKAGE_DIR:-dist/example-plugin}
publisher=${EXAMPLE_PUBLISHER:-com.example}
version=${EXAMPLE_VERSION:-0.1.0}
platform=${EXAMPLE_PLATFORM:-linux-x64}
target_dir=${EXAMPLE_TARGET_DIR:-$root/target/example-plugin}

case "$samples" in
    /*) ;;
    *) samples="$root/$samples" ;;
esac
case "$samples" in
    "$root"/dist/*) ;;
    *)
        echo "Пример разрешено собирать только внутри $root/dist: $samples" >&2
        exit 2
        ;;
esac

case "$version" in
    *[!A-Za-z0-9._-]*)
        echo "Недопустимая версия примера: $version" >&2
        exit 2
        ;;
esac
if [ -z "$version" ]; then
    echo "EXAMPLE_VERSION не должен быть пустым" >&2
    exit 2
fi

mkdir -p "$samples/keys"
rm -rf "$samples/.staging"
rm -f "$samples"/*.umpkg "$samples"/*.json "$samples"/*.jsonl "$samples"/*.stderr

private_key="$samples/keys/$publisher-private.json"
public_key="$samples/keys/$publisher-public.json"
if [ -f "$private_key" ] && [ -f "$public_key" ]; then
    echo "Ключ издателя $publisher уже создан, использую его"
elif [ -e "$private_key" ] || [ -e "$public_key" ]; then
    echo "Найден только один файл ключа в $samples/keys; удалите каталог и повторите" >&2
    exit 1
else
    echo "Создание тестового ключа издателя $publisher"
    cargo run --quiet -p util-maker-packages --bin util-maker-package -- keygen \
        --publisher "$publisher" \
        --private-key "$private_key" \
        --public-key "$public_key"
fi

echo "Сборка примера echo-plugin"
UTIL_MAKER_MODULE_VERSION="$version" cargo build --quiet --release --target-dir "$target_dir" -p util-maker-example-echo
binary="$target_dir/release/echo-plugin"
if [ ! -f "$binary" ]; then
    echo "Не найден процесс примера: $binary" >&2
    exit 1
fi

stage="$samples/.staging"
mkdir -p "$stage"
cp "$binary" "$stage/echo-plugin"
chmod 0755 "$stage/echo-plugin"

package="$samples/echo-plugin-$version-$platform.umpkg"
echo "Экспорт $package"
cargo run --quiet -p util-maker-packages --bin util-maker-package -- export \
    --source "$stage" \
    --manifest "$root/examples/echo-plugin/manifest.json" \
    --output "$package" \
    --private-key "$private_key" \
    --version "$version" \
    --platform "$platform" > "$samples/export.json"

cargo run --quiet -p util-maker-packages --bin util-maker-package -- verify \
    --package "$package" \
    --public-key "$public_key" \
    --platform "$platform" > "$samples/verify.json"

require_line() {
    pattern=$1
    file=$2
    if ! grep -F "$pattern" "$file" >/dev/null; then
        echo "Не найдено «$pattern» в $file" >&2
        exit 1
    fi
}
require_line '"id": "com.example.echo"' "$samples/verify.json"
require_line '"id": "hello"' "$samples/verify.json"
require_line '"id": "clock"' "$samples/verify.json"
require_line "\"version\": \"$version\"" "$samples/verify.json"
require_line '"entrypoint": "echo-plugin"' "$samples/verify.json"

printf '%s\n' \
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"hostApi":"1.0.0","moduleId":"com.example.echo"}}' \
    '{"jsonrpc":"2.0","id":2,"method":"invoke","params":{"callId":"example-hello","actionId":"hello","context":{"source":"tray"}}}' \
    '{"jsonrpc":"2.0","id":3,"method":"invoke","params":{"callId":"example-clock","actionId":"clock","context":{"source":"hotkey"}}}' \
    '{"jsonrpc":"2.0","id":4,"method":"shutdown","params":null}' \
    | "$stage/echo-plugin" > "$samples/actions.jsonl" 2> "$samples/actions.stderr"

require_line '"callId":"example-hello"' "$samples/actions.jsonl"
require_line '"status":"ok"' "$samples/actions.jsonl"
require_line "\"moduleVersion\":\"$version\"" "$samples/actions.jsonl"
require_line '"callId":"example-clock"' "$samples/actions.jsonl"
require_line '"unixMs"' "$samples/actions.jsonl"
require_line '"stopped":true' "$samples/actions.jsonl"

rm -rf "$stage"

echo
echo "Пакет: $package"
echo "Открытый ключ: $public_key"
echo "Проверка: $samples/verify.json"
echo "Ответы действий: $samples/actions.jsonl"
