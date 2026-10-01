.DEFAULT_GOAL := apk
.DELETE_ON_ERROR:
-include local.mk

EGCL_SRC ?= $(abspath ../..)
SBCL_BIN ?= sbcl
EGCL_APK_RUNTIME ?= /usr/libexec/egcl/android/
SDK ?= $(if $(ANDROID_HOME),$(ANDROID_HOME),$(if $(ANDROID_SDK_ROOT),$(ANDROID_SDK_ROOT),$(HOME)/Android/Sdk))
BUILD_TOOLS ?= $(SDK)/build-tools/34.0.0
ADB ?= adb
SERIAL ?=
DEVICE = $(ADB) $(if $(SERIAL),-s $(SERIAL))
PACKAGE := org.egcl.example.egl
APK := build/android-egl.apk

.PHONY: apk verify install run logcat clean help
# The builder reads current assets and reuses the persistent signing identity.
apk:
	SBCL_BIN="$(SBCL_BIN)" EGCL_APK_RUNTIME="$(EGCL_APK_RUNTIME)" \
	  "$(EGCL_SRC)/scripts/egcl-apk" "$(CURDIR)"

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
	@echo 'Set SBCL_BIN and EGCL_APK_RUNTIME in local.mk; SERIAL selects a phone.'
