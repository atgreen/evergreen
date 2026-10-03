;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(defpackage :egcl-apk
  (:use :cl)
  (:export :manifest :write-apk :build-apk-from :load-identity :create-identity))
(in-package :egcl-apk)

(defun bytes (&rest parts)
  (apply #'concatenate '(vector (unsigned-byte 8)) parts))
(defun octets (&rest values) (apply #'bytes (list values)))
(defun little-endian (value width)
  (unless (<= 0 value (1- (ash 1 (* width 8)))) (error "Integer exceeds ~D bytes" width))
  (let ((out (make-array width :element-type '(unsigned-byte 8))))
    (dotimes (i width out) (setf (aref out i) (ldb (byte 8 (* i 8)) value)))))
(defun u16 (n) (little-endian n 2))
(defun u32 (n) (little-endian n 4))
(defun u64 (n) (little-endian n 8))
(defun read-le (data offset width)
  (let ((n 0)) (dotimes (i width n) (setf n (logior n (ash (aref data (+ offset i)) (* i 8)))))))
(defun length-prefix (data) (bytes (u32 (length data)) data))
(defun utf8 (text)
  (let ((out (make-array 0 :element-type '(unsigned-byte 8) :adjustable t :fill-pointer 0)))
    (loop for c across text for n = (char-code c) do
      (cond ((< n #x80) (vector-push-extend n out))
            ((< n #x800)
             (vector-push-extend (logior #xc0 (ash n -6)) out)
             (vector-push-extend (logior #x80 (logand n 63)) out))
            ((or (<= #xd800 n #xdfff) (> n #x10ffff)) (error "Invalid Unicode scalar"))
            ((< n #x10000)
             (vector-push-extend (logior #xe0 (ash n -12)) out)
             (vector-push-extend (logior #x80 (logand (ash n -6) 63)) out)
             (vector-push-extend (logior #x80 (logand n 63)) out))
            (t (vector-push-extend (logior #xf0 (ash n -18)) out)
               (vector-push-extend (logior #x80 (logand (ash n -12) 63)) out)
               (vector-push-extend (logior #x80 (logand (ash n -6) 63)) out)
               (vector-push-extend (logior #x80 (logand n 63)) out))))
    (copy-seq out)))
(defun read-bytes (path)
  (with-open-file (s path :element-type '(unsigned-byte 8))
    (let ((data (make-array (file-length s) :element-type '(unsigned-byte 8))))
      (unless (= (read-sequence data s) (length data)) (error "Short read: ~A" path)) data)))
(defun write-bytes (path data &key (if-exists :supersede))
  (ensure-directories-exist path)
  (with-open-file (s path :direction :output :element-type '(unsigned-byte 8)
                          :if-exists if-exists :if-does-not-exist :create)
    (write-sequence data s)) path)
(defparameter *crc-table*
  (let ((table (make-array 256)))
    (dotimes (i 256 table)
      (let ((c i))
        (dotimes (j 8) (setf c (logxor (ash c -1) (if (oddp c) #xedb88320 0))))
        (setf (aref table i) c)))))
(defun crc32 (data)
  (let ((crc #xffffffff))
    (loop for b across data do
      (setf crc (logxor (ash crc -8) (aref *crc-table* (logand (logxor crc b) 255)))))
    (logxor crc #xffffffff)))
;; SHA-256 is the whole cost of APK v2 signing, which hashes the entire
;; archive in 1 MiB chunks. Ironclad's portable implementation measures about
;; 0.012 MiB/s under EGCL -- roughly 22 minutes for a 6.8 MB APK against SBCL's
;; 1.2 seconds -- so use the runtime's primitive there. Ironclad itself drops to
;; implementation-specific routines on every host that has them; this is the
;; same arrangement. Both produce the same digest, checked against the NIST
;; vectors on each side.
(defun sha256 (data)
  #+egcl (egcl-ext:sha256 data)
  #-egcl (ironclad:digest-sequence :sha256 data))
