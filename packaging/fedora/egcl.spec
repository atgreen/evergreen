# Local binary distribution, built from source by packaging/fedora/build.py.
# No debuginfo/strip pass may touch executables AFTER an image is appended.
%global debug_package %{nil}
%global __brp_strip %{nil}
%global __brp_strip_comment_note %{nil}
%global __brp_strip_static_archive %{nil}
%global _build_id_links none
# Foreign ELF libraries must not satisfy or require host ELF capabilities.
%global __requires_exclude_from ^%{_libexecdir}/egcl/.*$
%global __provides_exclude_from ^%{_libexecdir}/egcl/.*$

Name: egcl
Version: %{egcl_version}
Release: 6%{?dist}
Summary: Evergreen Common Lisp — a tiered JIT and saved executable images
License: GPL-3.0-or-later WITH Classpath-exception-2.0
URL: https://github.com/atgreen/evergreen
Source0: egcl-payload.tar.gz
Source1: egcl-source.tar.gz
BuildRequires: python3
BuildRequires: gcc
BuildRequires: make
BuildRequires: java-devel >= 17
# Fedora ships these as `mkdocs` / `mkdocs-material`, NOT under a python3-
# prefix, and neither name carries a compat provide -- so the prefixed spelling
# is unsatisfiable and rpmbuild refuses the build outright.
BuildRequires: mkdocs
BuildRequires: mkdocs-material
Requires: java-headless >= 17
Requires: which
Requires: coreutils
%if !0%{?egcl_rustup}
BuildRequires: cargo
%endif
ExclusiveArch: x86_64

%description
Evergreen Common Lisp (EGCL) for Fedora x86-64, dynamically linked against
glibc, with ASDF preloaded.
Includes the JAVA and EGCL-JVM APIs, their native JNI bridge, and the HTML
manual under %{_docdir}/egcl/manual/index.html.
Optional target packages dump applications for other platforms through QEMU
or Wine, without containers or a compiler on the user's machine.

%package target-s390x-linux
Summary: EGCL image-dumping tools for IBM Z Linux
License: (GPL-3.0-or-later WITH Classpath-exception-2.0) AND LGPL-2.1-or-later AND (GPL-3.0-or-later WITH GCC-exception-3.1)
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-s390x

%description target-s390x-linux
An s390x EGCL runtime, private Fedora runtime libraries, and a QEMU launcher.

%package target-aarch64-linux
Summary: EGCL image-dumping tools for AArch64 Linux
License: (GPL-3.0-or-later WITH Classpath-exception-2.0) AND LGPL-2.1-or-later AND (GPL-3.0-or-later WITH GCC-exception-3.1)
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-aarch64

%description target-aarch64-linux
An AArch64 EGCL runtime, private Fedora runtime libraries, and a QEMU launcher.

%package target-ppc64le-linux
Summary: EGCL image-dumping tools for little-endian POWER Linux
License: (GPL-3.0-or-later WITH Classpath-exception-2.0) AND LGPL-2.1-or-later AND (GPL-3.0-or-later WITH GCC-exception-3.1)
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-ppc64le

%description target-ppc64le-linux
A ppc64le EGCL runtime, private Fedora runtime libraries, and a QEMU launcher.

%package target-windows
Summary: EGCL image-dumping tools for Windows x86-64
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/wine

%description target-windows
A Windows x86-64 EGCL runtime and a Wine launcher with a private Wine prefix.

%package target-android
Summary: EGCL Android application runtimes and project generator
License: (GPL-3.0-or-later WITH Classpath-exception-2.0) AND BSD-2-Clause AND BSD-3-Clause
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-aarch64
Requires: python3
Requires: make

%description target-android
Reusable Android NativeActivity libraries for ARM64 phones and x86-64 emulators,
plus egcl-android-new and Makefile templates for building signed APKs on Linux.
The Android SDK and a JDK are needed for APK packaging; Rust and the NDK are
needed only when building these RPMs. Also includes the static AArch64
command-line runtime and QEMU launcher.

%prep
%setup -q -n payload -a 1

%build
# The Java helper classes are embedded in the native bridge; no JDK or build
# tools are needed by installed users. Keep this ELF outside the cross excludes.
python3 egcl-source/packaging/fedora/native-content.py --stage "$PWD" \
    --libdir "%{_libdir}" --datadir "%{_datadir}" --docdir "%{_docdir}"
# Existing command-line payloads were image-dumped before rpmbuild. Compile
# both reusable application libraries here from Source1, using vendored crates
# and an explicitly supplied local NDK. No network or containers in this step.
%{!?android_ndk:%{error:Pass --define 'android_ndk /absolute/path/to/android-ndk' (r27d or newer)}}
python3 egcl-source/packaging/android/build-runtime.py \
    --ndk "%{android_ndk}" --stage "$PWD" --offline

%check
python3 egcl-source/packaging/fedora/test-native-content.py
python3 egcl-source/packaging/fedora/test-cross-launcher.py
python3 egcl-source/packaging/android/test_generator.py
python3 egcl-source/packaging/android/test_build.py
python3 egcl-source/packaging/android/test_install_tools.py

%install
mkdir -p %{buildroot}
cp -a usr %{buildroot}/

%files
%{_bindir}/egcl
%{_libdir}/egcl
%dir %{_datadir}/common-lisp
%dir %{_datadir}/common-lisp/source
%{_datadir}/common-lisp/source/egcl-jvm
%dir %{_libexecdir}/egcl
%doc %{_docdir}/egcl

%files target-s390x-linux
%{_bindir}/egcl-s390x-linux
%{_libexecdir}/egcl/s390x-linux
%license %{_datadir}/licenses/egcl-target-s390x-linux

%files target-aarch64-linux
%{_bindir}/egcl-aarch64-linux
%{_libexecdir}/egcl/aarch64-linux
%license %{_datadir}/licenses/egcl-target-aarch64-linux

%files target-ppc64le-linux
%{_bindir}/egcl-ppc64le-linux
%{_libexecdir}/egcl/ppc64le-linux
%license %{_datadir}/licenses/egcl-target-ppc64le-linux

%files target-windows
%{_bindir}/egcl-windows
%{_libexecdir}/egcl/windows

%files target-android
%{_bindir}/egcl-android
%{_bindir}/egcl-android-new
%{_libexecdir}/egcl/android
%license %{_datadir}/licenses/egcl-target-android
