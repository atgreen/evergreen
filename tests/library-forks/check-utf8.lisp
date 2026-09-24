;;;; Known TorCL regression bliss-bsjw. SBCL accepts this byte sequence.
;;;; Run separately after check.lisp, preserving the same environment/cache.
(load (merge-pathnames "check.lisp" *load-truename*))
(with-open-file (binary "sample-utf8.txt" :element-type '(unsigned-byte 8))
  (let ((stream (flexi-streams:make-flexi-stream
                 binary :external-format '(:utf-8 :eol-style :lf))))
    (assert (equal '(65 233 8364)
                   (map 'list #'char-code (read-line stream))))
    (assert (eq :eof (read-char stream nil :eof)))))
(format t "MULTIBYTE-UTF8-OK~%")
