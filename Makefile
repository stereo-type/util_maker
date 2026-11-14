.DEFAULT_GOAL := help
DEV_IMAGE ?= util-maker-dev:local
DEV_HOME := /workspace/.cache/util-maker-dev
export CMD

dev_container = docker run --rm --user "$$(id -u):$$(id -g)" \
	--mount "type=bind,source=$(CURDIR),target=/workspace" \
	--env HOME=$(DEV_HOME) --env CARGO_HOME=$(DEV_HOME)/cargo

.PHONY: help dev-image dev-setup dev-run dev-shell
help:
	@printf '%s\n' 'make dev-setup  — подготовить и проверить окружение Ubuntu-агента' \
		'make dev-run CMD="..."  — выполнить команду разработки в контейнере' \
		'make dev-shell  — открыть интерактивный shell в том же окружении'

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
