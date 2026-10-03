.DEFAULT_GOAL := apk
.DELETE_ON_ERROR:
-include local.mk

EGCL_SRC ?= $(abspath ../..)
# SBCL builds this about 6x faster than EGCL (1.0 s against 6.2 s) and has ASDF
# built in, so it is the default while iterating. Set LISP to an egcl to build
# self-hosted; it needs the (require :asdf) that SBCL does not.
LISP ?= sbcl
EGCL_APK_RUNTIME ?= /usr/libexec/egcl/android/
SDK ?= $(if $(ANDROID_HOME),$(ANDROID_HOME),$(if $(ANDROID_SDK_ROOT),$(ANDROID_SDK_ROOT),$(HOME)/Android/Sdk))
BUILD_TOOLS ?= $(SDK)/build-tools/34.0.0
ADB ?= adb
SERIAL ?=
DEVICE = $(ADB) $(if $(SERIAL),-s $(SERIAL))
PACKAGE := org.egcl.example.egl
APK := build/android-egl.apk

# The builder and its pinned dependencies are not in the tree ASDF searches by
# default when working from a source checkout, so point ASDF at them. An
# INSTALLED egcl-target-android needs none of this: the three --eval forms alone
# are the whole build.
LISP_FLAGS = $(if $(filter sbcl%,$(notdir $(LISP))),\
	       --noinform --no-userinit --no-sysinit --non-interactive,--no-init)
export CL_SOURCE_REGISTRY = (:source-registry (:tree "$(EGCL_SRC)/lib/egcl-apk/") :ignore-inherited-configuration)

.PHONY: apk verify install run logcat clean help
# The builder reads current assets and reuses the persistent signing identity.
apk:
	EGCL_APK_RUNTIME="$(EGCL_APK_RUNTIME)" "$(LISP)" $(LISP_FLAGS) \
	  --eval '(require :asdf)' \
	  --eval '(asdf:load-asd (truename "android-egl.asd"))' \
	  --eval '(asdf:make "android-egl/apk")'

# SDK tools are independent validators, not build dependencies.
verify: apk
	"$(BUILD_TOOLS)/apksigner" verify --verbose "$(APK)"
	"$(BUILD_TOOLS)/zipalign" -c 4 "$(APK)"
	"$(BUILD_TOOLS)/aapt2" dump badging "$(APK)"

install: apk
	$(DEVICE) install -r --user 0 "$(APK)"

run:
	$(DEVICE) shell am start -W -n $(PACKAGE)/android.app.NativeActivity

logcat:
	$(DEVICE) logcat 'egcl:V' 'AndroidRuntime:E' 'libc:F' '*:S'

# Preserve the signing identity so later installs can update this app.
clean:
	rm -rf build

help:
	@echo 'make             Build and sign build/android-egl.apk using Common Lisp'
	@echo 'make verify      Check signature, alignment and manifest with SDK tools'
	@echo 'make install     Build and install on the attached phone'
	@echo 'make run         Launch the installed app'
	@echo 'make logcat      Show runtime diagnostics (Ctrl-C to stop)'
	@echo 'Set LISP and EGCL_APK_RUNTIME in local.mk; SERIAL selects a phone.'
