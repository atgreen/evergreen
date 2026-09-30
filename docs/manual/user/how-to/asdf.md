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
