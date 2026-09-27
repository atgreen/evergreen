JAVA_HOME ?= $(shell dirname $$(dirname $$(readlink -f $$(command -v javac))))
CC ?= cc
.DEFAULT_GOAL := all
.PHONY: all clean test
all: build/libtorcl_jvm.so
build/Bridge.class: java/org/torcl/jvm/Bridge.java
	mkdir -p build/classes
	"$(JAVA_HOME)/bin/javac" --release 17 -d build/classes $<
	cp build/classes/org/torcl/jvm/Bridge.class $@
build/bridge_class.h: build/Bridge.class
	python3 -c 'from pathlib import Path; b=Path("$<").read_bytes(); Path("$@").write_text("static const unsigned char bridge_class[] = {" + ",".join(map(str,b)) + "};\n")'
build/libtorcl_jvm.so: native.c build/bridge_class.h
	$(CC) -std=c11 -O2 -g -fPIC -shared -Wall -Wextra -Werror -Ibuild -I"$(JAVA_HOME)/include" -I"$(JAVA_HOME)/include/linux" native.c -ldl -lpthread -o $@
clean:
	rm -rf build

build/tests/JvmFixture.class: tests/JvmFixture.java
	mkdir -p build/tests
	"$(JAVA_HOME)/bin/javac" --release 17 -d build/tests $<
test: all build/tests/JvmFixture.class
	./tests/run.sh
