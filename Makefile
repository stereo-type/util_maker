.DEFAULT_GOAL := help
DEV_IMAGE ?= util-maker-dev:local
DEV_HOME := /workspace/.cache/util-maker-dev
WINDOWS_IMAGE ?= util-maker-windows:local
WINDOWS_CACHE := .cache/windows-build
BUILD_KIND ?= dev
BUILD_NUMBER ?= 0
CONTROL_SAMPLES_DIR ?= dist/control-samples
CONTROL_SAMPLE_VERSIONS ?= 0.4.1 0.5.0
CONTROL_SAMPLE_PLATFORM ?= linux-x64
EXAMPLE_PACKAGE_DIR ?= dist/example-plugin
EXAMPLE_VERSION ?= 0.1.0
EXAMPLE_PLATFORM ?= linux-x64
PACKAGE_SOURCE ?=
PACKAGE_MANIFEST ?=
PACKAGE_OUTPUT ?=
PACKAGE_PRIVATE_KEY ?=
PACKAGE_VERSION ?=
PACKAGE_PLATFORM ?=
PACKAGE_EXPORT_ARGS = $(if $(PACKAGE_VERSION),--version '$(PACKAGE_VERSION)') $(if $(PACKAGE_PLATFORM),--platform '$(PACKAGE_PLATFORM)')
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
	--env UTIL_MAKER_BUILD=$(BUILD_KIND) \
	--env UTIL_MAKER_BUILD_NUMBER=$(BUILD_NUMBER)

.PHONY: help dev-image dev-setup dev-run dev-shell fmt fmt-fix lint test check \
	version check-version icons control-samples example-package package-export windows-image build-windows

help:
	@printf '%s\n' 'make dev-setup  — подготовить и проверить окружение Ubuntu-агента' \
		'make dev-run CMD="..."  — выполнить команду разработки в контейнере' \
		'make dev-shell  — открыть интерактивный shell в том же окружении' \
		'make fmt  — проверить форматирование Rust' \
		'make fmt-fix  — отформатировать Rust' \
		'make lint  — Clippy с ошибками вместо предупреждений' \
		'make test  — тесты workspace, включая IPC контрольного модуля' \
		'make check  — сверка версии + fmt + lint + test' \
		'make version  — показать версию и тип сборки' \
		'make icons  — пересоздать иконки приложения' \
		'make control-samples  — два подписанных контрольных .umpkg в dist/control-samples' \
		'make example-package  — подписанный пример стороннего плагина в dist/example-plugin' \
		'make package-export  — подписать подготовленный каталог: PACKAGE_SOURCE, PACKAGE_MANIFEST, PACKAGE_OUTPUT, PACKAGE_PRIVATE_KEY' \
		'make build-windows  — собрать NSIS-установщик в dist/ (BUILD_KIND, BUILD_NUMBER)'

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
	@printf 'Версия: %s\nТип сборки: %s\nНомер сборки: %s\n' "$(VERSION)" "$(BUILD_KIND)" "$(BUILD_NUMBER)"

check-version:
	@cargo_version=$$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1); \
	if [ "$$cargo_version" != "$(VERSION)" ]; then \
		echo "Версия Cargo.toml ($$cargo_version) не совпадает с tauri.conf.json ($(VERSION))"; \
		exit 1; \
	fi; \
	echo "Версия $(VERSION) согласована."

check: check-version fmt lint test

icons:
	python3 scripts/make-icons.py

control-samples: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "CONTROL_SAMPLES_DIR='$(CONTROL_SAMPLES_DIR)' CONTROL_SAMPLE_VERSIONS='$(CONTROL_SAMPLE_VERSIONS)' CONTROL_SAMPLE_PLATFORM='$(CONTROL_SAMPLE_PLATFORM)' sh /workspace/scripts/make-control-samples.sh"

example-package: dev-image
	$(dev_container) "$(DEV_IMAGE)" sh -ec "EXAMPLE_PACKAGE_DIR='$(EXAMPLE_PACKAGE_DIR)' EXAMPLE_VERSION='$(EXAMPLE_VERSION)' EXAMPLE_PLATFORM='$(EXAMPLE_PLATFORM)' sh /workspace/scripts/make-example-package.sh"

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
