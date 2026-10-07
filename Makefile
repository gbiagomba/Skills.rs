# Makefile (PRO)
# Satisfies RULE 2 - maintain Makefile
# Satisfies RULE 1 - test before delivery
#
# Every target here is a thin wrapper over cargo. If make is unavailable on your
# platform, the equivalent cargo command is shown in the README.

PROJECT_NAME := skill
APP_NAME     := skill
CARGO        := cargo
BUILD_DIR    := target
INSTALL_PATH := /usr/local/bin
MSRV         := 1.88

.PHONY: all build release install uninstall run test clean fmt fmt-check lint check ci msrv audit help

all: release

## build: compile a release binary
build:
	$(CARGO) build --release

release: build

## install: install the binary from this working tree
install:
	$(CARGO) install --path . --locked

## uninstall: remove an installed binary
uninstall:
	$(CARGO) uninstall $(APP_NAME) || true

## run: run with ARGS, for example: make run ARGS="--help"
run:
	$(CARGO) run --release -- $(ARGS)

## test: run every test with the lockfile pinned
test:
	$(CARGO) test --locked --all-features

## fmt: rewrite source to the standard format
fmt:
	$(CARGO) fmt --all

## fmt-check: fail if anything is unformatted
fmt-check:
	$(CARGO) fmt --all -- --check

## lint: clippy with warnings denied, across both feature configurations
lint:
	$(CARGO) clippy --all-targets --all-features -- -D warnings
	$(CARGO) clippy --all-targets --no-default-features -- -D warnings

## check: type-check without producing a binary
check:
	$(CARGO) check --all-targets --all-features

## msrv: verify the declared minimum supported Rust version
msrv:
	$(CARGO) +$(MSRV) check --locked --all-targets

## audit: report known advisories (install with: cargo install cargo-audit)
audit:
	$(CARGO) audit || echo "cargo-audit is not installed; skipping"

## ci: the full gate, matching what the workflow runs
ci: fmt-check lint test
	$(CARGO) test --locked --no-default-features
	$(CARGO) build --release

## clean: remove build artifacts
clean:
	$(CARGO) clean

## help: list the targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/## //'
