#!/bin/sh
# Собирает два подписанных контрольных пакета разных версий для проверки
# обновления. Запускается внутри dev-контейнера из `make control-samples`.
set -eu

root=/workspace
samples=${CONTROL_SAMPLES_DIR:-dist/control-samples}
versions=${CONTROL_SAMPLE_VERSIONS:-"0.4.1 0.5.0"}
publisher=${CONTROL_SAMPLE_PUBLISHER:-org.utilmaker}
platform=${CONTROL_SAMPLE_PLATFORM:-linux-x64}
target_dir=${CONTROL_SAMPLE_TARGET_DIR:-$root/target/control-samples}

case "$samples" in
    /*) ;;
    *) samples="$root/$samples" ;;
esac
case "$samples" in
    "$root"/dist/*) ;;
    *)
        echo "Образцы разрешено создавать только внутри $root/dist: $samples" >&2
        exit 2
        ;;
esac

if [ -z "$versions" ]; then
    echo "CONTROL_SAMPLE_VERSIONS не должен быть пустым" >&2
    exit 2
fi

mkdir -p "$samples/keys"
rm -rf "$samples/.staging"
rm -f "$samples"/*.umpkg "$samples"/*.json "$samples"/*.jsonl \
    "$samples"/*.stderr "$samples/SHA256SUMS"
private_key="$samples/keys/$publisher-private.json"
public_key="$samples/keys/$publisher-public.json"

if [ -f "$private_key" ] && [ -f "$public_key" ]; then
    echo "Тестовый ключ издателя $publisher уже создан, использую его"
elif [ -e "$private_key" ] || [ -e "$public_key" ]; then
    echo "Найден только один файл тестового ключа в $samples/keys; удалите каталог и повторите" >&2
    exit 1
else
    echo "Тестовый ключ издателя $publisher"
    cargo run --quiet -p util-maker-packages --bin util-maker-package -- keygen \
        --publisher "$publisher" \
        --private-key "$private_key" \
        --public-key "$public_key"
fi

for version in $versions; do
    case "$version" in
        *[!A-Za-z0-9._-]*)
            echo "Недопустимая версия образца: $version" >&2
            exit 2
            ;;
    esac

    echo "Сборка контрольного модуля $version"
    UTIL_MAKER_MODULE_VERSION="$version" cargo build --quiet --release \
        --target-dir "$target_dir" -p util-maker-control-module
    binary="$target_dir/release/control-module"
    if [ ! -f "$binary" ]; then
        echo "Не найден процесс контрольного модуля: $binary" >&2
        exit 1
    fi

    stage="$samples/.staging/$version"
    mkdir -p "$stage/assets"
    cp "$binary" "$stage/control-module"
    cp "$root/plugins/control/assets/icon.png" "$stage/assets/icon.png"
    chmod 0755 "$stage/control-module"

    package="$samples/control-$version-$platform.umpkg"
    echo "Экспорт $package"
    cargo run --quiet -p util-maker-packages --bin util-maker-package -- export \
        --source "$stage" \
        --manifest "$root/plugins/control/manifest.json" \
        --output "$package" \
        --private-key "$private_key" \
        --version "$version" \
        --platform "$platform" > "$samples/export-$version.json"

    cargo run --quiet -p util-maker-packages --bin util-maker-package -- verify \
        --package "$package" \
        --public-key "$public_key" \
        --platform "$platform" > "$samples/verify-$version.json"

    require_line() {
        pattern=$1
        file=$2
        if ! grep -F "$pattern" "$file" >/dev/null; then
            echo "Не найдено «$pattern» в $file" >&2
            exit 1
        fi
    }
    require_line '"id": "org.utilmaker.control"' "$samples/verify-$version.json"
    require_line '"id": "ping"' "$samples/verify-$version.json"
    require_line "\"version\": \"$version\"" "$samples/verify-$version.json"
    require_line "\"entrypoint\": \"control-module\"" "$samples/verify-$version.json"

    ping_file="$samples/ping-$version.jsonl"
    {
        printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"hostApi":"1.0.0","moduleId":"org.utilmaker.control"}}'
        printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"invoke\",\"params\":{\"callId\":\"sample-$version\",\"actionId\":\"ping\",\"context\":{\"source\":\"tray\"}}}"
        printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"shutdown","params":null}'
    } | "$stage/control-module" > "$ping_file" 2> "$samples/ping-$version.stderr"
    require_line "\"moduleVersion\":\"$version\"" "$ping_file"
    require_line '"status":"ok"' "$ping_file"
done

rm -rf "$samples/.staging"
( cd "$samples" && sha256sum ./*.umpkg > SHA256SUMS )

echo
echo "Образцы: $samples"
ls -1 "$samples"/*.umpkg
echo "Ожидаемые версии в ответе ping: $versions"
echo "Открытый ключ для проверки: $public_key"
