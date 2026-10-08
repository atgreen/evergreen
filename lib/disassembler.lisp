;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;;; disassembler.lisp — in-process Power ISA (ppc64le) decoder for DISASSEMBLE.
;;;;
;;;; The runtime hands this file a function's installed native code as a list of
;;;; 32-bit instruction words through (egcl::%native-code 'name); DECODE turns one
;;;; word into the mnemonic the assembler's own encoding tests use as their
;;;; oracle ("add 3, 4, 5", "cmpdi 0, 3, 7", "sldi 3, 4, 3", "mflr 0", ...), and
;;;; NATIVE-LISTING renders the whole function with branch targets, OSR entries,
;;;; runtime-helper names and tagged-constant notes, in the same shape as the
;;;; x86-64 listing the Rust decoder produces. Nothing here shells out: every
;;;; word is decoded in this process. An opcode the JIT does not emit prints as
;;;; .long, never as a guess.

(defpackage :egcl-disasm
  (:use :common-lisp)
  (:export #:decode #:native-listing))

(in-package :egcl-disasm)

;;; ---------------------------------------------------------------------------
;;; Fields
;;; ---------------------------------------------------------------------------

;; Power documents bit 0 as the most significant bit; every field below is given
;; as (size position) counted from the least significant bit instead, which is
;; what BYTE wants and what the assembler's encoders use.
(defun field (w size pos) (ldb (byte size pos) w))
(defun signed (v bits) (if (logbitp (1- bits) v) (- v (ash 1 bits)) v))
(defun simm16 (w) (signed (field w 16 0) 16))
(defun uimm16 (w) (field w 16 0))
(defun hex (n) (format nil "0x~(~x~)" n))

(defun ins (name &rest operands)
  (format nil "~a~@[ ~{~a~^, ~}~]" name operands))

(defun disp (offset base) (format nil "~d(~d)" offset base))
(defun target (offset) (format nil "+~(~4,'0x~)" offset))

;;; ---------------------------------------------------------------------------
;;; Decoding one word
;;; ---------------------------------------------------------------------------

(defun condition-name (bi sense)
  "The extended mnemonic suffix for CR bit BI tested with SENSE (T = branch if set)."
  (ecase (logand bi 3)
    (0 (if sense "lt" "ge"))
    (1 (if sense "gt" "le"))
    (2 (if sense "eq" "ne"))
    (3 (if sense "so" "ns"))))

(defun decode-bc (w offset)
  (let* ((bo (field w 5 21))
         (bi (field w 5 16))
         (bd (signed (logand w #xFFFC) 16))
         (to (+ offset bd))
         (link (if (logbitp 0 w) "l" ""))
         (cr (ash bi -2)))
    (values
     (cond ((member bo '(12 4))
            (let ((name (format nil "b~a~a" (condition-name bi (= bo 12)) link)))
              (if (zerop cr) (ins name (target to)) (ins name (format nil "cr~d" cr) (target to)))))
           (t (ins (format nil "bc~a" link) bo bi (target to))))
     to)))

(defun decode-xl (w)
  (let ((xo (field w 10 1)) (bo (field w 5 21)) (bi (field w 5 16)) (link (logbitp 0 w)))
    (case xo
      (16 (cond ((= bo 20) (if link "blrl" "blr"))
                (t (ins (if link "bclrl" "bclr") bo bi))))
      (528 (cond ((= bo 20) (if link "bctrl" "bctr"))
                 (t (ins (if link "bcctrl" "bcctr") bo bi))))
      (t (format nil ".long ~a" (hex w))))))

(defun decode-rld (w)
  (let* ((s (field w 5 21)) (a (field w 5 16))
         (sh (logior (field w 5 11) (ash (field w 1 1) 5)))
         (m (logior (field w 5 6) (ash (field w 1 5) 5)))
         (dot (if (logbitp 0 w) "." "")))
    (case (field w 3 2)
      (0 (cond ((zerop sh) (ins (format nil "clrldi~a" dot) a s m))
               ((= m (- 64 sh)) (ins (format nil "srdi~a" dot) a s m))
               (t (ins (format nil "rldicl~a" dot) a s sh m))))
      (1 (cond ((= m (- 63 sh)) (ins (format nil "sldi~a" dot) a s sh))
               (t (ins (format nil "rldicr~a" dot) a s sh m))))
      (2 (ins (format nil "rldic~a" dot) a s sh m))
      (3 (ins (format nil "rldimi~a" dot) a s sh m))
      (t (format nil ".long ~a" (hex w))))))

(defun spr-name (w)
  (case (logior (field w 5 16) (ash (field w 5 11) 5))
    (1 "xer") (8 "lr") (9 "ctr") (t nil)))

(defun decode-x (w)
  (let* ((d (field w 5 21)) (a (field w 5 16)) (b (field w 5 11))
         (xo (field w 10 1))
         (dot (if (logbitp 0 w) "." "")))
    (cond
      ((= (field w 5 1) 15) (ins "isel" d a b (field w 5 6)))
      (t
       (case xo
         (266 (ins (format nil "add~a" dot) d a b))
         (40 (ins (format nil "subf~a" dot) d a b))
         (104 (ins (format nil "neg~a" dot) d a))
         (233 (ins (format nil "mulld~a" dot) d a b))
         (73 (ins (format nil "mulhd~a" dot) d a b))
         (28 (ins (format nil "and~a" dot) a d b))
         (444 (if (= d b) (ins (format nil "mr~a" dot) a d) (ins (format nil "or~a" dot) a d b)))
         (316 (ins (format nil "xor~a" dot) a d b))
         (476 (ins (format nil "nand~a" dot) a d b))
         (986 (ins (format nil "extsw~a" dot) a d))
         ((826 827)
          (ins (format nil "sradi~a" dot) a d (logior (field w 5 11) (ash (field w 1 1) 5))))
         (0 (ins (if (logbitp 21 w) "cmpd" "cmpw") (field w 3 23) a b))
         (32 (ins (if (logbitp 21 w) "cmpld" "cmplw") (field w 3 23) a b))
         (179 (ins "mtvsrd" d a))
         (51 (ins "mfvsrd" a d))
         (339 (let ((spr (spr-name w))) (if spr (ins (format nil "mf~a" spr) d) (ins "mfspr" d (hex (logior (field w 5 16) (ash (field w 5 11) 5)))))))
         (467 (let ((spr (spr-name w))) (if spr (ins (format nil "mt~a" spr) d) (ins "mtspr" (hex (logior (field w 5 16) (ash (field w 5 11) 5))) d))))
         (t (format nil ".long ~a" (hex w))))))))

(defun decode-fp (w single)
  (let* ((d (field w 5 21)) (a (field w 5 16)) (b (field w 5 11)) (c (field w 5 6))
         (sfx (if single "s" ""))
         (dot (if (logbitp 0 w) "." "")))
    (case (field w 5 1)
      (21 (ins (format nil "fadd~a~a" sfx dot) d a b))
      (20 (ins (format nil "fsub~a~a" sfx dot) d a b))
      (25 (ins (format nil "fmul~a~a" sfx dot) d a c))
      (18 (ins (format nil "fdiv~a~a" sfx dot) d a b))
      (t (if single
             (format nil ".long ~a" (hex w))
             (case (field w 10 1)
               (0 (ins "fcmpu" (field w 3 23) a b))
               (72 (ins (format nil "fmr~a" dot) d b))
               (40 (ins (format nil "fneg~a" dot) d b))
               (264 (ins (format nil "fabs~a" dot) d b))
               (t (format nil ".long ~a" (hex w)))))))))

(defun decode-vsx (w)
  (let ((xt (logior (field w 5 21) (ash (field w 1 0) 5)))
        (xb (logior (field w 5 11) (ash (field w 1 1) 5))))
    (case (field w 9 2)
      (267 (ins "xscvdpspn" xt xb))
      (331 (ins "xscvspdpn" xt xb))
      (t (format nil ".long ~a" (hex w))))))

(defun decode (w &optional (offset 0))
  "Decode the instruction word W found at byte OFFSET. Returns the text and, for
a relative branch, the byte offset it targets."
  (let ((op (field w 6 26)) (d (field w 5 21)) (a (field w 5 16)))
    (case op
      (14 (if (zerop a) (ins "li" d (simm16 w)) (ins "addi" d a (simm16 w))))
      (15 (if (zerop a) (ins "lis" d (hex (uimm16 w))) (ins "addis" d a (simm16 w))))
      (24 (if (= w #x60000000) "nop" (ins "ori" a d (hex (uimm16 w)))))
      (25 (ins "oris" a d (hex (uimm16 w))))
      (28 (ins "andi." a d (hex (uimm16 w))))
      (11 (ins (if (logbitp 21 w) "cmpdi" "cmpwi") (field w 3 23) a (simm16 w)))
      (10 (ins (if (logbitp 21 w) "cmpldi" "cmplwi") (field w 3 23) a (hex (uimm16 w))))
      (16 (decode-bc w offset))
      (18 (let ((to (+ offset (signed (logand w #x03FFFFFC) 26))))
            (values (ins (if (logbitp 0 w) "bl" "b") (target to)) to)))
      (19 (decode-xl w))
      (30 (decode-rld w))
      (31 (decode-x w))
      (32 (ins "lwz" d (disp (simm16 w) a)))
      (36 (ins "stw" d (disp (simm16 w) a)))
      (50 (ins "lfd" d (disp (simm16 w) a)))
      (54 (ins "stfd" d (disp (simm16 w) a)))
      (58 (ins (case (field w 2 0) (0 "ld") (1 "ldu") (t "lwa")) d (disp (signed (logand w #xFFFC) 16) a)))
      (62 (ins (if (logbitp 0 w) "stdu" "std") d (disp (signed (logand w #xFFFC) 16) a)))
      (59 (decode-fp w t))
      (63 (decode-fp w nil))
      (60 (decode-vsx w))
      (t (format nil ".long ~a" (hex w))))))

;;; ---------------------------------------------------------------------------
;;; Listing a function
;;; ---------------------------------------------------------------------------

(defun written-register (text)
  "The GPR named by the first operand of TEXT, when there is one: the register
that a non-immediate instruction overwrites, which ends any constant tracked in it."
  (let* ((space (position #\Space text))
         (end (and space (position #\, text :start space))))
    (when space
      (let ((token (string-trim " " (subseq text (1+ space) end))))
        (and (plusp (length token)) (every #'digit-char-p token) (parse-integer token))))))

(defun tagged-note (value nil-bits)
  "A readable Common Lisp value for an immediate that is clearly one. NIL's bits
are also the tag mask, so a bare `li r, 7` is not named; the comparison or tag
check that consumes it says which it was."
  (cond ((= value nil-bits) nil)
        ((and (/= value 0) (zerop (logand value 7)) (< value (ash 1 24)))
         (format nil "fixnum ~d" (ash value -3)))
        (t nil)))

(defun native-listing (name)
  "The annotated native listing of NAME's installed native code, or NIL when it
has none. DISASSEMBLE prints this after its header on ppc64le."
  (let ((info (egcl::%native-code name)))
    (when info
      (destructuring-bind (is-t2 entry words osr bcps helpers nil-bits) info
        (declare (ignore is-t2 entry bcps))
        (let ((regs (make-array 32 :initial-element nil))
              (osr-offsets (mapcar #'cdr osr))
              (pending-call nil)
              (offset 0))
          (with-output-to-string (out)
            (dolist (w words)
              (multiple-value-bind (text to) (decode w offset)
                (let* ((op (field w 6 26))
                       (d (field w 5 21))
                       (a (field w 5 16))
                       (b (field w 5 11))
                       (note nil))
                  ;; Follow the halfword-at-a-time construction of 64-bit constants
                  ;; so the final value can be named.
                  (flet ((set-reg (r v) (setf (aref regs r) v) v))
                    (case op
                      (14 (when (zerop a) (setf note (tagged-note (set-reg d (simm16 w)) nil-bits))))
                      (11 (when (= (simm16 w) nil-bits) (setf note "NIL test")))
                      (15 (when (zerop a) (set-reg d (ash (simm16 w) 16))))
                      (24 (when (and (= a d) (aref regs d))
                            (setf note (let ((v (set-reg a (logior (aref regs d) (uimm16 w)))))
                                         (or (cdr (assoc v helpers)) (tagged-note v nil-bits))))))
                      (25 (when (and (= a d) (aref regs d))
                            (set-reg a (logior (aref regs d) (ash (uimm16 w) 16)))))
                      (30 (if (and (= a d) (aref regs d) (= (field w 3 2) 1))
                              (set-reg a (ash (aref regs d) (logior (field w 5 11) (ash (field w 1 1) 5))))
                              (let ((r (written-register text))) (when r (setf (aref regs r) nil)))))
                      (31 (let ((xo (field w 10 1)))
                            (cond ((and (= xo 467) (= (logior (field w 5 16) (ash (field w 5 11) 5)) 9))
                                   ;; mtctr: the call target is whatever was built in rS.
                                   (let ((v (aref regs d)))
                                     (setf pending-call (and v (cdr (assoc v helpers))))
                                     (when pending-call (setf note (format nil "→ ~a" pending-call)))))
                                  ((and (= xo 28) (or (eql (aref regs b) 7) (eql (aref regs d) 7)))
                                   (setf note "tag check (low 3 bits select the type)")
                                   (setf (aref regs a) nil))
                                  (t (let ((r (written-register text))) (when r (setf (aref regs r) nil)))))))
                      (19 (cond ((string= text "bctrl")
                                 (setf note (if pending-call (format nil "call ~a" pending-call) "call via CTR"))
                                 (setf pending-call nil)
                                 (fill regs nil))
                                ((string= text "bctr") (setf note "jump via CTR"))
                                ((string= text "blr") (setf note "return"))))
                      ((16 18)
                       (when to
                         (setf note (format nil "→ ~a~a" (target to)
                                            (cond ((member to osr-offsets) " (OSR entry)")
                                                  ((< to offset) " (loop back-edge)")
                                                  (t ""))))))
                      (t (let ((r (written-register text))) (when r (setf (aref regs r) nil))))))
                  (format out "  +~(~4,'0x~):  ~a~@[    ; ~a~]~%" offset text note)))
              (incf offset 4))))))))

(in-package "COMMON-LISP-USER")
