;;;; Multibyte UTF-8 through both decoders. Run after check.lisp, preserving the
;;;; same environment/cache; loading check.lisp here supplies Babel and Flexi.
;;;;
;;;; Both halves were regressions with one root cause (bliss-rnd7, bliss-bsjw):
;;;; an arithmetic LOOP variable was stepped from a private counter, so babel's
;;;; decoder -- `for i fixnum from start below end` with an `(incf i)` per
;;;; continuation byte consumed -- revisited each continuation byte and rejected
;;;; it as a starter byte. SBCL passes this file unchanged.
(load (merge-pathnames "check.lisp" *load-truename*))

;;; Babel: round-trip a 2-byte and a 3-byte character through its own encoder.
(let* ((text (coerce (list #\h (code-char 233) #\l (code-char 8364)) 'string))
       (octets (babel:string-to-octets text :encoding :utf-8)))
  (assert (equalp #(104 195 169 108 226 130 172) octets))
  (assert (string= text (babel:octets-to-string octets :encoding :utf-8))))

;;; Flexi Streams: decode the same shapes off a binary stream.
(with-open-file (binary "sample-utf8.txt" :element-type '(unsigned-byte 8))
  (let ((stream (flexi-streams:make-flexi-stream
                 binary :external-format '(:utf-8 :eol-style :lf))))
    (assert (equal '(65 233 8364)
                   (map 'list #'char-code (read-line stream))))
    (assert (eq :eof (read-char stream nil :eof)))))

(format t "MULTIBYTE-UTF8-OK~%")
