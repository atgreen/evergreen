;;;; build-image.lisp — produce the installable `torcl` executable.
;;;;
;;;; Run by the Makefile `install` target as:
;;;;   TORCL_IMAGE_OUT=<path> torcl --no-init --load scripts/build-image.lisp
;;;;
;;;; Loads ASDF into the running image, then dumps a standalone executable (the
;;;; runtime binary with the world appended, per SAVE-LISP-AND-DIE :executable t)
;;;; so the installed `torcl` starts with ASDF already available — no (require
;;;; :asdf) and no source files needed at runtime. The output path comes from
;;;; TORCL_IMAGE_OUT (default "torcl" in the current directory).

(require :asdf)

(let ((out (or (uiop:getenv "TORCL_IMAGE_OUT") "torcl")))
  (format t "~&Dumping torcl executable (ASDF ~a) to ~a~%" (asdf:asdf-version) out)
  (save-lisp-and-die out :executable t))
