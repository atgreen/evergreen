# Bliss — Common Lisp implementation in Rust.
#
# Default target builds the workspace. `make check` runs the runtime test
# suite with --nocapture (per project convention for reproducing timing/output
# behaviour). Run `make help` for the full target list.

CARGO ?= cargo

.DEFAULT_GOAL := build
.PHONY: build check test test-rt test-cli clippy fmt fmt-check clean release run help

## build: compile the whole workspace (default target)
build:
	$(CARGO) build --workspace

## check: run the runtime (bliss-rt) test suite with output shown
check:
	$(CARGO) test -p bliss-rt -- --nocapture

## test: run the entire workspace test suite
test:
	$(CARGO) test --workspace

## test-rt: run only the bliss-rt (runtime) tests
test-rt:
	$(CARGO) test -p bliss-rt

## test-cli: run only the bliss-cli (interpreter) tests
test-cli:
	$(CARGO) test -p bliss-cli

## clippy: lint the workspace, denying warnings
clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

## fmt: format all sources
fmt:
	$(CARGO) fmt --all

## fmt-check: verify formatting without modifying files
fmt-check:
	$(CARGO) fmt --all -- --check

## release: build the workspace with optimizations
release:
	$(CARGO) build --workspace --release

## run: build and start the bliss CLI (pass args via ARGS=...)
run:
	$(CARGO) run -p bliss-cli -- $(ARGS)

## clean: remove build artifacts
clean:
	$(CARGO) clean

## help: list available targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## /  /'
