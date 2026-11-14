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


echo "Util Maker $version: сборка x86_64-pc-windows-msvc, тип ${UTIL_MAKER_BUILD:-dev}"
cargo xwin build --release --target x86_64-pc-windows-msvc \
    -p util-maker-desktop -p util-maker-control-module -p util-maker-screenshots \
    --bin util-maker-desktop --bin control-module --bin screenshot-plugin

exe="$target_dir/x86_64-pc-windows-msvc/release/util-maker-desktop.exe"
module="$target_dir/x86_64-pc-windows-msvc/release/control-module.exe"
manifest="/workspace/plugins/control/manifest.json"

for file in "$exe" "$module" "$manifest"; do
    if [ ! -f "$file" ]; then
        echo "Не найден обязательный файл сборки: $file" >&2
        exit 1
    fi
done

mkdir -p /workspace/dist
build_kind=$(printf '%s' "${UTIL_MAKER_BUILD:-dev}" | tr -cd 'A-Za-z0-9._-')
if [ -z "$build_kind" ]; then
    build_kind=dev
fi
out="/workspace/dist/UtilMaker-Setup-$version-$build_kind.exe"
rm -f "$out"

makensis -V2 \
    -DVERSION="$version" \
    -DBUILD_KIND="$build_kind" \
    -DAPP_EXE="$exe" \
    -DMODULE_EXE="$module" \
    -DMODULE_MANIFEST="$manifest" \
    -DMODULE_ICON=/workspace/plugins/control/assets/icon.png \
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
