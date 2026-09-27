.DEFAULT_GOAL := apk
-include local.mk
HOST ?= @HOST@
HOSTS ?= $(HOST)
RUNTIME ?= /usr/libexec/torcl/android
SDK ?= $(if $(ANDROID_HOME),$(ANDROID_HOME),$(if $(ANDROID_SDK_ROOT),$(ANDROID_SDK_ROOT),$(HOME)/Android/Sdk))
BUILD_TOOLS ?=
ANDROID_JAR ?=
SERIAL ?=
export KEYSTORE KEY_ALIAS KEYSTORE_PASSWORD KEY_PASSWORD

.PHONY: apk release verify doctor install run logcat clean help
apk release verify doctor install run logcat:
	python3 "$(RUNTIME)/build-apk.py" $@ --hosts "$(HOSTS)" --runtime "$(RUNTIME)" \
	  --sdk "$(SDK)" --build-tools "$(BUILD_TOOLS)" --android-jar "$(ANDROID_JAR)" --serial "$(SERIAL)"

clean:
	rm -rf build

help:
	@echo 'make [HOST=aarch64-linux-android|x86_64-linux-android]'
	@echo 'make HOSTS="aarch64-linux-android x86_64-linux-android"'
	@echo 'Targets: apk release verify doctor install run logcat clean'
	@echo 'Select a phone/emulator with SERIAL=...; set SDK in local.mk.'
