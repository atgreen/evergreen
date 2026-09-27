# Local binary distribution, built from source by packaging/fedora/build.py.
# No debuginfo/strip pass may touch executables AFTER an image is appended.
%global debug_package %{nil}
%global __brp_strip %{nil}
%global __brp_strip_comment_note %{nil}
%global __brp_strip_static_archive %{nil}
%global _build_id_links none
# Foreign ELF libraries must not satisfy or require host ELF capabilities.
%global __requires_exclude_from ^%{_libexecdir}/torcl/.*$
%global __provides_exclude_from ^%{_libexecdir}/torcl/.*$

Name: torcl
Version: %{torcl_version}
Release: 4%{?dist}
Summary: Common Lisp with a tiered JIT and saved executable images
License: MIT OR Apache-2.0
URL: https://github.com/atgreen/torcl
Source0: torcl-payload.tar.gz
Source1: torcl-source.tar.gz
BuildRequires: python3
BuildRequires: gcc
BuildRequires: make
BuildRequires: java-devel >= 17
BuildRequires: python3-mkdocs
BuildRequires: python3-mkdocs-material
Requires: java-headless >= 17
Requires: which
Requires: coreutils
%if !0%{?torcl_rustup}
BuildRequires: cargo
%endif
ExclusiveArch: x86_64

%description
TorCL for Fedora x86-64, dynamically linked against glibc, with ASDF preloaded.
Includes the JAVA and TORCL-JVM APIs, their native JNI bridge, and the HTML
manual under %{_docdir}/torcl/manual/index.html.
Optional target packages dump applications for other platforms through QEMU
or Wine, without containers or a compiler on the user's machine.

%package target-s390x-linux
Summary: TorCL image-dumping tools for IBM Z Linux
License: (MIT OR Apache-2.0) AND LGPL-2.1-or-later AND (GPL-3.0-or-later WITH GCC-exception-3.1)
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-s390x

%description target-s390x-linux
An s390x TorCL runtime, private Fedora runtime libraries, and a QEMU launcher.

%package target-aarch64-linux
Summary: TorCL image-dumping tools for AArch64 Linux
License: (MIT OR Apache-2.0) AND LGPL-2.1-or-later AND (GPL-3.0-or-later WITH GCC-exception-3.1)
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-aarch64

%description target-aarch64-linux
An AArch64 TorCL runtime, private Fedora runtime libraries, and a QEMU launcher.

%package target-windows
Summary: TorCL image-dumping tools for Windows x86-64
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/wine

%description target-windows
A Windows x86-64 TorCL runtime and a Wine launcher with a private Wine prefix.

%package target-android
Summary: TorCL Android application runtimes and project generator
License: (MIT OR Apache-2.0) AND BSD-2-Clause AND BSD-3-Clause
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-aarch64
Requires: python3
Requires: make

%description target-android
Reusable Android NativeActivity libraries for ARM64 phones and x86-64 emulators,
plus torcl-android-new and Makefile templates for building signed APKs on Linux.
The Android SDK and a JDK are needed for APK packaging; Rust and the NDK are
needed only when building these RPMs. Also includes the static AArch64
command-line runtime and QEMU launcher.

%prep
%setup -q -n payload -a 1

%build
# The Java helper classes are embedded in the native bridge; no JDK or build
# tools are needed by installed users. Keep this ELF outside the cross excludes.
python3 torcl-source/packaging/fedora/native-content.py --stage "$PWD" \
    --libdir "%{_libdir}" --datadir "%{_datadir}" --docdir "%{_docdir}"
# Existing command-line payloads were image-dumped before rpmbuild. Compile
# both reusable application libraries here from Source1, using vendored crates
# and an explicitly supplied local NDK. No network or containers in this step.
%{!?android_ndk:%{error:Pass --define 'android_ndk /absolute/path/to/android-ndk' (r27d or newer)}}
python3 torcl-source/packaging/android/build-runtime.py \
    --ndk "%{android_ndk}" --stage "$PWD" --offline

%check
python3 torcl-source/packaging/fedora/test-native-content.py
python3 torcl-source/packaging/android/test_generator.py
python3 torcl-source/packaging/android/test_build.py
python3 torcl-source/packaging/android/test_install_tools.py

%install
mkdir -p %{buildroot}
cp -a usr %{buildroot}/

%files
%{_bindir}/torcl
%{_libdir}/torcl
%dir %{_datadir}/common-lisp
%dir %{_datadir}/common-lisp/source
%{_datadir}/common-lisp/source/torcl-jvm
%dir %{_libexecdir}/torcl
%doc %{_docdir}/torcl

%files target-s390x-linux
%{_bindir}/torcl-s390x-linux
%{_libexecdir}/torcl/s390x-linux
%license %{_datadir}/licenses/torcl-target-s390x-linux

%files target-aarch64-linux
%{_bindir}/torcl-aarch64-linux
%{_libexecdir}/torcl/aarch64-linux
%license %{_datadir}/licenses/torcl-target-aarch64-linux

%files target-windows
%{_bindir}/torcl-windows
%{_libexecdir}/torcl/windows

%files target-android
%{_bindir}/torcl-android
%{_bindir}/torcl-android-new
%{_libexecdir}/torcl/android
%license %{_datadir}/licenses/torcl-target-android
