# All Rust commands run inside Docker; nothing is built on the host.
DC      := docker compose
DEV     := $(DC) run --rm dev
IMAGE   ?= pidx:latest
PROJECT ?= $(CURDIR)/fixtures
PROJECT_ABS := $(abspath $(PROJECT))
# Same id as the README's `$(pwd | shasum | cut -c1-12)` when run from the project folder.
PROJECT_ID  := $(shell echo "$(PROJECT_ABS)" | shasum | cut -c1-12)
RUN_PIDX    := docker run -i --rm -v "$(PROJECT_ABS)":/workspace:ro -v pidx-data:/data \
               -e PIDX_PROJECT_ID=$(PROJECT_ID) $(IMAGE)

.PHONY: help dev-image fmt lint test check build build-image build-image-php \
        scan serve stats doctor shell clean

help:
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-16s %s\n", $$1, $$2}'

dev-image: ## Build the dev toolchain image
	$(DC) build dev

fmt: ## Format all code
	$(DEV) cargo fmt --all

lint: ## rustfmt check + clippy with warnings as errors
	$(DEV) cargo fmt --all -- --check
	$(DEV) cargo clippy --workspace --all-targets --locked -- -D warnings

test: ## Run all tests
	$(DEV) cargo test --workspace --locked

check: lint test ## CI: lint then test

build: ## Debug build in the dev container
	$(DEV) cargo build --workspace

build-image: ## Build the runtime image ($(IMAGE))
	docker build --target runtime -t $(IMAGE) .

build-image-php: ## Build the runtime image with php-cli (artisan mode)
	docker build --target runtime-php -t pidx-php:latest .

scan: ## Index PROJECT=/path
	$(RUN_PIDX) scan --project /workspace

serve: ## Run the MCP server on stdio for PROJECT=/path
	$(RUN_PIDX) serve --project /workspace

stats: ## Index stats for PROJECT=/path
	$(RUN_PIDX) stats --project /workspace

doctor: ## Detection report for PROJECT=/path
	$(RUN_PIDX) doctor --project /workspace

shell: ## Shell in the dev container
	$(DC) run --rm dev bash

clean: ## Remove dev volumes (cargo cache, target)
	$(DC) down --volumes
