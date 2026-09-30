# EGCL — Common Lisp implementation in Rust.
#
# Default target builds the workspace. `make check` runs the runtime test
# suite with --nocapture (per project convention for reproducing timing/output
# behaviour). Run `make help` for the full target list.

CARGO ?= cargo
INSTALL ?= install

# Install location. PREFIX is where `egcl` lives (baked into the path); DESTDIR
# is an optional staging prefix prepended for packaging (e.g. DESTDIR=/tmp/pkg).
# Override either: `make install PREFIX=/opt` or `make install DESTDIR=…`.
PREFIX ?= /usr/local
DESTDIR ?=
BINDIR := $(DESTDIR)$(PREFIX)/bin

# The release egcl (musl is the default target; see .cargo/config.toml) and
# the standalone `egcl` executable dumped from it (runtime + saved image with
# ASDF preloaded).
RELEASE_BIN := target/x86_64-unknown-linux-musl/release/egcl
EGCL_EXE := target/egcl

.DEFAULT_GOAL := build
.PHONY: build check test test-rt test-cli clippy fmt fmt-check clean release run \
        help image pgo-image image-no-pgo test-pgo-build test-library-forks install uninstall

## build: compile the whole workspace (default target)
build:
	$(CARGO) build --workspace

## check: run the runtime (egcl-rt) test suite with output shown
check:
	$(CARGO) test -p egcl-rt -- --nocapture

## test: run the entire workspace test suite
test:
	$(CARGO) test --workspace

## test-rt: run only the egcl-rt (runtime) tests
test-rt:
	$(CARGO) test -p egcl-rt

## test-cli: run only the egcl (interpreter) tests
test-cli:
	$(CARGO) test -p egcl

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

## run: build and start the egcl CLI (pass args via ARGS=...)
run:
	$(CARGO) run -p egcl -- $(ARGS)

## image: build a profile-guided standalone executable with ASDF preloaded
# Run this as your normal user — it invokes cargo. `install` only copies the
# result, so `sudo make install` needs no cargo in root's PATH.
image: $(EGCL_EXE)

## pgo-image: alias for image (requires matching llvm-profdata)
pgo-image: image

## test-pgo-build: test PGO orchestration and failure isolation without compiling
test-pgo-build:
	python3 scripts/test-pgo-build.py

## test-library-forks: fetch pinned ocicl forks and test cold/cached library loads
test-library-forks:
	bash scripts/test-library-forks.sh

# Always retrain for the current sources/toolchain; an existing image alone
# cannot establish profile freshness. `install` remains a copy-only operation.
.PHONY: $(EGCL_EXE)
$(EGCL_EXE):
	CARGO="$(CARGO)" EGCL_IMAGE_OUT="$(or $(EGCL_IMAGE_OUT),$(EGCL_EXE))" bash scripts/build-pgo-image.sh

## image-no-pgo: build an ordinary release image without training or llvm-profdata
image-no-pgo: release
	EGCL_IMAGE_OUT=$(EGCL_EXE) $(RELEASE_BIN) --no-init --load scripts/build-image.lisp

## install: install `egcl` (ASDF-preloaded executable) to $(DESTDIR)$(PREFIX)/bin
# Build first with `make image` (as your user), then `sudo make install`. This
# target only copies — it never runs cargo, so it works under sudo where cargo
# is not on root's PATH.
install:
	@test -x $(EGCL_EXE) || { \
	  echo "error: $(EGCL_EXE) not found."; \
	  echo "Build it first as your normal user:  make image"; \
	  echo "then install as root:                sudo make install"; \
	  exit 1; }
	@# Refuse to install an image older than the sources it was built from.
	@# This target deliberately never runs cargo (see above), so it cannot
	@# rebuild — but it can tell you that what you are about to install is not
	@# what you just changed. Without this, editing source and running
	@# `sudo make install` without re-running `make image` installed a STALE
	@# image and reported success.
	@#
	@# `-path '*/target' -prune` matters: crates/*/target/ accumulates fixture
	@# files written during test runs, and counting those would make every
	@# image look stale forever.
	@if [ -z "$(ALLOW_STALE)" ]; then \
	  stale=$$(find crates lib Cargo.toml Cargo.lock scripts/build-image.lisp \
	      -path '*/target' -prune -o -type f \
	      \( -name '*.rs' -o -name '*.lisp' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) \
	      -newer $(EGCL_EXE) -print 2>/dev/null | head -5); \
	  if [ -n "$$stale" ]; then \
	    echo "error: $(EGCL_EXE) is OLDER than these sources:"; \
	    echo "$$stale" | sed 's/^/  /'; \
	    echo "It would install a stale image. Rebuild as your normal user:"; \
	    echo "    make image"; \
	    echo "then install as root:"; \
	    echo "    sudo make install"; \
	    echo "(to install the existing image anyway: make install ALLOW_STALE=1)"; \
	    exit 1; \
	  fi; \
	  if [ -f $(RELEASE_BIN) ] && [ $(RELEASE_BIN) -nt $(EGCL_EXE) ]; then \
	    echo "error: $(RELEASE_BIN) is NEWER than $(EGCL_EXE)."; \
	    echo "The release binary was rebuilt but the image was not re-dumped from it."; \
	    echo "Rebuild the image as your normal user:  make image"; \
	    echo "(to install the existing image anyway: make install ALLOW_STALE=1)"; \
	    exit 1; \
	  fi; \
	fi
	$(INSTALL) -d $(BINDIR)
	$(INSTALL) -m 755 $(EGCL_EXE) $(BINDIR)/egcl
	@echo "installed $(BINDIR)/egcl"

## uninstall: remove the installed `egcl`
uninstall:
	rm -f $(BINDIR)/egcl
	@echo "removed $(BINDIR)/egcl"

## clean: remove build artifacts
clean:
	$(CARGO) clean

## help: list available targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## /  /'

.PHONY: docs docs-serve
## docs: build the manual (install requirements-docs.txt first)
docs:
	python3 scripts/test_docs.py
	mkdocs build --strict

## docs-serve: preview the manual on http://127.0.0.1:8000/egcl/
docs-serve:
	mkdocs serve --dev-addr 127.0.0.1:8000
