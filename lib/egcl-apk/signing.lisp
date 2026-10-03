;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(in-package :egcl-apk)

(defun big-endian-integer (n)
  (let ((out nil))
    (loop do (push (logand n 255) out) (setf n (ash n -8)) until (zerop n))
    (apply #'octets out)))
(defun der (tag payload)
  (let ((n (length payload)))
    (bytes (octets tag)
           (if (< n 128) (octets n)
               (let ((size (big-endian-integer n))) (bytes (octets (+ 128 (length size))) size)))
           payload)))
(defun der-sequence (&rest parts) (der #x30 (apply #'bytes parts)))
(defun der-integer (n)
  (unless (and (integerp n) (>= n 0)) (error "Expected nonnegative DER integer"))
  (let ((v (big-endian-integer n)))
    (der 2 (if (logbitp 7 (aref v 0)) (bytes #(0) v) v))))
(defun der-bits (data) (der 3 (bytes #(0) data)))
(defun ec-algorithm ()
  ;; id-ecPublicKey, prime256v1
  (der-sequence (der 6 #(42 134 72 206 61 2 1)) (der 6 #(42 134 72 206 61 3 1 7))))
(defun signature-algorithm () (der-sequence (der 6 #(42 134 72 206 61 4 3 2))))
(defun public-key-info (public) (der-sequence (ec-algorithm) (der-bits public)))
(defun ecdsa-sign (private data)
  (let ((raw (ironclad:sign-message private (sha256 data))))
    (unless (= (length raw) 64) (error "Unexpected P-256 signature length"))
    (der-sequence (der-integer (ironclad:octets-to-integer raw :start 0 :end 32 :big-endian t))
                  (der-integer (ironclad:octets-to-integer raw :start 32 :end 64 :big-endian t)))))
(defun certificate-time (time)
  (multiple-value-bind (s m h day month year) (decode-universal-time time 0)
    (if (<= 1950 year 2049)
        (der #x17 (utf8 (format nil "~2,'0D~2,'0D~2,'0D~2,'0D~2,'0D~2,'0DZ" (mod year 100) month day h m s)))
        (der #x18 (utf8 (format nil "~4,'0D~2,'0D~2,'0D~2,'0D~2,'0D~2,'0DZ" year month day h m s))))))
(defun certificate (private public)
  (let* ((name (der-sequence (der #x31 (der-sequence (der 6 #(85 4 3)) (der #x0c (utf8 "EGCL APK"))))))
         (now (get-universal-time))
         (tbs (der-sequence
               (der #xa0 (der-integer 2))
               (der-integer (1+ (ironclad:octets-to-integer (ironclad:random-data 16) :big-endian t)))
               (signature-algorithm) name
               (der-sequence (certificate-time (- now 86400)) (certificate-time (+ now (* 3650 86400))))
               name (public-key-info public))))
    (der-sequence tbs (signature-algorithm) (der-bits (ecdsa-sign private tbs)))))

(defstruct signing-identity private public certificate)
(defun restrict-to-owner (path)
  "Make PATH readable and writable by its owner only.

This is why creating an identity needs no particular umask from its caller: the
file holds an unencrypted P-256 private key, and leaving its mode to whatever
the caller happened to set is how such a key ends up world-readable. EGCL
reaches chmod through EGCL-POSIX and SBCL through SB-POSIX."
  #+egcl (progn (require :egcl-posix)
                (funcall (read-from-string "egcl-posix:chmod") (namestring path) #o600))
  #+sbcl (progn (require :sb-posix)
                (funcall (read-from-string "sb-posix:chmod") (namestring path) #o600))
  #-(or egcl sbcl)
  (warn "Cannot restrict ~A to its owner on this implementation; ~
         check its permissions by hand." path))

(defun create-identity (path)
  "Create a new persistent development signing identity; never overwrite one.

The key is chmod 0600 as soon as it exists, so no caller has to arrange a
umask first."
  (when (probe-file path) (error "Signing identity already exists: ~A" path))
  (multiple-value-bind (private public-key) (ironclad:generate-key-pair :secp256r1)
    (let* ((public (getf (ironclad:destructure-public-key public-key) :y))
           (secret (getf (ironclad:destructure-private-key private) :x))
           (cert (certificate private public)))
      (write-bytes path (bytes (utf8 "EGCLKEY1") secret public (length-prefix cert)) :if-exists :error)
      (restrict-to-owner path)
      (make-signing-identity :private private :public public :certificate cert))))
(defun load-identity (path)
  (let ((data (read-bytes path)))
    (unless (and (>= (length data) 109) (equalp (subseq data 0 8) (utf8 "EGCLKEY1"))
                 (= (+ 109 (read-le data 105 4)) (length data)))
      (error "Invalid EGCL signing identity: ~A" path))
    (let* ((secret (subseq data 8 40)) (public (subseq data 40 105))
           (n (ironclad:octets-to-integer secret :big-endian t)))
      (unless (< 0 n #xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551)
        (error "Invalid P-256 private scalar"))
      (let ((private (ironclad:make-private-key :secp256r1 :x secret)))
        (unless (equalp public (getf (ironclad:destructure-private-key private) :y))
          (error "Signing key/public key mismatch"))
        (make-signing-identity :private private :public public :certificate (subseq data 109))))))

(defun apk-digest (sections)
  ;; Sections are chunked separately; the final EOCD uses the unsigned central
  ;; directory offset (the signing block's eventual start), per APK v2.
  (let ((digests nil))
    (dolist (section sections)
      (loop for start from 0 below (length section) by 1048576 do
        (let ((end (min (length section) (+ start 1048576))))
          (push (sha256 (bytes #(#xa5) (u32 (- end start)) (subseq section start end))) digests))))
    (sha256 (bytes #(#x5a) (u32 (length digests)) (apply #'bytes (nreverse digests))))))
(defun signing-block (identity sections)
  (let* ((digest-record (length-prefix (bytes (u32 #x0201) (length-prefix (apk-digest sections)))))
         (signed-data (bytes (length-prefix digest-record)
                             (length-prefix (length-prefix (signing-identity-certificate identity)))
                             (u32 0)))
         (signature-record (length-prefix
                            (bytes (u32 #x0201) (length-prefix (ecdsa-sign (signing-identity-private identity) signed-data)))))
         (signer (bytes (length-prefix signed-data) (length-prefix signature-record)
                        (length-prefix (public-key-info (signing-identity-public identity)))))
         (value (length-prefix (length-prefix signer)))
         (pair (bytes (u64 (+ 4 (length value))) (u32 #x7109871a) value))
         (size (+ (length pair) 24)))
    (bytes (u64 size) pair (u64 size) (utf8 "APK Sig Block 42"))))
