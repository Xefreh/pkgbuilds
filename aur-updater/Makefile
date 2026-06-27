# aur-updater — build & lifecycle helpers.
# Run `make` (or `make help`) to list targets.

CARGO        ?= cargo
BIN          := target/release/aur-updater
SYSTEMD_USER := $(HOME)/.config/systemd/user
UNITS        := aur-updater.service aur-updater.timer

.DEFAULT_GOAL := help
.PHONY: help build run dry-run fmt clippy check test clean install uninstall

help: ## List available targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'

build: ## Compile the release binary
	$(CARGO) build --release

run: build ## Real run (commit + push AUR + Telegram)
	./$(BIN)

dry-run: build ## Detect and print, push nothing
	./$(BIN) --dry-run

fmt: ## Format the code
	$(CARGO) fmt

clippy: ## Lint (warnings are errors)
	$(CARGO) clippy --all-targets -- -D warnings

check: ## Local CI: format check + clippy + build + tests
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) build --release
	$(CARGO) test

test: ## Run the test suite
	$(CARGO) test

clean: ## Remove build artifacts
	$(CARGO) clean

install: build ## Install and enable the systemd --user timer
	mkdir -p $(SYSTEMD_USER)
	@for u in $(UNITS); do ln -sf "$(CURDIR)/$$u" "$(SYSTEMD_USER)/$$u"; done
	systemctl --user daemon-reload
	systemctl --user enable --now aur-updater.timer

uninstall: ## Disable and remove the systemd --user timer
	-systemctl --user disable --now aur-updater.timer
	@for u in $(UNITS); do rm -f "$(SYSTEMD_USER)/$$u"; done
	systemctl --user daemon-reload
