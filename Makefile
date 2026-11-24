# THUGSred-smsGateway - project management interface.  Run `make help`.

SHELL       := /bin/sh
PREFIX      ?= /opt/THUGSred-smsGateway
CONFDIR     ?= /etc/thugsred
DESTDIR     ?=
CARGO       ?= cargo
SERVICE     := thugsred-smsgateway
BIN         := thugsred-smsgateway
VERSION     := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
HOST_ARCH   := $(shell uname -m)
ARM64       := aarch64-unknown-linux-gnu
AMD64       := x86_64-unknown-linux-gnu
CONFIG      ?= $(CONFDIR)/smsgateway.conf
INSTALLED   := $(PREFIX)/bin/$(BIN)
SUDO        := $(shell [ "$$(id -u)" -eq 0 ] || echo sudo)
BACKUP      ?=

# Keep local user names and directories out of release binaries.
REMAP       := --remap-path-prefix=$(CURDIR)=. --remap-path-prefix=$(HOME)/.cargo=cargo
export CARGO_TERM_COLOR ?= auto

.DEFAULT_GOAL := help
.PHONY: help deps build release build-arm64 build-amd64 check fmt fmt-check lint test \
        test-integration test-mysql audit deny docs install uninstall purge reinstall upgrade \
        config check-config migrate backup restore doctor start stop restart reload status \
        logs logs-messages logs-system enable disable clean distclean package sbom deb \
        hooks secret-scan simulator not-root

help: ## Show this help
	@printf 'THUGSred-smsGateway %s\n\nUsage: make <target> [PREFIX=%s] [DESTDIR=]\n\n' '$(VERSION)' '$(PREFIX)'
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  \033[1m%-18s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

not-root:
	@if [ "$$(id -u)" -eq 0 ] && [ -z "$$ALLOW_ROOT_BUILD" ]; then \
		echo "warning: building as root is discouraged; build as a normal user and run only 'sudo make install' as root (set ALLOW_ROOT_BUILD=1 to silence)"; fi

## --- Build ------------------------------------------------------------------

deps: ## Check the toolchain and fetch locked dependencies
	@command -v $(CARGO) >/dev/null || { echo "cargo not found: install Rust from https://rustup.rs or 'apt install cargo'"; exit 1; }
	@command -v cc >/dev/null || { echo "C compiler not found: sudo apt install build-essential"; exit 1; }
	@$(CARGO) --version && rustc --version
	$(CARGO) fetch --locked
	@echo "optional tools: cargo-audit, cargo-deny (cargo install --locked cargo-audit cargo-deny), gitleaks, alsa-utils"

build: not-root ## Debug build
	$(CARGO) build --locked

release: not-root ## Optimised release build for this machine
	RUSTFLAGS="$(REMAP) $$RUSTFLAGS" $(CARGO) build --locked --release

build-arm64: not-root ## Release build for Linux ARM64 (Raspberry Pi 4); cross-compiles on other hosts
	@if [ "$(HOST_ARCH)" != "aarch64" ]; then \
		command -v aarch64-linux-gnu-gcc >/dev/null || { echo "cross toolchain missing: sudo apt install gcc-aarch64-linux-gnu libc6-dev-arm64-cross, and 'rustup target add $(ARM64)' (or apt install libstd-rust-dev-arm64-cross)"; exit 1; }; fi
	CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=$$( [ "$(HOST_ARCH)" = "aarch64" ] && echo cc || echo aarch64-linux-gnu-gcc ) \
	RUSTFLAGS="$(REMAP) $$RUSTFLAGS" $(CARGO) build --locked --release --target $(ARM64)

build-amd64: not-root ## Release build for Linux AMD64; cross-compiles on other hosts
	@if [ "$(HOST_ARCH)" != "x86_64" ]; then \
		command -v x86_64-linux-gnu-gcc >/dev/null || { echo "cross toolchain missing: sudo apt install gcc-x86-64-linux-gnu libc6-dev-amd64-cross, and 'rustup target add $(AMD64)'"; exit 1; }; fi
	CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=$$( [ "$(HOST_ARCH)" = "x86_64" ] && echo cc || echo x86_64-linux-gnu-gcc ) \
	RUSTFLAGS="$(REMAP) $$RUSTFLAGS" $(CARGO) build --locked --release --target $(AMD64)

## --- Quality ----------------------------------------------------------------

check: fmt-check lint test ## Formatting, lints and all tests

fmt: ## Format the source code
	$(CARGO) fmt --all

fmt-check: ## Verify formatting
	$(CARGO) fmt --all -- --check

lint: ## Clippy with warnings as errors
	$(CARGO) clippy --locked --all-targets -- -D warnings

test: ## Unit and integration tests (simulated modem, SQLite; no hardware needed)
	$(CARGO) test --locked

test-integration: ## Integration tests only (gateway, API and resilience against the simulator)
	$(CARGO) test --locked --test gateway --test api --test resilience --test examples

test-mysql: ## All tests against MySQL/MariaDB: make test-mysql TEST_MYSQL_URL=mysql://root:pw@127.0.0.1:3306
	@[ -n "$(TEST_MYSQL_URL)" ] || { echo "set TEST_MYSQL_URL=mysql://user:password@host:port (the user must be allowed to create databases)"; exit 1; }
	TEST_MYSQL_URL='$(TEST_MYSQL_URL)' $(CARGO) test --locked

audit: ## Check dependencies for known vulnerabilities (cargo-audit)
	@command -v cargo-audit >/dev/null || { echo "cargo-audit not installed: cargo install --locked cargo-audit"; exit 1; }
	$(CARGO) audit

deny: ## Check licences, advisories and sources (cargo-deny)
	@command -v cargo-deny >/dev/null || { echo "cargo-deny not installed: cargo install --locked cargo-deny"; exit 1; }
	$(CARGO) deny check

secret-scan: ## Scan the working tree for credentials (gitleaks, or the built-in fallback)
	@if command -v gitleaks >/dev/null; then gitleaks detect --no-banner --redact --source .; else sh scripts/secret-scan.sh; fi

hooks: ## Install the optional pre-commit secret scan
	install -m 0755 scripts/pre-commit .git/hooks/pre-commit
	@echo "pre-commit hook installed"

docs: ## Build the Rust API documentation (target/doc)
	$(CARGO) doc --locked --no-deps

## --- Installation -----------------------------------------------------------

install: ## Install the release binary, unit file and default configuration (keeps existing config/data)
	@[ -f target/release/$(BIN) ] || { echo "target/release/$(BIN) not found: run 'make release' first (as a normal user)"; exit 1; }
	PREFIX='$(PREFIX)' CONFDIR='$(CONFDIR)' DESTDIR='$(DESTDIR)' sh scripts/install.sh

uninstall: ## Remove the service and program files; keeps configuration, database, logs and recordings
	PREFIX='$(PREFIX)' CONFDIR='$(CONFDIR)' sh scripts/uninstall.sh

purge: ## Remove everything including configuration and all data (asks for confirmation)
	PREFIX='$(PREFIX)' CONFDIR='$(CONFDIR)' sh scripts/uninstall.sh --purge

reinstall: ## Reinstall program files and restart the service if it was running
	@$(MAKE) --no-print-directory install
	@if systemctl is-active --quiet $(SERVICE); then $(SUDO) systemctl restart $(SERVICE) && echo "service restarted"; fi

upgrade: ## Back up the database, install the new build, migrate and restart
	@[ -f target/release/$(BIN) ] || { echo "run 'make release' first"; exit 1; }
	@if [ -x '$(INSTALLED)' ]; then '$(INSTALLED)' --config '$(CONFIG)' db backup || echo "note: automatic backup skipped (see message above)"; fi
	@$(MAKE) --no-print-directory install
	'$(INSTALLED)' --config '$(CONFIG)' check-config
	@if systemctl is-active --quiet $(SERVICE); then $(SUDO) systemctl restart $(SERVICE) && echo "service restarted"; else echo "service is not running; start it with 'make start'"; fi

config: ## Create missing configuration files from the examples (never overwrites)
	@for f in smsgateway sim integrations addressbook anonymous; do \
		if [ -e '$(CONFDIR)'/$$f.conf ]; then echo "kept $(CONFDIR)/$$f.conf"; \
		else $(SUDO) install -D -m 0640 config/$$f.conf.example '$(CONFDIR)'/$$f.conf && echo "created $(CONFDIR)/$$f.conf"; fi; done

check-config: ## Validate the configuration (CONFIG=path, default the installed one, else the examples)
	@if [ -r '$(CONFIG)' ]; then $(CARGO) run --locked --quiet -- --config '$(CONFIG)' check-config; \
	else echo "$(CONFIG) not readable; validating the example configuration instead"; $(CARGO) test --locked --quiet --test examples; fi

migrate: ## Apply database migrations
	'$(INSTALLED)' --config '$(CONFIG)' db migrate

backup: ## Create a verified database backup (SQLite) in the backups directory
	'$(INSTALLED)' --config '$(CONFIG)' db backup

restore: ## Restore a backup: make restore BACKUP=/path/to/file (stops the service)
	@[ -n "$(BACKUP)" ] || { echo "usage: make restore BACKUP=/path/to/messages-YYYYMMDDTHHMMSSZ.db"; exit 1; }
	@printf 'This replaces the current message database with %s. Type "restore" to continue: ' '$(BACKUP)'; read a; [ "$$a" = "restore" ] || { echo aborted; exit 1; }
	$(SUDO) systemctl stop $(SERVICE)
	$(SUDO) -u $(SERVICE) '$(INSTALLED)' --config '$(CONFIG)' db restore '$(BACKUP)' --yes
	$(SUDO) systemctl start $(SERVICE)

doctor: ## Diagnose the installation (uses the installed binary when present)
	@if [ -x '$(INSTALLED)' ] && [ -r '$(CONFIG)' ]; then '$(INSTALLED)' --config '$(CONFIG)' doctor; \
	else $(CARGO) run --locked --quiet -- --config '$(CONFIG)' doctor; fi

## --- Service ----------------------------------------------------------------

start: ## Start the service
	$(SUDO) systemctl start $(SERVICE)

stop: ## Stop the service
	$(SUDO) systemctl stop $(SERVICE)

restart: ## Restart the service
	$(SUDO) systemctl restart $(SERVICE)

reload: ## Reload the configuration without restarting (SIGHUP)
	$(SUDO) systemctl reload $(SERVICE)

status: ## Service and gateway status
	@systemctl --no-pager status $(SERVICE) || true
	@[ -x '$(INSTALLED)' ] && '$(INSTALLED)' --config '$(CONFIG)' status || true

enable: ## Start the service at boot
	$(SUDO) systemctl enable $(SERVICE)

disable: ## Do not start the service at boot
	$(SUDO) systemctl disable $(SERVICE)

logs: ## Follow the service journal
	$(SUDO) journalctl -u $(SERVICE) -f

logs-messages: ## Follow the message log
	$(SUDO) tail -F /var/log/$(SERVICE)/messages.log

logs-system: ## Follow the system log
	$(SUDO) tail -F /var/log/$(SERVICE)/system.log

simulator: ## Run the software modem on a pseudo-terminal (development)
	$(CARGO) run --locked --quiet -- simulator --link /tmp/ttySMSGW

## --- Distribution -----------------------------------------------------------

sbom: ## Write a CycloneDX software bill of materials to dist/
	sh scripts/sbom.sh dist/$(BIN)-$(VERSION).sbom.cdx.json

package: release sbom ## Release archive, SHA-256 checksums and SBOM in dist/
	@a=$$( [ "$(HOST_ARCH)" = "aarch64" ] && echo arm64 || echo amd64 ); \
	name=$(BIN)-$(VERSION)-linux-$$a; stage=dist/$$name; \
	mkdir -p "$$stage/bin" && \
	cp target/release/$(BIN) "$$stage/bin/" && \
	cp -r config systemd scripts docs README.md LICENSE CHANGELOG.md SECURITY.md Makefile "$$stage/" 2>/dev/null; \
	tar -C dist --owner=0 --group=0 --numeric-owner -czf "dist/$$name.tar.gz" "$$name" && \
	find "dist/$$name" -depth -delete && \
	(cd dist && sha256sum *.tar.gz *.json $$(ls *.deb 2>/dev/null) > SHA256SUMS) && \
	echo "wrote dist/$$name.tar.gz and dist/SHA256SUMS"

deb: release ## Optional Debian package in dist/ (needs dpkg-deb)
	@a=$$( [ "$(HOST_ARCH)" = "aarch64" ] && echo arm64 || echo amd64 ); \
	sh scripts/build-deb.sh target/release/$(BIN) $$a $(VERSION)

clean: ## Remove build artefacts
	$(CARGO) clean

distclean: clean ## Remove build artefacts and dist/
	@if [ -d dist ]; then find dist -depth -delete; fi
	@echo "removed build output (configuration and data are never touched)"
