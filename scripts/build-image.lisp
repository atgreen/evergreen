;;;; build-image.lisp — produce the installable `bliss` executable.
;;;;
;;;; Run by the Makefile `install` target as:
;;;;   BLISS_IMAGE_OUT=<path> bliss-cli --no-init --load scripts/build-image.lisp
;;;;
;;;; Loads ASDF into the running image, then dumps a standalone executable (the
;;;; runtime binary with the world appended, per SAVE-LISP-AND-DIE :executable t)
;;;; so the installed `bliss` starts with ASDF already available — no (require
;;;; :asdf) and no source files needed at runtime. The output path comes from
;;;; BLISS_IMAGE_OUT (default "bliss" in the current directory).

(require :asdf)

(let ((out (or (uiop:getenv "BLISS_IMAGE_OUT") "bliss")))
  (format t "~&Dumping bliss executable (ASDF ~a) to ~a~%" (asdf:asdf-version) out)
  (save-lisp-and-die out :executable t))
