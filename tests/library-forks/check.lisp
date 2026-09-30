;;;; Real clients of the pinned EGCL compatibility forks. No implicit downloads.
(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE"))
       :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*download* nil
      ocicl-runtime:*local-only* t)

(asdf:load-system :babel)
(assert (equalp #(72 101 108 108 111)
                (babel:string-to-octets "Hello" :encoding :utf-8)))
(dolist (alternatives '((:little-endian :big-endian) (:32-bit :64-bit)))
  (assert (= 1 (count-if (lambda (feature) (member feature *features*))
                         alternatives))))

(asdf:load-system :flexi-streams)
#+(or egcl sbcl)
(dolist (name '("STREAM-READ-CHAR" "STREAM-READ-BYTE" "STREAM-WRITE-CHAR"
                "STREAM-UNREAD-CHAR" "STREAM-FINISH-OUTPUT"))
  (assert (eq (find-symbol name :trivial-gray-streams)
              (find-symbol name #+egcl :egcl-gray-streams #+sbcl :sb-gray))))
(with-open-file (binary "sample-ascii.txt" :element-type '(unsigned-byte 8))
  (let ((stream (flexi-streams:make-flexi-stream
                 binary :external-format '(:utf-8 :eol-style :lf))))
    (assert (string= "Hello" (read-line stream)))
    (assert (eq :eof (read-char stream nil :eof)))))
(format t "LIBRARY-FORKS-OK~%")
