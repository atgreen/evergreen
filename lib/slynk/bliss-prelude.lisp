;;;; bliss-prelude.lisp --- shims for CL functions slynk needs that bliss lacks.
;;;; Loaded before slynk. Each shim interns+exports the symbol in COMMON-LISP so
;;;; slynk's (:use cl) inherits it, then defines a minimal implementation.

(in-package :cl-user)

(defun %bliss-ensure-cl (name fn)
  "Ensure COMMON-LISP has an external function NAME (a string), defining FN
   only if unbound. Returns the symbol."
  (let ((sym (intern name :common-lisp)))
    (export sym :common-lisp)
    (unless (fboundp sym)
      (setf (symbol-function sym) fn))
    sym))

;;; ---- Pretty-printer dispatch (stubbed: no custom dispatch) --------------
(%bliss-ensure-cl "COPY-PPRINT-DISPATCH"
                  (lambda (&optional table) (declare (ignore table)) nil))
(%bliss-ensure-cl "SET-PPRINT-DISPATCH"
                  (lambda (type fn &optional priority table)
                    (declare (ignore type fn priority table)) nil))
(%bliss-ensure-cl "PPRINT-DISPATCH"
                  (lambda (object &optional table)
                    (declare (ignore object table)) (values nil nil)))

;;; ---- Readtable stubs -----------------------------------------------------
;;; bliss represents the readtable as the keyword :STANDARD-READTABLE. Provide
;;; the accessors slynk uses; the reader is fixed (upcase), so these are constant.
(%bliss-ensure-cl "READTABLEP"
                  (lambda (x) (typep x 'readtable)))
(%bliss-ensure-cl "READTABLE-CASE"
                  (lambda (rt) (declare (ignore rt)) :upcase))
(%bliss-ensure-cl "COPY-READTABLE"
                  (lambda (&optional from to) (declare (ignore to))
                    (or from *readtable*)))

;;; ---- MAKE-TWO-WAY-STREAM (bliss has the impl; expose the CL name) ---------
;;; (defined natively; the shim only covers the case it is missing)

;;; ---- SLYNK-GRAY stub -----------------------------------------------------
;;; bliss implements streams natively (not via a Gray-streams CLOS protocol),
;;; so slynk-gray.lisp cannot load. Our MAKE-OUTPUT-STREAM/MAKE-INPUT-STREAM
;;; return the process's real std streams (icl pipes the subprocess stdout), so
;;; the Gray IO-redirection classes are unnecessary. slynk.lisp only calls
;;; slynk-gray::reset-stream-line-column — stub it.
(unless (find-package :slynk-gray)
  (make-package :slynk-gray :use '(:cl)))
(let ((sym (intern "RESET-STREAM-LINE-COLUMN" :slynk-gray)))
  (unless (fboundp sym)
    (setf (symbol-function sym) (lambda (stream) (declare (ignore stream)) nil))))

(format t "; bliss-prelude loaded~%")
