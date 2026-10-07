# Load an ASDF system

Use ASDF to load a local Lisp project. These steps assume EGCL is installed
and your shell is in a new project directory.

Create `hello.asd`:

```lisp
(asdf:defsystem "hello"
  :serial t
  :components ((:file "hello")))
```

Create `hello.lisp`:

```lisp
(defpackage :hello
  (:use :cl)
  (:export :greet))
(in-package :hello)

(defun greet (name)
  (format t "Hello, ~A!~%" name))
```

Create `run.lisp`:

```lisp
(require :asdf)
(asdf:load-asd (truename "hello.asd"))
(asdf:load-system "hello")
(hello:greet "Lisp")
```

Run it:

```sh
egcl --no-init --load run.lisp
```

The greeting is `Hello, Lisp!`. Loading an explicit `.asd` pathname avoids
having to configure ASDF's source registry for this example.

For dependencies, install their sources and register them with ASDF in the
usual way. EGCL does not guarantee that every existing Common Lisp library
works unchanged. When a library rejects the implementation or uses an
unsupported extension, identify that interface before changing its feature
conditionals. Contributor guidance for maintained compatibility forks is in
[the repository map](../../contributing/reference/repository.md).

## Quicklisp

On x86-64 Linux, use the EGCL client fork. This requires an EGCL build containing
the runtime fixes through [PR #90](https://github.com/atgreen/evergreen/pull/90);
the v0.0.3 release does not contain them.

Fetch the tested client into a **new project directory** using an ocicl version
with Git-source support:

```sh
mkdir egcl-quicklisp
cd egcl-quicklisp
touch ocicl.csv
ocicl install git+https://github.com/atgreen/quicklisp-client@2115f1d96963bf48d6a4c5c7111189bd326bfbed
mkdir quicklisp
cp ocicl/quicklisp-client-2115f1d/setup.lisp ocicl/quicklisp-client-2115f1d/asdf.lisp quicklisp/
cp -R ocicl/quicklisp-client-2115f1d/quicklisp quicklisp/
```

Keep `ocicl.csv` for reproducibility. The separate `quicklisp/` directory is the
installation: caches, distribution metadata, and downloaded libraries belong
there, not in the fetched client source tree. This does not change your existing
Quicklisp installation or Lisp initialization files.

Create `run.lisp` in that project directory:

```lisp
(load "quicklisp/setup.lisp")
(ql:quickload "alexandria" :prompt nil)
(assert (equal (alexandria:iota 5 :start 3) '(3 4 5 6 7)))
```

Then run:

```sh
egcl --no-init --load run.lisp
```

The first setup loads ASDF and installs Quicklisp's distribution metadata;
`QUICKLOAD` downloads and compiles missing releases. Later processes load the
same `setup.lisp` and reuse the installation. The project's `install-egcl-forks`
command also fetches the client (`egcl` branch), but only downloads/registers
its sources: copy them into a separate installation as above before setup.

The client uses native EGCL TCP streams; the tested distribution transport is
plain HTTP, **not TLS**. Do not use `QL:UPDATE-CLIENT` to replace this client with
the upstream version, which lacks the EGCL adapter. Library distributions are
separate from the client: a library needing implementation-specific support may
still need an EGCL compatibility fork. Successful Quicklisp installation is not
a claim that every distributed library runs unchanged.
