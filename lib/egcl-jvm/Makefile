JAVA_HOME ?= $(shell dirname $$(dirname $$(readlink -f $$(command -v javac))))
CC ?= cc
.DEFAULT_GOAL := all
.PHONY: all clean test
all: build/libegcl_jvm.so
build/classes.stamp: java/org/egcl/jvm/Bridge.java java/org/egcl/jvm/Api.java java/org/egcl/jvm/AmbiguousCall.java
	mkdir -p build/classes
	"$(JAVA_HOME)/bin/javac" --release 17 -d build/classes $^
	touch $@
build/bridge_class.h: build/classes.stamp
	python3 -c 'from pathlib import Path; Path("$@").write_text("".join("static const unsigned char " + n.lower() + "_class[] = {" + ",".join(map(str,Path("build/classes/org/egcl/jvm/"+n+".class").read_bytes())) + "};\n" for n in ("Bridge", "Api", "AmbiguousCall")))'
build/libegcl_jvm.so: native.c build/bridge_class.h
	$(CC) -std=c11 -O2 -g -fPIC -shared -Wall -Wextra -Werror -Ibuild -I"$(JAVA_HOME)/include" -I"$(JAVA_HOME)/include/linux" native.c -ldl -lpthread -o $@
clean:
	rm -rf build

build/tests/JvmFixture.class: tests/JvmFixture.java tests/ErgonomicFixture.java
	mkdir -p build/tests
	"$(JAVA_HOME)/bin/javac" --release 17 -d build/tests $^
test: all build/tests/JvmFixture.class
	./tests/run.sh
