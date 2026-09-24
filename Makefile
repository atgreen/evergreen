# TorCL — Common Lisp implementation in Rust.
#
# Default target builds the workspace. `make check` runs the runtime test
# suite with --nocapture (per project convention for reproducing timing/output
# behaviour). Run `make help` for the full target list.

CARGO ?= cargo
INSTALL ?= install

# Install location. PREFIX is where `torcl` lives (baked into the path); DESTDIR
# is an optional staging prefix prepended for packaging (e.g. DESTDIR=/tmp/pkg).
# Override either: `make install PREFIX=/opt` or `make install DESTDIR=…`.
PREFIX ?= /usr/local
DESTDIR ?=
BINDIR := $(DESTDIR)$(PREFIX)/bin

# The release torcl (musl is the default target; see .cargo/config.toml) and
# the standalone `torcl` executable dumped from it (runtime + saved image with
# ASDF preloaded).
RELEASE_BIN := target/x86_64-unknown-linux-musl/release/torcl
TORCL_EXE := target/torcl

.DEFAULT_GOAL := build
.PHONY: build check test test-rt test-cli clippy fmt fmt-check clean release run \
        help image pgo-image image-no-pgo test-pgo-build test-library-forks install uninstall

## build: compile the whole workspace (default target)
build:
	$(CARGO) build --workspace

## check: run the runtime (torcl-rt) test suite with output shown
check:
	$(CARGO) test -p torcl-rt -- --nocapture

## test: run the entire workspace test suite
test:
	$(CARGO) test --workspace

## test-rt: run only the torcl-rt (runtime) tests
test-rt:
	$(CARGO) test -p torcl-rt

## test-cli: run only the torcl (interpreter) tests
test-cli:
	$(CARGO) test -p torcl

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

## run: build and start the torcl CLI (pass args via ARGS=...)
run:
	$(CARGO) run -p torcl -- $(ARGS)

## image: build a profile-guided standalone executable with ASDF preloaded
# Run this as your normal user — it invokes cargo. `install` only copies the
# result, so `sudo make install` needs no cargo in root's PATH.
image: $(TORCL_EXE)

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
.PHONY: $(TORCL_EXE)
$(TORCL_EXE):
	CARGO="$(CARGO)" TORCL_IMAGE_OUT="$(or $(TORCL_IMAGE_OUT),$(TORCL_EXE))" bash scripts/build-pgo-image.sh

## image-no-pgo: build an ordinary release image without training or llvm-profdata
image-no-pgo: release
	TORCL_IMAGE_OUT=$(TORCL_EXE) $(RELEASE_BIN) --no-init --load scripts/build-image.lisp

## install: install `torcl` (ASDF-preloaded executable) to $(DESTDIR)$(PREFIX)/bin
# Build first with `make image` (as your user), then `sudo make install`. This
# target only copies — it never runs cargo, so it works under sudo where cargo
# is not on root's PATH.
install:
	@test -x $(TORCL_EXE) || { \
	  echo "error: $(TORCL_EXE) not found."; \
	  echo "Build it first as your normal user:  make image"; \
	  echo "then install as root:                sudo make install"; \
	  exit 1; }
	$(INSTALL) -d $(BINDIR)
	$(INSTALL) -m 755 $(TORCL_EXE) $(BINDIR)/torcl
	@echo "installed $(BINDIR)/torcl"

## uninstall: remove the installed `torcl`
uninstall:
	rm -f $(BINDIR)/torcl
	@echo "removed $(BINDIR)/torcl"

## clean: remove build artifacts
clean:
	$(CARGO) clean

## help: list available targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## /  /'
