# One source RPM supports a full rebuild or independent package groups.
# No debuginfo/strip pass may touch executables AFTER an image is appended.
%global debug_package %{nil}
%global __brp_strip %{nil}
%global __brp_strip_comment_note %{nil}
%global __brp_strip_static_archive %{nil}
%global _build_id_links none
# Foreign ELF libraries must not satisfy or require host ELF capabilities.
%global __requires_exclude_from ^%{_libexecdir}/egcl/.*$
%global __provides_exclude_from ^%{_libexecdir}/egcl/.*$

%{!?egcl_build_group:%global egcl_build_group all}
%{!?egcl_prebuilt:%global egcl_prebuilt 0}
%{!?egcl_tools:%global egcl_tools %{_topdir}/tools}
%{!?android_ndk:%global android_ndk %{egcl_tools}/android-ndk-r27d}
%global egcl_native 0
%if "%{egcl_build_group}" == "all" || "%{egcl_build_group}" == "native"
%global egcl_native 1
%endif
%global egcl_s390x 0
%if "%{egcl_build_group}" == "all" || "%{egcl_build_group}" == "s390x"
%global egcl_s390x 1
%endif
%global egcl_aarch64 0
%if "%{egcl_build_group}" == "all" || "%{egcl_build_group}" == "aarch64"
%global egcl_aarch64 1
%endif
%global egcl_ppc64le 0
%if "%{egcl_build_group}" == "all" || "%{egcl_build_group}" == "ppc64le"
%global egcl_ppc64le 1
%endif
%global egcl_windows 0
%if "%{egcl_build_group}" == "all" || "%{egcl_build_group}" == "windows"
%global egcl_windows 1
%endif
%global egcl_android 0
%if "%{egcl_build_group}" == "all" || "%{egcl_build_group}" == "android"
%global egcl_android 1
%endif
%if %{egcl_prebuilt}
%global egcl_stage payload
%else
%global egcl_stage target/fedora-rpm/stage
%endif

Name: egcl
Version: %{egcl_version}
%{!?egcl_release:%global egcl_release 6}
Release: %{egcl_release}%{?dist}
Summary: Evergreen Common Lisp — a tiered JIT and saved executable images
License: GPL-3.0-or-later WITH Classpath-exception-2.0
URL: https://github.com/atgreen/evergreen
Source0: egcl-source.tar.gz
%if %{egcl_prebuilt}
Source1: egcl-payload.tar.gz
%else
Source2: musl-1.2.5.tar.gz
Source3: libunwind-21.1.8.src.tar.xz
%endif
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

%if %{egcl_native}
%package static
Summary: Statically linked musl EGCL runtime

%description static
EGCL for Linux x86-64, statically linked against musl, with ASDF preloaded.
The egcl-static command runs without a system dynamic loader and produces
statically linked saved executables. Install egcl for the glibc-based runtime
and JVM integration.
%endif

%if %{egcl_s390x}
%package target-s390x-linux
Summary: EGCL image-dumping tools for IBM Z Linux
License: (GPL-3.0-or-later WITH Classpath-exception-2.0) AND LGPL-2.1-or-later AND (GPL-3.0-or-later WITH GCC-exception-3.1)
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-s390x

%description target-s390x-linux
An s390x EGCL runtime, private Fedora runtime libraries, and a QEMU launcher.
%endif

%if %{egcl_aarch64}
%package target-aarch64-linux
Summary: EGCL image-dumping tools for AArch64 Linux
License: (GPL-3.0-or-later WITH Classpath-exception-2.0) AND LGPL-2.1-or-later AND (GPL-3.0-or-later WITH GCC-exception-3.1)
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-aarch64

%description target-aarch64-linux
An AArch64 EGCL runtime, private Fedora runtime libraries, and a QEMU launcher.
%endif

%if %{egcl_ppc64le}
%package target-ppc64le-linux
Summary: EGCL image-dumping tools for little-endian POWER Linux
License: (GPL-3.0-or-later WITH Classpath-exception-2.0) AND LGPL-2.1-or-later AND (GPL-3.0-or-later WITH GCC-exception-3.1)
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-ppc64le

%description target-ppc64le-linux
A ppc64le EGCL runtime, private Fedora runtime libraries, and a QEMU launcher.
%endif

%if %{egcl_s390x}
%package target-s390x-linux-static
Summary: Static musl EGCL image-dumping tools for IBM Z Linux
License: (GPL-3.0-or-later WITH Classpath-exception-2.0) AND MIT AND (Apache-2.0 WITH LLVM-exception)
Requires: /usr/bin/qemu-s390x

%description target-s390x-linux-static
A statically linked musl s390x EGCL runtime and QEMU launcher. Produces
standalone executables without a target sysroot or shared-library dependencies.
%endif

%if %{egcl_aarch64}
%package target-aarch64-linux-static
Summary: Static musl EGCL image-dumping tools for AArch64 Linux
Requires: /usr/bin/qemu-aarch64

%description target-aarch64-linux-static
A statically linked musl AArch64 EGCL runtime and QEMU launcher. Produces
standalone executables without a target sysroot or shared-library dependencies.
%endif

%if %{egcl_ppc64le}
%package target-ppc64le-linux-static
Summary: Static musl EGCL image-dumping tools for little-endian POWER Linux
Requires: /usr/bin/qemu-ppc64le

%description target-ppc64le-linux-static
A statically linked musl ppc64le EGCL runtime and QEMU launcher. Produces
standalone executables without a target sysroot or shared-library dependencies.
%endif

%if %{egcl_windows}
%package target-windows
Summary: EGCL image-dumping tools for Windows x86-64
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/wine

%description target-windows
A Windows x86-64 EGCL runtime and a Wine launcher with a private Wine prefix.
%endif

%if %{egcl_android}
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
%endif

%prep
%setup -q -n egcl-source
%if %{egcl_prebuilt}
tar -xf %{SOURCE1}
%endif

%build
%if !%{egcl_prebuilt}
# The SRPM contains only source and vendored dependencies. Toolchains and the
# NDK are prepared by the builder before rpmbuild; this step stays offline.
# RPM host C flags contain x86-only options; each target selects its own flags.
unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS
export CARGO_NET_OFFLINE=true
python3 packaging/fedora/build.py --stage-only --group "%{egcl_build_group}" \
    --tools "%{egcl_tools}" --android-ndk "%{android_ndk}"
%endif
%if %{egcl_native}
python3 packaging/fedora/native-content.py --stage "$PWD/%{egcl_stage}" \
    --libdir "%{_libdir}" --datadir "%{_datadir}" --docdir "%{_docdir}"
%endif
%if %{egcl_android}
python3 packaging/android/build-runtime.py \
    --ndk "%{android_ndk}" --stage "$PWD/%{egcl_stage}" --offline
%endif
cp %{egcl_stage}/usr/share/doc/egcl/build.json build-provenance.json
%if !%{egcl_native}
# The main RPM owns shared documentation. Keep provenance outside the stage
# for the release collector, without leaving unpackaged files in subpackages.
rm -rf %{egcl_stage}/usr/share/doc/egcl
%endif

%check
python3 packaging/fedora/test-native-content.py
python3 packaging/fedora/test-static-package.py
python3 packaging/fedora/test-cross-launcher.py
python3 packaging/fedora/test-launcher.py
python3 packaging/android/test_generator.py
python3 packaging/android/test_build.py
python3 packaging/android/test_install_tools.py

%install
mkdir -p %{buildroot}%{_libexecdir}/egcl
cp -a %{egcl_stage}/usr %{buildroot}/

%if %{egcl_native}
%files
%{_bindir}/egcl
%{_libdir}/egcl
%dir %{_datadir}/common-lisp
%dir %{_datadir}/common-lisp/source
%{_datadir}/common-lisp/source/egcl-jvm
%dir %{_libexecdir}/egcl
%doc %{_docdir}/egcl
%endif

%if %{egcl_native}
%files static
%{_bindir}/egcl-static
%endif

%if %{egcl_s390x}
%files target-s390x-linux
%{_bindir}/egcl-s390x-linux
%{_libexecdir}/egcl/s390x-linux
%license %{_datadir}/licenses/egcl-target-s390x-linux
%endif

%if %{egcl_aarch64}
%files target-aarch64-linux
%{_bindir}/egcl-aarch64-linux
%{_libexecdir}/egcl/aarch64-linux
%license %{_datadir}/licenses/egcl-target-aarch64-linux
%endif

%if %{egcl_ppc64le}
%files target-ppc64le-linux
%{_bindir}/egcl-ppc64le-linux
%{_libexecdir}/egcl/ppc64le-linux
%license %{_datadir}/licenses/egcl-target-ppc64le-linux
%endif

%if %{egcl_windows}
%files target-windows
%{_bindir}/egcl-windows
%{_libexecdir}/egcl/windows
%endif

%if %{egcl_s390x}
%files target-s390x-linux-static
%{_bindir}/egcl-s390x-linux-static
%dir %{_libexecdir}/egcl
%{_libexecdir}/egcl/s390x-linux-static
%license %{_datadir}/licenses/egcl-target-s390x-linux-static
%endif

%if %{egcl_aarch64}
%files target-aarch64-linux-static
%{_bindir}/egcl-aarch64-linux-static
%dir %{_libexecdir}/egcl
%{_libexecdir}/egcl/aarch64-linux-static
%endif

%if %{egcl_ppc64le}
%files target-ppc64le-linux-static
%{_bindir}/egcl-ppc64le-linux-static
%dir %{_libexecdir}/egcl
%{_libexecdir}/egcl/ppc64le-linux-static
%endif

%if %{egcl_android}
%files target-android
%{_bindir}/egcl-android
%{_bindir}/egcl-android-new
%{_libexecdir}/egcl/android
%license %{_datadir}/licenses/egcl-target-android
%endif
