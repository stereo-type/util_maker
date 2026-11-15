#!/bin/sh
# Собирает подписанный пакет пипетки для текущей Linux-платформы, проверяет
# подпись и состав архива, затем имитирует обновление только этого пакета.
# Запускается внутри dev-контейнера из `make color-picker-package`.
set -eu

root=/workspace
out=${COLOR_PICKER_PACKAGE_DIR:-dist/color-picker-package}
platform=${COLOR_PICKER_PACKAGE_PLATFORM:-linux-x64}
target_dir=${COLOR_PICKER_TARGET_DIR:-$root/target/color-picker-package}

case "$out" in
    /*) ;;
    *) out="$root/$out" ;;
esac
case "$out" in
    "$root"/dist/*) ;;
    *)
        echo "Пакет разрешено создавать только внутри $root/dist: $out" >&2
        exit 2
        ;;
esac

manifest="$root/plugins/color-picker/manifest.json"
icon="$root/plugins/color-picker/assets/icon.png"
version=$(sed -n 's/^  "version": "\([^"]*\)".*/\1/p' "$manifest" | head -n 1)
if [ -z "$version" ]; then
    echo "Не удалось прочитать версию из $manifest" >&2
    exit 1
fi
next_version=${COLOR_PICKER_NEXT_VERSION:-$(python3 - "$version" <<'PY'
import sys
parts = sys.argv[1].split(".")
if len(parts) != 3 or not all(part.isdigit() for part in parts):
    raise SystemExit(f"неподдерживаемая версия: {sys.argv[1]}")
parts[2] = str(int(parts[2]) + 1)
print(".".join(parts))
PY
)}
case "$next_version" in
    *[!A-Za-z0-9._-]*|"")
        echo "Недопустимая следующая версия: $next_version" >&2
        exit 2
        ;;
esac

rm -rf "$out"
mkdir -p "$out/keys" "$out/.staging"
private_key="$out/keys/org.utilmaker-private.json"
public_key="$out/keys/org.utilmaker-public.json"
cargo run --quiet -p util-maker-packages --bin util-maker-package -- keygen \
    --publisher org.utilmaker \
    --private-key "$private_key" \
    --public-key "$public_key" >/dev/null

package_plugin() {
    package_version=$1
    stage=$2
    output=$3
    echo "Сборка color-picker-module $package_version"
    UTIL_MAKER_MODULE_VERSION="$package_version" cargo build --quiet --release \
        --target-dir "$target_dir" -p util-maker-color-picker --bin color-picker-module
    rm -rf "$stage"
    mkdir -p "$stage/assets"
    cp "$target_dir/release/color-picker-module" "$stage/color-picker-module"
    cp "$icon" "$stage/assets/icon.png"
    chmod 0755 "$stage/color-picker-module"

    echo "Экспорт $output"
    cargo run --quiet -p util-maker-packages --bin util-maker-package -- export \
        --source "$stage" \
        --manifest "$manifest" \
        --output "$output" \
        --private-key "$private_key" \
        --version "$package_version" \
        --platform "$platform" > "$out/export-$package_version.json"

    cargo run --quiet -p util-maker-packages --bin util-maker-package -- verify \
        --package "$output" \
        --public-key "$public_key" \
        --platform "$platform" \
        --host-api 1.0.0 > "$out/verify-$package_version.json"

    python3 - "$output" "$package_version" "$platform" <<'PY'
import hashlib
import json
import sys
import zipfile

package, version, platform = sys.argv[1:]
with zipfile.ZipFile(package) as archive:
    names = set(archive.namelist())
    assert "manifest.json" in names, "нет manifest.json"
    assert "signature.ed25519" in names, "нет signature.ed25519"
    manifest = json.loads(archive.read("manifest.json"))
    assert manifest["id"] == "org.utilmaker.color-picker", manifest["id"]
    assert manifest["publisher"] == "org.utilmaker", manifest["publisher"]
    assert manifest["version"] == version, manifest["version"]
    assert manifest["hostApi"] == ">=1.0.0 <2.0.0", manifest["hostApi"]
    assert manifest["entrypoints"] == {
        platform: "color-picker-module.exe" if platform == "windows-x64" else "color-picker-module"
    }, manifest["entrypoints"]
    assert [action["id"] for action in manifest["actions"]] == ["pick-color"]
    assert set(manifest["capabilities"]) == {
        "screen.capture",
        "clipboard.write",
        "settings.read",
    }
    assert manifest["settings"]["fields"][0]["id"] == "format"
    assert "assets/icon.png" in names, "нет иконки"
    listed = {item["path"]: item["sha256"] for item in manifest["files"]}
    assert set(listed) == names - {"manifest.json", "signature.ed25519"}, (set(listed), names)
    for path, digest in listed.items():
        actual = hashlib.sha256(archive.read(path)).hexdigest()
        assert actual == digest, f"SHA-256 {path}: {actual} != {digest}"
print(f"Пакет {package}: подпись проверена, файлов {len(names) - 2}")
PY
}

package_v1="$out/org.utilmaker.color-picker-$version-$platform.umpkg"
package_v2="$out/org.utilmaker.color-picker-$next_version-$platform.umpkg"
package_plugin "$version" "$out/.staging/v1" "$package_v1"
package_plugin "$next_version" "$out/.staging/v2" "$package_v2"

profile="$out/profile"
data="$out/data"
mkdir -p "$data/module-settings/org.utilmaker.color-picker"
printf '%s\n' '{"values":{"format":"rgb"}}' > "$data/module-settings/org.utilmaker.color-picker/settings.json"
printf '%s\n' '{"org.utilmaker.color-picker/pick-color":["Ctrl+Shift+P"]}' > "$data/shortcuts.json"
before_settings=$(sha256sum "$data/module-settings/org.utilmaker.color-picker/settings.json" | cut -d' ' -f1)
before_shortcuts=$(sha256sum "$data/shortcuts.json" | cut -d' ' -f1)

cargo run --quiet -p util-maker-packages --bin util-maker-package -- install \
    --package "$package_v1" \
    --public-key "$public_key" \
    --profile "$profile" \
    --platform "$platform" > "$out/install-$version.json"
update=$(cargo run --quiet -p util-maker-packages --bin util-maker-package -- install \
    --package "$package_v2" \
    --public-key "$public_key" \
    --profile "$profile" \
    --platform "$platform")
printf '%s\n' "$update" > "$out/install-$next_version.json"
cargo run --quiet -p util-maker-packages --bin util-maker-package -- list \
    --profile "$profile" > "$out/installed.json"

after_settings=$(sha256sum "$data/module-settings/org.utilmaker.color-picker/settings.json" | cut -d' ' -f1)
after_shortcuts=$(sha256sum "$data/shortcuts.json" | cut -d' ' -f1)
[ "$before_settings" = "$after_settings" ] || {
    echo "Обновление изменило настройки формата" >&2
    exit 1
}
[ "$before_shortcuts" = "$after_shortcuts" ] || {
    echo "Обновление изменило назначение сочетания" >&2
    exit 1
}

python3 - "$out" "$version" "$next_version" <<'PY'
import json
import pathlib
import sys

out, previous, current = sys.argv[1:]
root = pathlib.Path(out)
listed = json.loads((root / "installed.json").read_text())
record = next(item for item in listed if item["id"] == "org.utilmaker.color-picker")
assert record["version"] == current, record
assert record["enabled"] is True, record
index = json.loads((root / "profile/installed.json").read_text())
profile = index["packages"]["org.utilmaker.color-picker"]
assert profile["activeVersion"] == current, profile
assert set(profile["versions"]) >= {previous, current}, profile
assert profile["enabled"] is True, profile
update = json.loads((root / f"install-{current}.json").read_text())
assert update["previousVersion"] == previous, update
assert update["alreadyInstalled"] is False, update
settings = root / "data/module-settings/org.utilmaker.color-picker/settings.json"
shortcuts = root / "data/shortcuts.json"
assert json.loads(settings.read_text())["values"]["format"] == "rgb"
assert "org.utilmaker.color-picker/pick-color" in json.loads(shortcuts.read_text())
print(f"Обновление {previous} → {current}: активная версия и файлы настроек на месте")
PY

rm -rf "$out/.staging"

echo
echo "Пакеты:"
ls -1 "$out"/*.umpkg
echo "Профиль: $profile"
echo "Открытый ключ: $public_key"
