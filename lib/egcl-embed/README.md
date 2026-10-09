# egcl-embed-asdf

Carry build-host files in your application's image, as components of its
`.asd`. A library such as local-time reads `/usr/share/zoneinfo/...` through
Lisp streams; an image cross-built or copied to another host lands where those
files do not exist. Embedding reads the files on the build host, when the
system loads, and records them under the paths the application will open at
run time, so a saved image, a shaken executable (`egcl-shake-asdf`) or a
future APK finds them anywhere.

```lisp
(asdf:defsystem "my-app"
  :defsystem-depends-on ("egcl-embed-asdf")
  :depends-on ("local-time")
  :components
  ((:embedded-tree "zoneinfo"
     :path "/usr/share/zoneinfo/"
     :only ("UTC" "America/*" "Europe/*"))
   (:embedded-file "config"
     :source "deploy/config.sexp"
     :path "/etc/my-app/config.sexp")
   (:file "my-app")))
```

| Component | Carries | Options |
| --- | --- | --- |
| `(:embedded-file NAME :path P)` | the one file `P` | `:source S` reads `S` instead (relative to the system) |
| `(:embedded-tree NAME :path P)` | every file under directory `P`, at the same relative paths | `:source S`; `:only (PATTERNS…)` keeps only files matching a wild relative pathname such as `"America/*"` or `"**/*.lisp"` |

`:source` defaults to `:path`, since "the same place on the build host" is the
usual case. A missing source is an error when the system loads, which is the
cheapest time to learn. The string forms `("egcl-embed-asdf:embedded-file" …)`
work as well as the keywords.

What the application sees: `open` for input, `probe-file`, `truename`,
`file-length`, `file-write-date`, `directory` and `load` find embedded files,
and an embedded file shadows a real one at the same path, so behaviour does
not depend on the host. They are read-only: opening one for output, renaming
or deleting it is a `file-error`. **Only the Lisp image sees them**: the
operating system, foreign code called through the FFI and child processes do
not, so a C library that `fopen`s the path still needs the real file.

Underneath is `egcl-ext:embed-file`, which can be called directly:

```lisp
(egcl-ext:embed-file "/usr/share/zoneinfo/UTC")              ; same path on both hosts
(egcl-ext:embed-file "/etc/my-app/config.sexp" "deploy/config.sexp")
(egcl-ext:embedded-file-p "/usr/share/zoneinfo/UTC")        ; => T
(egcl-ext:embedded-files)                                    ; => the paths
```

Contents are stored one byte per octet, so a megabyte of files is a megabyte
of image. `crates/egcl/tests/embed_asdf_cli.rs` drives this from a fixture
project; `crates/egcl/tests/embedded_files_cli.rs` covers the runtime half.
Installing `egcl` puts this extension in
`/usr/share/common-lisp/source/egcl-embed/`, which ASDF's default source
registry already searches.
