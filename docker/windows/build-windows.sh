#!/bin/sh
# Собирает хост и штатные пакеты под x86_64-pc-windows-msvc и упаковывает
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
    -p util-maker-color-picker -p util-maker-qr-code -p util-maker-translator-yandex \
    -p util-maker-translator-google \
    --bin util-maker-desktop --bin control-module --bin screenshot-plugin --bin translator-module \
    --bin color-picker-module --bin qr-code-module --bin translator-yandex --bin translator-google

exe="$target_dir/x86_64-pc-windows-msvc/release/util-maker-desktop.exe"
module="$target_dir/x86_64-pc-windows-msvc/release/control-module.exe"
screenshot_module="$target_dir/x86_64-pc-windows-msvc/release/screenshot-plugin.exe"
translator_module="$target_dir/x86_64-pc-windows-msvc/release/translator-module.exe"
color_picker_module="$target_dir/x86_64-pc-windows-msvc/release/color-picker-module.exe"
qr_code_module="$target_dir/x86_64-pc-windows-msvc/release/qr-code-module.exe"
yandex_module="$target_dir/x86_64-pc-windows-msvc/release/translator-yandex.exe"
google_module="$target_dir/x86_64-pc-windows-msvc/release/translator-google.exe"
manifest="/workspace/plugins/control/manifest.json"
screenshot_manifest="/workspace/plugins/screenshot/manifest.json"
screenshot_icon="/workspace/plugins/screenshot/assets/icon.png"
translator_manifest="/workspace/plugins/translator/manifest.json"
translator_icon="/workspace/plugins/translator/assets/icon.png"
color_picker_manifest="/workspace/plugins/color-picker/manifest.json"
color_picker_icon="/workspace/plugins/color-picker/assets/icon.png"
qr_code_manifest="/workspace/plugins/qr-code/manifest.json"
qr_code_icon="/workspace/plugins/qr-code/assets/icon.png"
yandex_manifest="/workspace/plugins/translator/providers/yandex/manifest.json"
google_manifest="/workspace/plugins/translator/providers/google/manifest.json"

for file in "$exe" "$module" "$manifest" "$screenshot_module" "$screenshot_manifest" "$screenshot_icon" "$translator_module" "$translator_manifest" "$translator_icon" "$color_picker_module" "$color_picker_manifest" "$qr_code_module" "$qr_code_manifest" "$qr_code_icon" \
        "$yandex_module" "$yandex_manifest" "$google_module" "$google_manifest"; do
    if [ ! -f "$file" ]; then
        echo "Не найден обязательный файл сборки: $file" >&2
        exit 1
    fi
done

package_dir="$dist_dir/packages"
mkdir -p "$package_dir/keys" "$package_dir/staging"
# В установщик попадают только пакеты текущей версии. Без очистки dist
# копит архивы прошлых сборок, и установщик раздувается до десятков мегабайт.
find "$package_dir" -maxdepth 1 -type f -name '*.umpkg' -delete
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
    plugin_version=$(sed -n 's/^  "version": "\([^"]*\)".*/\1/p' "$plugin_manifest" | head -n 1)
    output="$package_dir/$plugin_id-$plugin_version-windows-x64.umpkg"
    cargo run --quiet -p util-maker-packages --bin util-maker-package -- export \
        --source "$stage" \
        --manifest "$plugin_manifest" \
        --output "$output" \
        --private-key "$private_key" \
        --platform windows-x64
    cargo run --quiet -p util-maker-packages --bin util-maker-package -- verify \
        --package "$output" \
        --public-key "$public_key" \
        --platform windows-x64 >/dev/null
}

package_plugin org.utilmaker.screenshot "$screenshot_module" "$screenshot_manifest" "$screenshot_icon"
package_plugin org.utilmaker.translator "$translator_module" "$translator_manifest" "$translator_icon"
package_plugin org.utilmaker.color-picker "$color_picker_module" "$color_picker_manifest" "$color_picker_icon"
package_plugin org.utilmaker.qr-code "$qr_code_module" "$qr_code_manifest" "$qr_code_icon"
package_plugin org.utilmaker.translator.yandex "$yandex_module" "$yandex_manifest" ""
package_plugin org.utilmaker.translator.google "$google_module" "$google_manifest" ""

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
