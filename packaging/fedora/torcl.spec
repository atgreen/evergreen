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
Release: 1%{?dist}
Summary: Common Lisp with a tiered JIT and saved executable images
License: MIT OR Apache-2.0
URL: https://github.com/atgreen/torcl
Source0: torcl-payload.tar.gz
ExclusiveArch: x86_64

%description
TorCL for Fedora x86-64, dynamically linked against glibc, with ASDF preloaded.
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
Summary: TorCL image-dumping tools for Android AArch64
License: (MIT OR Apache-2.0) AND BSD-2-Clause AND BSD-3-Clause
Requires: %{name} = %{version}-%{release}
Requires: /usr/bin/qemu-aarch64

%description target-android
An Android AArch64 TorCL runtime statically linked with bionic, and a QEMU
launcher. Produces Android command-line executables, not APKs. Dynamic Android
library loading is unavailable in this static profile.

%prep
%setup -q -n payload

%build
# Payload was compiled, stripped, image-dumped and verified before rpmbuild.

%install
mkdir -p %{buildroot}
cp -a usr %{buildroot}/

%files
%{_bindir}/torcl
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
%{_libexecdir}/torcl/android
%license %{_datadir}/licenses/torcl-target-android
