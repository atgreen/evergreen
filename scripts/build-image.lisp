;;;; build-image.lisp — produce the installable `egcl` executable.
;;;;
;;;; Run by the Makefile `install` target as:
;;;;   EGCL_IMAGE_OUT=<path> egcl --no-init --load scripts/build-image.lisp
;;;;
;;;; Loads ASDF into the running image, then dumps a standalone executable (the
;;;; runtime binary with the world appended, per SAVE-LISP-AND-DIE :executable t)
;;;; so the installed `egcl` starts with ASDF already available — no (require
;;;; :asdf) and no source files needed at runtime. The output path comes from
;;;; EGCL_IMAGE_OUT (default "egcl" in the current directory).

(require :asdf)

(let ((out (or (uiop:getenv "EGCL_IMAGE_OUT") "egcl")))
  (format t "~&Dumping egcl executable (ASDF ~a) to ~a~%" (asdf:asdf-version) out)
  (save-lisp-and-die out :executable t))
