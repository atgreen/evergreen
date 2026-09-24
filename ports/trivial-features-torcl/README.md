# TorCL support for trivial-features

`implementation.patch` adds TorCL to ocicl's
`trivial-features-20260908-828246a`. TorCL already provides the canonical OS,
architecture, endianness and word-size features; the port enables implementation
selection and supplies an explicit ASDF component. It does not advertise the
old implementation name in TorCL's `*features*`.

Apply from the dependency's source directory:

```sh
patch -p1 < /path/to/torcl/ports/trivial-features-torcl/implementation.patch
```

Restart TorCL and load Babel with that directory in ASDF's source registry.
If using ocicl, preserve its initialization when migrating by copying your old
init file to `~/.torclrc`; TorCL does not read `~/.blissrc` automatically.

The unpatched release accepts `bliss` but rejects `torcl`. In the tested TorCL
installation, reaching this dependency error through ASDF ran until the memory
cap or timeout; the patch makes Babel compile and load successfully. It does
not fix the separate ASDF error-handling runaway.

This is a local compatibility patch, not an upstream release. An ocicl update
can replace it. Existing locally modified copies may require manual merging.
