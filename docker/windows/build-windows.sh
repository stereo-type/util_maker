#!/bin/sh
# Собирает хост и контрольный модуль под x86_64-pc-windows-msvc и упаковывает
# NSIS-установщик в /workspace/dist. Запускается внутри контейнера
# docker/windows из цели `make build-windows`.
set -eu

cd /workspace

version=$(sed -n 's/^  "version": "\([^"]*\)".*/\1/p' apps/desktop/tauri.conf.json | head -n 1)
if [ -z "$version" ]; then
    echo "Не удалось прочитать version из apps/desktop/tauri.conf.json" >&2
    exit 1
fi

target_dir="${CARGO_TARGET_DIR:-/workspace/.cache/windows-build/target}"
export CARGO_TARGET_DIR="$target_dir"

dist_dir=/workspace/dist
mkdir -p "$dist_dir"
build_kind=$(printf '%s' "${UTIL_MAKER_BUILD:-dev}" | tr -cd 'A-Za-z0-9._-')
if [ -z "$build_kind" ]; then
    build_kind=dev
fi
out="$dist_dir/UtilMaker-Setup-$version-$build_kind.exe"

# Установщики прошлых сборок того же типа больше не нужны: dist иначе копит
# версии. Маска ограничена типом сборки, поэтому сборка dev не удаляет
# release/prod, а пакеты, эталоны и прочие файлы в dist остаются на месте.
removed=0
for previous in "$dist_dir"/UtilMaker-Setup-*-"$build_kind".exe; do
    [ -f "$previous" ] || continue
    rm -f -- "$previous"
    removed=$((removed + 1))
    echo "Удалён прежний установщик: ${previous##*/}"
done
if [ "$removed" -gt 0 ]; then
    echo "Очистка dist: удалено установщиков типа $build_kind — $removed."
else
    echo "Очистка dist: прежних установщиков типа $build_kind нет."
fi

echo "Util Maker $version: сборка x86_64-pc-windows-msvc, тип $build_kind"
cargo xwin build --release --target x86_64-pc-windows-msvc \
    -p util-maker-desktop -p util-maker-control-module -p util-maker-screenshots -p util-maker-translator \
    --bin util-maker-desktop --bin control-module --bin screenshot-plugin --bin translator-module

exe="$target_dir/x86_64-pc-windows-msvc/release/util-maker-desktop.exe"
module="$target_dir/x86_64-pc-windows-msvc/release/control-module.exe"
screenshot_module="$target_dir/x86_64-pc-windows-msvc/release/screenshot-plugin.exe"
translator_module="$target_dir/x86_64-pc-windows-msvc/release/translator-module.exe"
manifest="/workspace/plugins/control/manifest.json"
screenshot_manifest="/workspace/plugins/screenshot/manifest.json"
translator_manifest="/workspace/plugins/translator/manifest.json"

for file in "$exe" "$module" "$manifest" "$screenshot_module" "$screenshot_manifest" "$translator_module" "$translator_manifest"; do
    if [ ! -f "$file" ]; then
        echo "Не найден обязательный файл сборки: $file" >&2
        exit 1
    fi
done

package_dir="$dist_dir/packages"
mkdir -p "$package_dir/keys" "$package_dir/staging"
private_key="$package_dir/keys/org.utilmaker-private.json"
public_key="$package_dir/keys/org.utilmaker-public.json"
if [ ! -f "$private_key" ]; then
    echo "Генерация ключа издателя для штатных пакетов"
    cargo run --quiet -p util-maker-packages --bin util-maker-package -- keygen \
        --publisher org.utilmaker \
        --private-key "$private_key" \
        --public-key "$public_key"
fi

package_plugin() {
    plugin_id=$1
    plugin_exe=$2
    plugin_manifest=$3
    plugin_icon=$4
    stage="$package_dir/staging/$plugin_id"
    rm -rf "$stage"
    mkdir -p "$stage/assets"
    cp "$plugin_exe" "$stage/"
    cp "$plugin_manifest" "$stage/manifest.json"
    if [ -n "$plugin_icon" ] && [ -f "$plugin_icon" ]; then
        cp "$plugin_icon" "$stage/assets/icon.png"
    fi
    output="$package_dir/$plugin_id-$version-windows-x64.umpkg"
    cargo run --quiet -p util-maker-packages --bin util-maker-package -- export \
        --source "$stage" \
        --manifest "$plugin_manifest" \
        --output "$output" \
        --private-key "$private_key" \
        --platform windows-x64
    cargo run --quiet -p util-maker-packages --bin util-maker-package -- verify \
        --package "$output" \
        --public-key "$public_key" \
        --platform windows-x64 \
        --host-api 1.0.0 >/dev/null
}

package_plugin org.utilmaker.screenshot "$screenshot_module" "$screenshot_manifest" /workspace/plugins/screenshot/assets/icon.png
package_plugin org.utilmaker.translator "$translator_module" "$translator_manifest" ""

rm -rf "$package_dir/staging"

makensis -V2 \
    -DVERSION="$version" \
    -DBUILD_KIND="$build_kind" \
    -DAPP_EXE="$exe" \
    -DMODULE_EXE="$module" \
    -DMODULE_MANIFEST="$manifest" \
    -DMODULE_ICON=/workspace/plugins/control/assets/icon.png \
    -DPACKAGE_DIR="$package_dir" \
    -DSCRIPTS_DIR=/workspace/scripts \
    -DREADME_FILE=/workspace/installer/README.txt \
    -DOUT_FILE="$out" \
    /workspace/installer/util-maker.nsi



if [ ! -f "$out" ]; then
    echo "NSIS не создал установщик $out" >&2
    exit 1
fi

echo "Установщик: $out"
echo "Размер: $(du -h "$out" | cut -f1)"
echo "Контрольная сумма SHA-256: $(sha256sum "$out" | cut -d' ' -f1)"
