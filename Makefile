.DEFAULT_GOAL := help
DEV_IMAGE ?= util-maker-dev:local
DEV_HOME := /workspace/.cache/util-maker-dev
WINDOWS_IMAGE ?= util-maker-windows:local
WINDOWS_CACHE := .cache/windows-build
BUILD_KIND ?= dev
CONTROL_SAMPLES_DIR ?= dist/control-samples
CONTROL_SAMPLE_VERSIONS ?= 0.4.1 0.5.0
CONTROL_SAMPLE_PLATFORM ?= linux-x64
EXAMPLE_PACKAGE_DIR ?= dist/example-plugin
EXAMPLE_VERSION ?= 0.1.0
EXAMPLE_PLATFORM ?= linux-x64
COLOR_PICKER_PACKAGE_DIR ?= dist/color-picker-package
COLOR_PICKER_PACKAGE_PLATFORM ?= linux-x64
PACKAGE_SOURCE ?=
PACKAGE_MANIFEST ?=
PACKAGE_OUTPUT ?=
PACKAGE_PRIVATE_KEY ?=
PACKAGE_VERSION ?=
PACKAGE_PLATFORM ?=
PACKAGE_PROFILE ?=
PACKAGE_PUBLIC_KEY ?=
PACKAGE_HOST_API ?=
PACKAGE_EXPORT_ARGS = $(if $(PACKAGE_VERSION),--version '$(PACKAGE_VERSION)') $(if $(PACKAGE_PLATFORM),--platform '$(PACKAGE_PLATFORM)')
PACKAGE_INSTALL_ARGS = --package '$(PACKAGE_OUTPUT)' --public-key '$(PACKAGE_PUBLIC_KEY)' --profile '$(PACKAGE_PROFILE)' --platform '$(PACKAGE_PLATFORM)' $(if $(PACKAGE_HOST_API),--host-api '$(PACKAGE_HOST_API)')
VERSION := $(shell sed -n 's/^  "version": "\([^"]*\)".*/\1/p' apps/desktop/tauri.conf.json | head -n 1)
export CMD

dev_container = docker run --rm --user "$$(id -u):$$(id -g)" \
	--mount "type=bind,source=$(CURDIR),target=/workspace" \
	--env HOME=$(DEV_HOME) --env CARGO_HOME=$(DEV_HOME)/cargo

windows_container = docker run --rm --user "$$(id -u):$$(id -g)" \
	--mount "type=bind,source=$(CURDIR),target=/workspace" \
	--env HOME=/workspace/$(WINDOWS_CACHE)/home \
	--env CARGO_HOME=/workspace/$(WINDOWS_CACHE)/cargo \
	--env CARGO_TARGET_DIR=/workspace/$(WINDOWS_CACHE)/target \
	--env XWIN_CACHE_DIR=/workspace/$(WINDOWS_CACHE)/xwin \
	--env UTIL_MAKER_BUILD=$(BUILD_KIND)

.PHONY: help dev-image dev-setup dev-run dev-shell fmt fmt-fix lint test check task-summary \
	version check-version bump-version build-windows-rise icons screenshot-check color-picker-check color-picker-reference color-picker-package screen-ocr-check screen-ocr-reference qr-code-check qr-code-reference \
	control-samples example-package package-export package-install windows-image build-windows

help:
	@printf '%s\n' 'make dev-setup  — подготовить и проверить окружение Ubuntu-агента' \
		'make dev-run CMD="..."  — выполнить команду разработки в контейнере' \
		'make dev-shell  — открыть интерактивный shell в том же окружении' \
		'make fmt  — проверить форматирование Rust' \
		'make fmt-fix  — отформатировать Rust' \
		'make lint  — Clippy с ошибками вместо предупреждений' \
		'make test  — тесты workspace, включая IPC контрольного модуля' \
		'make check  — сверка версии + fmt + lint + test' \
		'make task-summary  — обновить сводку незавершённых задач по разделам' \
		'make version  — показать версию и тип сборки' \
		'make bump-version VERSION=x.y.z  — поднять версию в Cargo.toml, tauri.conf.json и установщике' \
		'make build-windows-rise  — поднять patch-версию и сразу собрать установщик' \
		'make icons  — пересоздать иконки приложения' \
		'make screenshot-check  — процесс, редактор и манифест плагина «Скриншоты»' \
		'make color-picker-check  — выборка, лупа, копирование и граница процесса пипетки' \
		'make color-picker-reference  — пересоздать эталон пипетки' \
		'make color-picker-package  — подписанный пакет пипетки и проверка обновления' \
		'make screen-ocr-check  — обрезка, Tesseract, эталон и процесс OCR' \
		'make screen-ocr-reference  — пересоздать эталон OCR' \
		'make qr-code-check  — кодирование, показ QR, декодирование, эталоны и процесс' \
		'make qr-code-reference  — пересоздать эталоны QR' \
		'make control-samples  — два подписанных контрольных .umpkg в dist/control-samples' \
		'make example-package  — подписанный пример стороннего плагина в dist/example-plugin' \
		'make package-export  — подписать подготовленный каталог: PACKAGE_SOURCE, PACKAGE_MANIFEST, PACKAGE_OUTPUT, PACKAGE_PRIVATE_KEY' \
		'make package-install  — установить .umpkg в профиль: PACKAGE_OUTPUT, PACKAGE_PUBLIC_KEY, PACKAGE_PROFILE, PACKAGE_PLATFORM' \
		'make build-windows  — собрать NSIS-установщик в dist/ (BUILD_KIND=dev)'

task-summary:
	python3 scripts/task-summary.py

dev-image:
	docker build --tag "$(DEV_IMAGE)" docker/dev
	@mkdir -p .cache/util-maker-dev

dev-setup: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh /usr/local/bin/util-maker-check-env

dev-run: dev-image
	@test -n "$$CMD" || { echo 'Укажите CMD, например make dev-run CMD="cargo --version"'; exit 2; }
	$(dev_container) "$(DEV_IMAGE)" sh -ec "$$CMD"

dev-shell: dev-image
	$(dev_container) -it "$(DEV_IMAGE)" bash

fmt: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "cargo fmt --all -- --check"

fmt-fix: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "cargo fmt --all"

lint: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "cargo clippy --workspace --all-targets -- -D warnings"

test: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "cargo build --workspace --bins && cargo test --workspace --all-targets"

version:
	@printf 'Версия: %s\nТип сборки: %s\n' "$(VERSION)" "$(BUILD_KIND)"

bump-version:
	@test -n "$(VERSION)" || { echo 'Укажите VERSION=x.y.z'; exit 2; }
	@echo "$(VERSION)" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$$' || { echo 'VERSION должен быть в формате x.y.z'; exit 2; }
	@sed -i '0,/^version = ".*"/s//version = "$(VERSION)"/' Cargo.toml
	@sed -i 's/"version": "[^"]*"/"version": "$(VERSION)"/' apps/desktop/tauri.conf.json
	@sed -i 's/!define VERSION "[^"]*"/!define VERSION "$(VERSION)"/' installer/util-maker.nsi
	@sed -i 's/"version": "[^"]*"/"version": "$(VERSION)"/' plugins/control/manifest.json
	@sed -i 's/"version": "[^"]*"/"version": "$(VERSION)"/' plugins/screenshot/manifest.json
	@sed -i 's/"version": "[^"]*"/"version": "$(VERSION)"/' plugins/translator/manifest.json
	@sed -i 's/"version": "[^"]*"/"version": "$(VERSION)"/' plugins/screen-ocr/manifest.json
	@sed -i 's/"version": "[^"]*"/"version": "$(VERSION)"/' plugins/qr-code/manifest.json
	@sed -i 's/"version": "[^"]*"/"version": "$(VERSION)"/' plugins/color-picker/manifest.json
	@sed -i 's/Util Maker — прототип [0-9][0-9.]*/Util Maker — прототип $(VERSION)/' installer/README.txt
	@echo "Версия $(VERSION) записана в Cargo.toml, tauri.conf.json и установщик. Проверьте make check-version."

build-windows-rise:
	@set -e; \
	current="$(VERSION)"; \
	echo "$$current" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$$' || { echo "Версия $$current не в формате x.y.z, автоматический patch-бамп невозможен"; exit 2; }; \
	next=$$(printf '%s\n' "$$current" | awk -F. '{printf "%s.%s.%d", $$1, $$2, $$3 + 1}'); \
	echo "Версия $$current -> $$next"; \
	$(MAKE) bump-version VERSION="$$next"; \
	$(MAKE) build-windows

check-version:
	@cargo_version=$$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1); \
	if [ "$$cargo_version" != "$(VERSION)" ]; then \
		echo "Версия Cargo.toml ($$cargo_version) не совпадает с tauri.conf.json ($(VERSION))"; \
		exit 1; \
	fi; \
	for manifest in plugins/control/manifest.json plugins/screenshot/manifest.json plugins/translator/manifest.json plugins/screen-ocr/manifest.json plugins/qr-code/manifest.json plugins/color-picker/manifest.json; do \
		manifest_version=$$(sed -n 's/^  "version": "\([^"]*\)".*/\1/p' "$$manifest" | head -n 1); \
		if [ "$$manifest_version" != "$$cargo_version" ]; then \
			echo "Версия $$manifest ($$manifest_version) не совпадает с Cargo.toml ($$cargo_version)"; \
			exit 1; \
		fi; \
	done; \
	echo "Версия $(VERSION) согласована."

check: check-version fmt lint test

icons:
	python3 scripts/make-icons.py

screenshot-check: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "cargo build --workspace --bins && cargo test -p util-maker-screenshots && cargo test -p util-maker-core --test screenshot_module"

color-picker-check: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "cargo build --workspace --bins && cargo test -p util-maker-color-picker && cargo test -p util-maker-core --test color_picker_module"

color-picker-reference:
	python3 scripts/make-color-picker-reference.py

color-picker-package: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "COLOR_PICKER_PACKAGE_DIR='$(COLOR_PICKER_PACKAGE_DIR)' COLOR_PICKER_PACKAGE_PLATFORM='$(COLOR_PICKER_PACKAGE_PLATFORM)' sh /workspace/scripts/make-color-picker-package.sh"

screen-ocr-check: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "cargo build --workspace --bins && UTIL_MAKER_REQUIRE_TESSERACT=1 cargo test -p util-maker-screen-ocr && cargo test -p util-maker-core --test screen_ocr_module"

screen-ocr-reference: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "python3 scripts/make-screen-ocr-reference.py"

qr-code-check: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "cargo build --workspace --bins && cargo test -p util-maker-qr-code && cargo test -p util-maker-core --test qr_code_module"

qr-code-reference: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "python3 scripts/make-qr-code-reference.py"

control-samples: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "CONTROL_SAMPLES_DIR='$(CONTROL_SAMPLES_DIR)' CONTROL_SAMPLE_VERSIONS='$(CONTROL_SAMPLE_VERSIONS)' CONTROL_SAMPLE_PLATFORM='$(CONTROL_SAMPLE_PLATFORM)' sh /workspace/scripts/make-control-samples.sh"

example-package: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "EXAMPLE_PACKAGE_DIR='$(EXAMPLE_PACKAGE_DIR)' EXAMPLE_VERSION='$(EXAMPLE_VERSION)' EXAMPLE_PLATFORM='$(EXAMPLE_PLATFORM)' sh /workspace/scripts/make-example-package.sh"

package-install: dev-image
	@test -n "$(PACKAGE_OUTPUT)" || { echo 'Укажите PACKAGE_OUTPUT=путь/к/пакету.umpkg'; exit 2; }
	@test -n "$(PACKAGE_PUBLIC_KEY)" || { echo 'Укажите PACKAGE_PUBLIC_KEY=путь/к/публичному/ключу.json'; exit 2; }
	@test -n "$(PACKAGE_PROFILE)" || { echo 'Укажите PACKAGE_PROFILE=каталог/профиля'; exit 2; }
	@test -n "$(PACKAGE_PLATFORM)" || { echo 'Укажите PACKAGE_PLATFORM=linux-x64'; exit 2; }
	$(dev_container) "$(DEV_IMAGE)" sh -ec "cargo run --quiet -p util-maker-packages --bin util-maker-package -- install $(PACKAGE_INSTALL_ARGS)"

package-export: dev-image
	@test -n "$(PACKAGE_SOURCE)" || { echo 'Укажите PACKAGE_SOURCE=<каталог полезной нагрузки>'; exit 2; }
	@test -n "$(PACKAGE_MANIFEST)" || { echo 'Укажите PACKAGE_MANIFEST=<путь к manifest.json>'; exit 2; }
	@test -n "$(PACKAGE_OUTPUT)" || { echo 'Укажите PACKAGE_OUTPUT=<путь к .umpkg>'; exit 2; }
	@test -n "$(PACKAGE_PRIVATE_KEY)" || { echo 'Укажите PACKAGE_PRIVATE_KEY=<закрытый ключ издателя>'; exit 2; }
	$(dev_container) "$(DEV_IMAGE)" sh -ec "cargo run --quiet -p util-maker-packages --bin util-maker-package -- export --source '$(PACKAGE_SOURCE)' --manifest '$(PACKAGE_MANIFEST)' --output '$(PACKAGE_OUTPUT)' --private-key '$(PACKAGE_PRIVATE_KEY)' $(PACKAGE_EXPORT_ARGS)"

windows-image:
	docker build --tag "$(WINDOWS_IMAGE)" docker/windows
	@mkdir -p $(WINDOWS_CACHE)

build-windows: windows-image
	@mkdir -p dist $(WINDOWS_CACHE)
	$(windows_container) "$(WINDOWS_IMAGE)" sh /usr/local/bin/util-maker-build-windows
	@printf 'Установщик: %s\n' "$$(ls -1t dist/UtilMaker-Setup-*.exe | head -1)"
