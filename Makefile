# Bliss — Common Lisp implementation in Rust.
#
# Default target builds the workspace. `make check` runs the runtime test
# suite with --nocapture (per project convention for reproducing timing/output
# behaviour). Run `make help` for the full target list.

CARGO ?= cargo
INSTALL ?= install

# Install location. PREFIX is where `bliss` lives (baked into the path); DESTDIR
# is an optional staging prefix prepended for packaging (e.g. DESTDIR=/tmp/pkg).
# Override either: `make install PREFIX=/opt` or `make install DESTDIR=…`.
PREFIX ?= /usr/local
DESTDIR ?=
BINDIR := $(DESTDIR)$(PREFIX)/bin

# The release bliss-cli (musl is the default target; see .cargo/config.toml) and
# the standalone `bliss` executable dumped from it (runtime + saved image with
# ASDF preloaded).
RELEASE_BIN := target/x86_64-unknown-linux-musl/release/bliss-cli
BLISS_EXE := target/bliss

.DEFAULT_GOAL := build
.PHONY: build check test test-rt test-cli clippy fmt fmt-check clean release run \
        help image install uninstall

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

## image: dump the standalone `bliss` executable (runtime + ASDF-loaded image)
# Run this as your normal user — it invokes cargo. `install` only copies the
# result, so `sudo make install` needs no cargo in root's PATH.
image: $(BLISS_EXE)

# Depends on the phony `release` so cargo (the source of truth for freshness)
# always runs — a bare file dependency on $(RELEASE_BIN) would let make skip the
# rebuild after Rust sources change. `install` does NOT depend on this, so it
# needs no cargo.
$(BLISS_EXE): release scripts/build-image.lisp
	BLISS_IMAGE_OUT=$(BLISS_EXE) $(RELEASE_BIN) --no-init --load scripts/build-image.lisp

## install: install `bliss` (ASDF-preloaded executable) to $(DESTDIR)$(PREFIX)/bin
# Build first with `make image` (as your user), then `sudo make install`. This
# target only copies — it never runs cargo, so it works under sudo where cargo
# is not on root's PATH.
install:
	@test -x $(BLISS_EXE) || { \
	  echo "error: $(BLISS_EXE) not found."; \
	  echo "Build it first as your normal user:  make image"; \
	  echo "then install as root:                sudo make install"; \
	  exit 1; }
	$(INSTALL) -d $(BINDIR)
	$(INSTALL) -m 755 $(BLISS_EXE) $(BINDIR)/bliss
	@echo "installed $(BINDIR)/bliss"

## uninstall: remove the installed `bliss`
uninstall:
	rm -f $(BINDIR)/bliss
	@echo "removed $(BINDIR)/bliss"

## clean: remove build artifacts
clean:
	$(CARGO) clean

## help: list available targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## /  /'
