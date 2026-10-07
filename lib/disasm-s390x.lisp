;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;;; In-process z/Architecture (s390x) disassembler for DISASSEMBLE.
;;;;
;;;; The Rust side hands this file the native code of a T1/T2 function as a
;;;; list of octets and prints the string FORMAT-NATIVE-LISTING returns. Nothing
;;;; here is architecture-specific at runtime -- it is pure byte decoding -- so
;;;; the same code is exercised on every host by a fixture generated with GNU
;;;; objdump (bliss-ehjj1). Operand syntax follows binutils, including its
;;;; extended condition mnemonics, so a listing can be diffed against objdump.
;;;;
;;;; z/Architecture fixes an instruction's length by the top two bits of its
;;;; first byte (00: 2 bytes, 01/10: 4 bytes, 11: 6 bytes). An opcode this table
;;;; does not know is therefore still the right length, and is printed as a
;;;; `.byte` line, so offsets never drift past an unknown instruction.

(defpackage #:egcl-disasm
  (:use #:cl)
  (:export #:decode-instruction
           #:format-native-listing
           #:format-objdump-lines))

(in-package #:egcl-disasm)

;;; ------------------------------------------------------------------
;;; Opcode table
;;;
;;; KEY is how the opcode is located in the instruction:
;;;   (:b0 op)      one-byte opcode in byte 0
;;;   (:b01 op)     two-byte opcode in bytes 0-1 (RRE, RRF, S, E)
;;;   (:b0n op)     byte 0 plus the low nibble of byte 1 (RI, RIL)
;;;   (:b05 op)     byte 0 plus byte 5 (RXY, RSY, SIY, RIE, RRS, RIS)
;;; FORMAT names the operand layout, decoded by DECODE-OPERANDS below.

(defvar *opcodes* (make-hash-table :test #'equal))

(defun define-opcodes (key-kind entries)
  (dolist (entry entries)
    (destructuring-bind (op mnemonic format) entry
      (setf (gethash (list key-kind op) *opcodes*) (cons mnemonic format)))))

;; RR and friends: one-byte opcodes.
(define-opcodes :b0
  '((#x07 "bcr" :rr-bcr) (#x0a "svc" :i) (#x0d "basr" :rr)
    (#x10 "lpr" :rr) (#x11 "lnr" :rr) (#x12 "ltr" :rr) (#x13 "lcr" :rr)
    (#x14 "nr" :rr) (#x15 "clr" :rr) (#x16 "or" :rr) (#x17 "xr" :rr)
    (#x18 "lr" :rr) (#x19 "cr" :rr) (#x1a "ar" :rr) (#x1b "sr" :rr)
    (#x1c "mr" :rr) (#x1d "dr" :rr) (#x1e "alr" :rr) (#x1f "slr" :rr)
    (#x28 "ldr" :rr-f) (#x38 "ler" :rr-f)
    ;; RX
    (#x40 "sth" :rx) (#x41 "la" :rx) (#x42 "stc" :rx) (#x43 "ic" :rx)
    (#x44 "ex" :rx) (#x45 "bal" :rx) (#x46 "bct" :rx) (#x47 "bc" :rx-bc)
    (#x48 "lh" :rx) (#x49 "ch" :rx) (#x4a "ah" :rx) (#x4b "sh" :rx)
    (#x4c "mh" :rx) (#x4d "bas" :rx) (#x4e "cvd" :rx) (#x4f "cvb" :rx)
    (#x50 "st" :rx) (#x51 "lae" :rx) (#x54 "n" :rx) (#x55 "cl" :rx)
    (#x56 "o" :rx) (#x57 "x" :rx) (#x58 "l" :rx) (#x59 "c" :rx)
    (#x5a "a" :rx) (#x5b "s" :rx) (#x5c "m" :rx) (#x5d "d" :rx)
    (#x5e "al" :rx) (#x5f "sl" :rx)
    (#x60 "std" :rx-f) (#x68 "ld" :rx-f) (#x70 "ste" :rx-f) (#x71 "ms" :rx)
    (#x78 "le" :rx-f)
    ;; RSI
    (#x84 "brxh" :rsi) (#x85 "brxle" :rsi)
    ;; RS
    (#x88 "srl" :rs-shift) (#x89 "sll" :rs-shift) (#x8a "sra" :rs-shift)
    (#x8b "sla" :rs-shift) (#x8c "srdl" :rs-shift) (#x8d "sldl" :rs-shift)
    (#x8e "srda" :rs-shift) (#x8f "slda" :rs-shift)
    (#x90 "stm" :rs) (#x98 "lm" :rs) (#xba "cs" :rs) (#xbb "cds" :rs)
    (#xbd "clm" :rs-m) (#xbe "stcm" :rs-m) (#xbf "icm" :rs-m)
    ;; SI
    (#x91 "tm" :si) (#x92 "mvi" :si) (#x94 "ni" :si) (#x95 "cli" :si)
    (#x96 "oi" :si) (#x97 "xi" :si)
    ;; SS-a
    (#xd1 "mvn" :ss) (#xd2 "mvc" :ss) (#xd3 "mvz" :ss) (#xd4 "nc" :ss)
    (#xd5 "clc" :ss) (#xd6 "oc" :ss) (#xd7 "xc" :ss) (#xdc "tr" :ss)
    (#xdd "trt" :ss)))

;; Two-byte opcodes: RRE / RRF / S / E.
(define-opcodes :b01
  '((#x0101 "pr" :e) (#x01ff "trap2" :e)
    (#xb204 "sck" :s) (#xb205 "stck" :s) (#xb222 "ipm" :rre-r1)
    (#xb24e "sar" :rre-ar) (#xb24f "ear" :rre-ra) (#xb252 "msr" :rre) (#xb27c "stckf" :s)
    (#xb2b2 "lpswe" :s) (#xb2ff "trap4" :s)
    ;; BFP
    (#xb303 "lcebr" :rre-ff) (#xb304 "ldebr" :rre-ff) (#xb309 "cebr" :rre-ff)
    (#xb30a "aebr" :rre-ff) (#xb30b "sebr" :rre-ff) (#xb30d "debr" :rre-ff)
    (#xb313 "lcdbr" :rre-ff) (#xb314 "sqebr" :rre-ff) (#xb315 "sqdbr" :rre-ff)
    (#xb317 "meebr" :rre-ff) (#xb319 "cdbr" :rre-ff) (#xb31a "adbr" :rre-ff)
    (#xb31b "sdbr" :rre-ff) (#xb31c "mdbr" :rre-ff) (#xb31d "ddbr" :rre-ff)
    (#xb344 "ledbr" :rre-ff) (#xb357 "fiebr" :rrf-fmf) (#xb35f "fidbr" :rrf-fmf)
    (#xb374 "lzer" :rre-f1) (#xb375 "lzdr" :rre-f1)
    (#xb398 "cfebr" :rrf-rmf) (#xb399 "cfdbr" :rrf-rmf)
    (#xb3a1 "cdlgbr" :rrf-fmrm) (#xb3a4 "cegbr" :rrf-fmr) (#xb3a5 "cdgbr" :rrf-fmr)
    (#xb3a8 "cgebr" :rrf-rmf) (#xb3a9 "cgdbr" :rrf-rmf)
    (#xb3ac "clgebr" :rrf-rmfm) (#xb3ad "clgdbr" :rrf-rmfm)
    (#xb3c1 "ldgr" :rre-fr) (#xb3cd "lgdr" :rre-rf)
    ;; 64-bit general RRE
    (#xb900 "lpgr" :rre) (#xb901 "lngr" :rre) (#xb902 "ltgr" :rre) (#xb903 "lcgr" :rre)
    (#xb904 "lgr" :rre) (#xb905 "lurag" :rre) (#xb906 "lgbr" :rre) (#xb907 "lghr" :rre)
    (#xb908 "agr" :rre) (#xb909 "sgr" :rre) (#xb90a "algr" :rre) (#xb90b "slgr" :rre)
    (#xb90c "msgr" :rre) (#xb90d "dsgr" :rre) (#xb90e "eregg" :rre) (#xb90f "lrvgr" :rre)
    (#xb910 "lpgfr" :rre) (#xb911 "lngfr" :rre) (#xb912 "ltgfr" :rre) (#xb913 "lcgfr" :rre)
    (#xb914 "lgfr" :rre) (#xb916 "llgfr" :rre) (#xb917 "llgtr" :rre)
    (#xb918 "agfr" :rre) (#xb919 "sgfr" :rre) (#xb91a "algfr" :rre) (#xb91b "slgfr" :rre)
    (#xb91c "msgfr" :rre) (#xb91d "dsgfr" :rre) (#xb91f "lrvr" :rre)
    (#xb920 "cgr" :rre) (#xb921 "clgr" :rre) (#xb926 "lbr" :rre) (#xb927 "lhr" :rre)
    (#xb930 "cgfr" :rre) (#xb931 "clgfr" :rre)
    (#xb960 "cgrt" :rrf-cmp-trap) (#xb961 "clgrt" :rrf-cmp-trap)
    (#xb972 "crt" :rrf-cmp-trap) (#xb973 "clrt" :rrf-cmp-trap)
    (#xb980 "ngr" :rre) (#xb981 "ogr" :rre) (#xb982 "xgr" :rre) (#xb983 "flogr" :rre)
    (#xb984 "llgcr" :rre) (#xb985 "llghr" :rre) (#xb986 "mlgr" :rre) (#xb987 "dlgr" :rre)
    (#xb988 "alcgr" :rre) (#xb989 "slbgr" :rre) (#xb98a "cspg" :rre)
    (#xb994 "llcr" :rre) (#xb995 "llhr" :rre) (#xb996 "mlr" :rre) (#xb997 "dlr" :rre)
    (#xb998 "alcr" :rre) (#xb999 "slbr" :rre)
    (#xb9e1 "popcnt" :rre) (#xb9e2 "locgr" :rrf-loc) (#xb9e4 "ngrk" :rrf-a)
    (#xb9e6 "ogrk" :rrf-a) (#xb9e7 "xgrk" :rrf-a) (#xb9e8 "agrk" :rrf-a)
    (#xb9e9 "sgrk" :rrf-a) (#xb9ea "algrk" :rrf-a) (#xb9eb "slgrk" :rrf-a)
    (#xb9ec "mgrk" :rrf-a) (#xb9ed "msgrkc" :rrf-a)
    (#xb9f2 "locr" :rrf-loc) (#xb9f4 "nrk" :rrf-a) (#xb9f6 "ork" :rrf-a)
    (#xb9f7 "xrk" :rrf-a) (#xb9f8 "ark" :rrf-a) (#xb9f9 "srk" :rrf-a)
    (#xb9fa "alrk" :rrf-a) (#xb9fb "slrk" :rrf-a) (#xb9fd "msrkc" :rrf-a)))

;; Byte 0 plus the low nibble of byte 1: RI and RIL.
(define-opcodes :b0n
  '((#xa50 "iihh" :ri-u) (#xa51 "iihl" :ri-u) (#xa52 "iilh" :ri-u) (#xa53 "iill" :ri-u)
    (#xa54 "nihh" :ri-u) (#xa55 "nihl" :ri-u) (#xa56 "nilh" :ri-u) (#xa57 "nill" :ri-u)
    (#xa58 "oihh" :ri-u) (#xa59 "oihl" :ri-u) (#xa5a "oilh" :ri-u) (#xa5b "oill" :ri-u)
    (#xa5c "llihh" :ri-u) (#xa5d "llihl" :ri-u) (#xa5e "llilh" :ri-u) (#xa5f "llill" :ri-u)
    (#xa70 "tmlh" :ri-u) (#xa71 "tmll" :ri-u) (#xa72 "tmhh" :ri-u) (#xa73 "tmhl" :ri-u)
    (#xa74 "brc" :ri-brc) (#xa75 "bras" :ri-branch) (#xa76 "brct" :ri-branch)
    (#xa77 "brctg" :ri-branch)
    (#xa78 "lhi" :ri) (#xa79 "lghi" :ri) (#xa7a "ahi" :ri) (#xa7b "aghi" :ri)
    (#xa7c "mhi" :ri) (#xa7d "mghi" :ri) (#xa7e "chi" :ri) (#xa7f "cghi" :ri)
    ;; RIL
    (#xc00 "larl" :ril-branch) (#xc01 "lgfi" :ril) (#xc04 "brcl" :ril-brcl)
    (#xc05 "brasl" :ril-branch) (#xc06 "xihf" :ril-u) (#xc07 "xilf" :ril-u)
    (#xc08 "iihf" :ril-u) (#xc09 "iilf" :ril-u) (#xc0a "nihf" :ril-u) (#xc0b "nilf" :ril-u)
    (#xc0c "oihf" :ril-u) (#xc0d "oilf" :ril-u) (#xc0e "llihf" :ril-u) (#xc0f "llilf" :ril-u)
    (#xc20 "msgfi" :ril) (#xc21 "msfi" :ril) (#xc24 "slgfi" :ril-u) (#xc25 "slfi" :ril-u)
    (#xc28 "agfi" :ril) (#xc29 "afi" :ril) (#xc2a "algfi" :ril-u) (#xc2b "alfi" :ril-u)
    (#xc2c "cgfi" :ril) (#xc2d "cfi" :ril) (#xc2e "clgfi" :ril-u) (#xc2f "clfi" :ril-u)
    (#xc42 "llhrl" :ril-branch) (#xc44 "lghrl" :ril-branch) (#xc45 "lhrl" :ril-branch)
    (#xc46 "llghrl" :ril-branch) (#xc47 "sthrl" :ril-branch) (#xc48 "lgrl" :ril-branch)
    (#xc4b "stgrl" :ril-branch) (#xc4c "lgfrl" :ril-branch) (#xc4d "lrl" :ril-branch)
    (#xc4e "llgfrl" :ril-branch) (#xc4f "strl" :ril-branch)
    (#xc60 "exrl" :ril-branch) (#xc62 "pfdrl" :ril-mbranch) (#xc64 "cghrl" :ril-branch)
    (#xc65 "chrl" :ril-branch) (#xc66 "clghrl" :ril-branch) (#xc67 "clhrl" :ril-branch)
    (#xc68 "cgrl" :ril-branch) (#xc6a "clgrl" :ril-branch) (#xc6c "cgfrl" :ril-branch)
    (#xc6d "crl" :ril-branch) (#xc6e "clgfrl" :ril-branch) (#xc6f "clrl" :ril-branch)
    (#xcc6 "brcth" :ril-branch) (#xcc8 "aih" :ril) (#xcca "alsih" :ril) (#xccb "alsihn" :ril)
    (#xccd "cih" :ril) (#xccf "clih" :ril-u)))

;; Byte 0 plus byte 5: RXY (e3/ed), RSY/SIY (eb), RIE/RRS/RIS (ec); and SIL (e5)
;; which keys on byte 1 but is listed here under its own prefix.
(define-opcodes :b05
  '(;; RXY e3
    (#xe302 "ltg" :rxy) (#xe303 "lrag" :rxy) (#xe304 "lg" :rxy) (#xe306 "cvby" :rxy)
    (#xe308 "ag" :rxy) (#xe309 "sg" :rxy) (#xe30a "alg" :rxy) (#xe30b "slg" :rxy)
    (#xe30c "msg" :rxy) (#xe30d "dsg" :rxy) (#xe30e "cvbg" :rxy) (#xe30f "lrvg" :rxy)
    (#xe312 "lt" :rxy) (#xe313 "lray" :rxy) (#xe314 "lgf" :rxy) (#xe315 "lgh" :rxy)
    (#xe316 "llgf" :rxy) (#xe317 "llgt" :rxy) (#xe318 "agf" :rxy) (#xe319 "sgf" :rxy)
    (#xe31a "algf" :rxy) (#xe31b "slgf" :rxy) (#xe31c "msgf" :rxy) (#xe31d "dsgf" :rxy)
    (#xe31e "lrv" :rxy) (#xe31f "lrvh" :rxy) (#xe320 "cg" :rxy) (#xe321 "clg" :rxy)
    (#xe324 "stg" :rxy) (#xe326 "cvdy" :rxy) (#xe32e "cvdg" :rxy) (#xe32f "strvg" :rxy)
    (#xe330 "cgf" :rxy) (#xe331 "clgf" :rxy) (#xe332 "ltgf" :rxy) (#xe334 "cgh" :rxy)
    (#xe336 "pfd" :rxy-m) (#xe33e "strv" :rxy) (#xe33f "strvh" :rxy) (#xe346 "bctg" :rxy)
    (#xe350 "sty" :rxy) (#xe351 "msy" :rxy) (#xe354 "ny" :rxy) (#xe355 "cly" :rxy)
    (#xe356 "oy" :rxy) (#xe357 "xy" :rxy) (#xe358 "ly" :rxy) (#xe359 "cy" :rxy)
    (#xe35a "ay" :rxy) (#xe35b "sy" :rxy) (#xe35c "mfy" :rxy) (#xe35e "aly" :rxy)
    (#xe35f "sly" :rxy) (#xe370 "sthy" :rxy) (#xe371 "lay" :rxy) (#xe372 "stcy" :rxy)
    (#xe373 "icy" :rxy) (#xe375 "laey" :rxy) (#xe376 "lb" :rxy) (#xe377 "lgb" :rxy)
    (#xe378 "lhy" :rxy) (#xe379 "chy" :rxy) (#xe37a "ahy" :rxy) (#xe37b "shy" :rxy)
    (#xe380 "ng" :rxy) (#xe381 "og" :rxy) (#xe382 "xg" :rxy) (#xe385 "lgat" :rxy)
    (#xe386 "mlg" :rxy) (#xe387 "dlg" :rxy) (#xe388 "alcg" :rxy) (#xe389 "slbg" :rxy)
    (#xe38e "stpq" :rxy) (#xe38f "lpq" :rxy) (#xe390 "llgc" :rxy) (#xe391 "llgh" :rxy)
    (#xe394 "llc" :rxy) (#xe395 "llh" :rxy) (#xe396 "ml" :rxy) (#xe397 "dl" :rxy)
    (#xe398 "alc" :rxy) (#xe399 "slb" :rxy) (#xe39c "llgtat" :rxy) (#xe39d "llgfat" :rxy)
    (#xe39f "lat" :rxy) (#xe3c0 "lbh" :rxy) (#xe3c2 "llch" :rxy) (#xe3c4 "lhh" :rxy)
    (#xe3c6 "llhh" :rxy) (#xe3c7 "sthh" :rxy) (#xe3c8 "lfh" :rxy) (#xe3ca "stfh" :rxy)
    (#xe3cb "chf" :rxy) (#xe3cd "clhf" :rxy)
    ;; RXY ed (BFP)
    (#xed64 "ley" :rxy-f) (#xed65 "ldy" :rxy-f) (#xed66 "stey" :rxy-f) (#xed67 "stdy" :rxy-f)
    ;; RSY / SIY eb
    (#xeb04 "lmg" :rsy) (#xeb0a "srag" :rsy-shift) (#xeb0b "slag" :rsy-shift)
    (#xeb0c "srlg" :rsy-shift) (#xeb0d "sllg" :rsy-shift) (#xeb0f "tracg" :rsy)
    (#xeb14 "csy" :rsy) (#xeb1c "rllg" :rsy-shift) (#xeb1d "rll" :rsy-shift)
    (#xeb20 "clmh" :rsy-m) (#xeb21 "clmy" :rsy-m) (#xeb23 "clt" :rsy-cmp-trap-mem)
    (#xeb24 "stmg" :rsy) (#xeb25 "stctg" :rsy) (#xeb26 "stmh" :rsy)
    (#xeb2b "clgt" :rsy-cmp-trap-mem) (#xeb2c "stcmh" :rsy-m) (#xeb2d "stcmy" :rsy-m)
    (#xeb2f "lctlg" :rsy) (#xeb30 "csg" :rsy) (#xeb31 "cdsy" :rsy) (#xeb3e "cdsg" :rsy)
    (#xeb44 "bxhg" :rsy) (#xeb45 "bxleg" :rsy) (#xeb4c "ecag" :rsy)
    (#xeb51 "tmy" :siy-u) (#xeb52 "mviy" :siy-u) (#xeb54 "niy" :siy-u) (#xeb55 "cliy" :siy-u)
    (#xeb56 "oiy" :siy-u) (#xeb57 "xiy" :siy-u)
    (#xeb6a "asi" :siy) (#xeb6e "alsi" :siy) (#xeb7a "agsi" :siy) (#xeb7e "algsi" :siy)
    (#xeb80 "icmh" :rsy-m) (#xeb81 "icmy" :rsy-m) (#xeb8e "mvclu" :rsy) (#xeb8f "clclu" :rsy)
    (#xeb90 "stmy" :rsy) (#xeb96 "lmh" :rsy) (#xeb98 "lmy" :rsy) (#xeb9a "lamy" :rsy)
    (#xeb9b "stamy" :rsy)
    (#xebdc "srak" :rsy-shift) (#xebdd "slak" :rsy-shift) (#xebde "srlk" :rsy-shift)
    (#xebdf "sllk" :rsy-shift)
    (#xebe0 "locfh" :rsy-loc) (#xebe1 "stocfh" :rsy-loc) (#xebe2 "locg" :rsy-loc)
    (#xebe3 "stocg" :rsy-loc) (#xebe4 "lang" :rsy) (#xebe6 "laog" :rsy) (#xebe7 "laxg" :rsy)
    (#xebe8 "laag" :rsy) (#xebea "laalg" :rsy)
    (#xebf2 "loc" :rsy-loc) (#xebf3 "stoc" :rsy-loc) (#xebf4 "lan" :rsy) (#xebf6 "lao" :rsy)
    (#xebf7 "lax" :rsy) (#xebf8 "laa" :rsy) (#xebfa "laal" :rsy)
    ;; RIE / RRS / RIS ec
    (#xec42 "lochi" :rie-g) (#xec44 "brxhg" :rie-e) (#xec45 "brxlg" :rie-e)
    (#xec46 "locghi" :rie-g) (#xec4e "lochhi" :rie-g)
    (#xec51 "risblg" :rie-f) (#xec54 "rnsbg" :rie-f) (#xec55 "risbg" :rie-f)
    (#xec56 "rosbg" :rie-f) (#xec57 "rxsbg" :rie-f) (#xec59 "risbgn" :rie-f)
    (#xec5d "risbhg" :rie-f)
    (#xec64 "cgrj" :rie-b) (#xec65 "clgrj" :rie-b) (#xec76 "crj" :rie-b) (#xec77 "clrj" :rie-b)
    (#xec70 "cgit" :rie-a) (#xec71 "clgit" :rie-a-u) (#xec72 "cit" :rie-a) (#xec73 "clfit" :rie-a-u)
    (#xec7c "cgij" :rie-c) (#xec7d "clgij" :rie-c-u) (#xec7e "cij" :rie-c) (#xec7f "clij" :rie-c-u)
    (#xecd8 "ahik" :rie-d) (#xecd9 "aghik" :rie-d) (#xecda "alhsik" :rie-d) (#xecdb "alghsik" :rie-d)
    (#xece4 "cgrb" :rrs) (#xece5 "clgrb" :rrs) (#xecf6 "crb" :rrs) (#xecf7 "clrb" :rrs)
    (#xecfc "cgib" :ris) (#xecfd "clgib" :ris-u) (#xecfe "cib" :ris) (#xecff "clib" :ris-u)))

;; SIL: byte 0 = e5, opcode completed by byte 1.
(define-opcodes :b01
  '((#xe544 "mvhhi" :sil) (#xe548 "mvghi" :sil) (#xe54c "mvhi" :sil)
    (#xe554 "chhsi" :sil) (#xe555 "clhhsi" :sil-u) (#xe558 "cghsi" :sil)
    (#xe559 "clghsi" :sil-u) (#xe55c "chsi" :sil) (#xe55d "clfhsi" :sil-u)))

;;; ------------------------------------------------------------------
;;; Condition-code masks, as binutils spells them.

(defparameter *branch-suffixes*
  #("nop" "o" "h" "nle" "l" "nhe" "lh" "ne" "e" "nlh" "he" "nl" "le" "nh" "no" "")
  "Suffix for BRC/BRCL/BCR/BC and the LOC family by mask value; 15 is unconditional.")

(defparameter *compare-suffixes*
  #(nil nil "h" nil "l" nil "ne" nil "e" nil "nl" nil "nh" nil nil nil)
  "Suffix for compare-and-branch / compare-and-trap by mask: binutils abbreviates
only the six masks that name a comparison outcome; NIL prints the mask.")

(defparameter *loc-suffixes*
  #(nil "o" "h" "nle" "l" "nhe" "lh" "ne" "e" "nlh" "he" "nl" "le" "nh" "no" nil)
  "Suffix for the LOC family by mask; masks 0 and 15 print the mask.")

;;; ------------------------------------------------------------------
;;; Field helpers. BYTES is a vector of octets, POS the instruction start.

(declaim (inline byte-at hi lo))
(defun byte-at (bytes pos) (aref bytes pos))
(defun hi (b) (ash b -4))
(defun lo (b) (logand b 15))

(defun signed (value bits)
  (if (logbitp (1- bits) value) (- value (ash 1 bits)) value))

(defun u16 (bytes pos) (logior (ash (byte-at bytes pos) 8) (byte-at bytes (1+ pos))))
(defun s16 (bytes pos) (signed (u16 bytes pos) 16))
(defun u32 (bytes pos) (logior (ash (u16 bytes pos) 16) (u16 bytes (+ pos 2))))
(defun s32 (bytes pos) (signed (u32 bytes pos) 32))

(defun r (n) (format nil "%r~D" n))
(defun f (n) (format nil "%f~D" n))
(defun a (n) (format nil "%a~D" n))

(defun disp12 (bytes pos)
  "The 12-bit displacement whose high nibble is the low nibble of BYTES[POS]."
  (logior (ash (lo (byte-at bytes pos)) 8) (byte-at bytes (1+ pos))))

(defun disp20 (bytes pos)
  "The signed 20-bit displacement: DL at POS (12 bits) and DH at POS+2 (8 bits)."
  (signed (logior (ash (byte-at bytes (+ pos 2)) 12) (disp12 bytes pos)) 20))

(defun mem (disp base &optional (index 0))
  "A storage operand as binutils prints it: D, D(B), D(X,B), or D(X,0)."
  (cond ((and (zerop base) (zerop index)) (format nil "~D" disp))
        ((zerop index) (format nil "~D(~A)" disp (r base)))
        ((zerop base) (format nil "~D(~A,0)" disp (r index)))
        (t (format nil "~D(~A,~A)" disp (r index) (r base)))))

(defun mem-len (disp len base)
  (format nil "~D(~D,~A)" disp len (r base)))

(defun operands (&rest parts)
  (format nil "~{~A~^,~}" parts))

;;; ------------------------------------------------------------------
;;; Decoding

(defun instruction-length (first-byte)
  (case (ash first-byte -6) (0 2) (3 6) (t 4)))

(defun lookup (bytes pos len)
  "The (mnemonic . format) entry for the instruction at POS, or NIL."
  (let ((b0 (byte-at bytes pos)))
    (or (gethash (list :b0 b0) *opcodes*)
        ;; E-format instructions (trap2, pr) are two bytes with a 16-bit opcode.
        (gethash (list :b01 (logior (ash b0 8) (byte-at bytes (1+ pos)))) *opcodes*)
        (and (>= len 4)
             (gethash (list :b0n (logior (ash b0 4) (lo (byte-at bytes (1+ pos))))) *opcodes*))
        (and (= len 6)
             (gethash (list :b05 (logior (ash b0 8) (byte-at bytes (+ pos 5)))) *opcodes*)))))

(defun decode-instruction (bytes pos &key (target-printer #'default-target-printer))
  "Decode the instruction at POS. Returns (VALUES MNEMONIC OPERANDS LENGTH), where
MNEMONIC is NIL for an opcode the table does not know. TARGET-PRINTER renders a
relative branch destination given the absolute offset of the target."
  (let* ((len (instruction-length (byte-at bytes pos))))
    (if (> (+ pos len) (length bytes))
        (values nil nil (- (length bytes) pos))
        (let ((entry (lookup bytes pos len)))
          (if (null entry)
              (values nil nil len)
              (multiple-value-bind (mnemonic ops)
                  (decode-operands (car entry) (cdr entry) bytes pos target-printer)
                (values mnemonic ops len)))))))

(defun default-target-printer (target)
  (format nil "0x~(~X~)" target))

(defun branch-target (bytes pos halfwords target-printer)
  (funcall target-printer (+ pos (* 2 halfwords))))

(defun suffixed (mnemonic suffixes mask)
  "MNEMONIC plus the binutils condition suffix for MASK. Returns a second value
T when MASK has no suffix and must instead be printed as an operand."
  (let ((suffix (aref suffixes mask)))
    (if (null suffix)
        (values mnemonic t)
        (values (concatenate 'string mnemonic suffix) nil))))

(defun decode-operands (mnemonic format bytes pos tp)
  "Returns (VALUES MNEMONIC OPERAND-STRING) for FORMAT at POS."
  (let* ((b1 (byte-at bytes (1+ pos)))
         (r1 (hi b1))
         (r2 (lo b1)))
    (ecase format
      (:e (values mnemonic ""))
      (:i (values mnemonic (format nil "~D" b1)))
      (:rr (values mnemonic (operands (r r1) (r r2))))
      (:rr-f (values mnemonic (operands (f r1) (f r2))))
      (:rr-bcr
       ;; bcr M1,R2: br / b<cc>r / nopr, the register dropped only for nopr %r0.
       (cond ((= r1 15) (values "br" (r r2)))
             ((= r1 0) (values "nopr" (if (zerop r2) "" (r r2))))
             (t (values (format nil "b~Ar" (aref *branch-suffixes* r1)) (r r2)))))
      (:rre (values mnemonic (operands (r (hi (byte-at bytes (+ pos 3))))
                                       (r (lo (byte-at bytes (+ pos 3)))))))
      (:rre-ff (values mnemonic (operands (f (hi (byte-at bytes (+ pos 3))))
                                          (f (lo (byte-at bytes (+ pos 3)))))))
      (:rre-f1 (values mnemonic (f (hi (byte-at bytes (+ pos 3))))))
      (:rre-r1 (values mnemonic (r (hi (byte-at bytes (+ pos 3))))))
      (:rre-fr (values mnemonic (operands (f (hi (byte-at bytes (+ pos 3))))
                                          (r (lo (byte-at bytes (+ pos 3)))))))
      (:rre-rf (values mnemonic (operands (r (hi (byte-at bytes (+ pos 3))))
                                          (f (lo (byte-at bytes (+ pos 3)))))))
      (:rre-ra (values mnemonic (operands (r (hi (byte-at bytes (+ pos 3))))
                                          (a (lo (byte-at bytes (+ pos 3)))))))
      (:rre-ar (values mnemonic (operands (a (hi (byte-at bytes (+ pos 3))))
                                          (r (lo (byte-at bytes (+ pos 3)))))))
      ;; RRF-a: R1,R2,R3 with R3 in the high nibble of byte 2.
      (:rrf-a (let ((b2 (byte-at bytes (+ pos 2))) (b3 (byte-at bytes (+ pos 3))))
                (values mnemonic (operands (r (hi b3)) (r (lo b3)) (r (hi b2))))))
      ;; RRF-c with a condition: locgr R1,R2,M3 -> locgr<cc> R1,R2.
      (:rrf-loc (let ((b2 (byte-at bytes (+ pos 2))) (b3 (byte-at bytes (+ pos 3))))
                  (multiple-value-bind (name raw) (suffixed mnemonic *loc-suffixes* (hi b2))
                    (values name (if raw
                                     (operands (r (hi b3)) (r (lo b3)) (hi b2))
                                     (operands (r (hi b3)) (r (lo b3))))))))
      (:rrf-cmp-trap
       (let ((b2 (byte-at bytes (+ pos 2))) (b3 (byte-at bytes (+ pos 3))))
         (multiple-value-bind (name raw) (suffixed mnemonic *compare-suffixes* (hi b2))
           (values name (if raw
                            (operands (r (hi b3)) (r (lo b3)) (hi b2))
                            (operands (r (hi b3)) (r (lo b3))))))))
      ;; RRF-e: R1,M3,F2 (M3 in the high nibble of byte 2).
      (:rrf-rmf (let ((b2 (byte-at bytes (+ pos 2))) (b3 (byte-at bytes (+ pos 3))))
                  (values mnemonic (operands (r (hi b3)) (hi b2) (f (lo b3))))))
      (:rrf-rmfm (let ((b2 (byte-at bytes (+ pos 2))) (b3 (byte-at bytes (+ pos 3))))
                   (values mnemonic (operands (r (hi b3)) (hi b2) (f (lo b3)) (lo b2)))))
      (:rrf-fmr (let ((b3 (byte-at bytes (+ pos 3))))
                  (values mnemonic (operands (f (hi b3)) (r (lo b3))))))
      (:rrf-fmrm (let ((b2 (byte-at bytes (+ pos 2))) (b3 (byte-at bytes (+ pos 3))))
                   (values mnemonic (operands (f (hi b3)) (hi b2) (r (lo b3)) (lo b2)))))
      (:rrf-fmf (let ((b2 (byte-at bytes (+ pos 2))) (b3 (byte-at bytes (+ pos 3))))
                  (values mnemonic (operands (f (hi b3)) (hi b2) (f (lo b3))))))
      ;; RI
      (:ri (values mnemonic (operands (r r1) (s16 bytes (+ pos 2)))))
      (:ri-u (values mnemonic (operands (r r1) (u16 bytes (+ pos 2)))))
      (:ri-branch (values mnemonic (operands (r r1) (branch-target bytes pos (s16 bytes (+ pos 2)) tp))))
      (:ri-brc (multiple-value-bind (name) (suffixed "j" *branch-suffixes* r1)
                 (values name (branch-target bytes pos (s16 bytes (+ pos 2)) tp))))
      ;; RIL
      (:ril (values mnemonic (operands (r r1) (s32 bytes (+ pos 2)))))
      (:ril-u (values mnemonic (operands (r r1) (u32 bytes (+ pos 2)))))
      (:ril-branch (values mnemonic (operands (r r1) (branch-target bytes pos (s32 bytes (+ pos 2)) tp))))
      (:ril-mbranch (values mnemonic (operands r1 (branch-target bytes pos (s32 bytes (+ pos 2)) tp))))
      (:ril-brcl (multiple-value-bind (name) (suffixed "jg" *branch-suffixes* r1)
                   (values name (branch-target bytes pos (s32 bytes (+ pos 2)) tp))))
      ;; RX: R1,D2(X2,B2)
      (:rx (values mnemonic (operands (r r1) (mem (disp12 bytes (+ pos 2))
                                                  (hi (byte-at bytes (+ pos 2))) r2))))
      (:rx-f (values mnemonic (operands (f r1) (mem (disp12 bytes (+ pos 2))
                                                    (hi (byte-at bytes (+ pos 2))) r2))))
      (:rx-bc (let ((target (mem (disp12 bytes (+ pos 2)) (hi (byte-at bytes (+ pos 2))) r2)))
                (cond ((= r1 15) (values "b" target))
                      ((= r1 0) (values "nop" target))
                      (t (values (format nil "b~A" (aref *branch-suffixes* r1)) target)))))
      ;; RXY: R1,D2(X2,B2) with a 20-bit displacement
      (:rxy (values mnemonic (operands (r r1) (mem (disp20 bytes (+ pos 2))
                                                   (hi (byte-at bytes (+ pos 2))) r2))))
      (:rxy-f (values mnemonic (operands (f r1) (mem (disp20 bytes (+ pos 2))
                                                     (hi (byte-at bytes (+ pos 2))) r2))))
      (:rxy-m (values mnemonic (operands r1 (mem (disp20 bytes (+ pos 2))
                                                 (hi (byte-at bytes (+ pos 2))) r2))))
      ;; RS / RSY: R1,R3,D2(B2); shifts omit R3 (RS) or keep R3 as the source (RSY)
      (:rs (values mnemonic (operands (r r1) (r r2) (mem (disp12 bytes (+ pos 2))
                                                         (hi (byte-at bytes (+ pos 2)))))))
      (:rs-shift (values mnemonic (operands (r r1) (mem (disp12 bytes (+ pos 2))
                                                        (hi (byte-at bytes (+ pos 2)))))))
      (:rs-m (values mnemonic (operands (r r1) r2 (mem (disp12 bytes (+ pos 2))
                                                      (hi (byte-at bytes (+ pos 2)))))))
      (:rsy-m (values mnemonic (operands (r r1) r2 (mem (disp20 bytes (+ pos 2))
                                                       (hi (byte-at bytes (+ pos 2)))))))
      (:rsy (values mnemonic (operands (r r1) (r r2) (mem (disp20 bytes (+ pos 2))
                                                          (hi (byte-at bytes (+ pos 2)))))))
      (:rsy-shift (values mnemonic (operands (r r1) (r r2) (mem (disp20 bytes (+ pos 2))
                                                                (hi (byte-at bytes (+ pos 2)))))))
      (:rsy-loc (multiple-value-bind (name raw) (suffixed mnemonic *loc-suffixes* r2)
                  (let ((m (mem (disp20 bytes (+ pos 2)) (hi (byte-at bytes (+ pos 2))))))
                    (values name (if raw (operands (r r1) m r2) (operands (r r1) m))))))
      (:rsy-cmp-trap-mem (values mnemonic (operands (r r1) (mem (disp20 bytes (+ pos 2))
                                                                (hi (byte-at bytes (+ pos 2))))
                                                    r2)))
      ;; SI / SIY: D1(B1),I2
      (:si (values mnemonic (operands (mem (disp12 bytes (+ pos 2)) (hi (byte-at bytes (+ pos 2)))) b1)))
      (:siy-u (values mnemonic (operands (mem (disp20 bytes (+ pos 2)) (hi (byte-at bytes (+ pos 2)))) b1)))
      (:siy (values mnemonic (operands (mem (disp20 bytes (+ pos 2)) (hi (byte-at bytes (+ pos 2))))
                                       (signed b1 8))))
      ;; SIL: D1(B1),I2
      (:sil (values mnemonic (operands (mem (disp12 bytes (+ pos 2)) (hi (byte-at bytes (+ pos 2))))
                                       (s16 bytes (+ pos 4)))))
      (:sil-u (values mnemonic (operands (mem (disp12 bytes (+ pos 2)) (hi (byte-at bytes (+ pos 2))))
                                         (u16 bytes (+ pos 4)))))
      ;; RSI: R1,R3,RI2
      (:rsi (values mnemonic (operands (r r1) (r r2) (branch-target bytes pos (s16 bytes (+ pos 2)) tp))))
      ;; RIE-a: R1,I2,M3 -> cgit<cc> R1,I2
      ((:rie-a :rie-a-u)
       (let ((m3 (hi (byte-at bytes (+ pos 4))))
             (i2 (if (eq format :rie-a) (s16 bytes (+ pos 2)) (u16 bytes (+ pos 2)))))
         (multiple-value-bind (name raw) (suffixed mnemonic *compare-suffixes* m3)
           (values name (if raw (operands (r r1) i2 m3) (operands (r r1) i2))))))
      ;; RIE-b: R1,R2,M3,RI4 -> cgrj<cc> R1,R2,target
      (:rie-b
       (let ((m3 (hi (byte-at bytes (+ pos 4))))
             (target (branch-target bytes pos (s16 bytes (+ pos 2)) tp)))
         (multiple-value-bind (name raw) (suffixed mnemonic *compare-suffixes* m3)
           (values name (if raw (operands (r r1) (r r2) m3 target) (operands (r r1) (r r2) target))))))
      ;; RIE-c: R1,I2,M3,RI4 -> cgij<cc> R1,I2,target
      ((:rie-c :rie-c-u)
       (let ((i2 (let ((b (byte-at bytes (+ pos 4)))) (if (eq format :rie-c) (signed b 8) b)))
             (target (branch-target bytes pos (s16 bytes (+ pos 2)) tp)))
         (multiple-value-bind (name raw) (suffixed mnemonic *compare-suffixes* r2)
           (values name (if raw (operands (r r1) i2 r2 target) (operands (r r1) i2 target))))))
      ;; RIE-d: R1,R3,I2
      (:rie-d (values mnemonic (operands (r r1) (r r2) (s16 bytes (+ pos 2)))))
      ;; RIE-e: R1,R3,RI2
      (:rie-e (values mnemonic (operands (r r1) (r r2) (branch-target bytes pos (s16 bytes (+ pos 2)) tp))))
      ;; RIE-f: R1,R2,I3,I4,I5
      (:rie-f (values mnemonic (operands (r r1) (r r2) (byte-at bytes (+ pos 2))
                                         (byte-at bytes (+ pos 3)) (byte-at bytes (+ pos 4)))))
      ;; RIE-g: R1,I2,M3 -> locghi<cc> R1,I2
      (:rie-g (multiple-value-bind (name raw) (suffixed mnemonic *loc-suffixes* r2)
                (let ((i2 (s16 bytes (+ pos 2))))
                  (values name (if raw (operands (r r1) i2 r2) (operands (r r1) i2))))))
      ;; RRS: R1,R2,M3,D4(B4) -> cgrb<cc> R1,R2,D4(B4)
      (:rrs (let ((m3 (hi (byte-at bytes (+ pos 4))))
                  (m (mem (disp12 bytes (+ pos 2)) (hi (byte-at bytes (+ pos 2))))))
              (multiple-value-bind (name raw) (suffixed mnemonic *compare-suffixes* m3)
                (values name (if raw (operands (r r1) (r r2) m3 m) (operands (r r1) (r r2) m))))))
      ;; RIS: R1,I2,M3,D4(B4) -> cgib<cc> R1,I2,D4(B4)
      ((:ris :ris-u)
       (let ((i2 (let ((b (byte-at bytes (+ pos 4)))) (if (eq format :ris) (signed b 8) b)))
             (m (mem (disp12 bytes (+ pos 2)) (hi (byte-at bytes (+ pos 2))))))
         (multiple-value-bind (name raw) (suffixed mnemonic *compare-suffixes* r2)
           (values name (if raw (operands (r r1) i2 r2 m) (operands (r r1) i2 m))))))
      ;; S: D2(B2)
      (:s (values mnemonic (mem (disp12 bytes (+ pos 2)) (hi (byte-at bytes (+ pos 2))))))
      ;; SS-a: D1(L,B1),D2(B2)
      (:ss (values mnemonic (operands (mem-len (disp12 bytes (+ pos 2)) (1+ b1)
                                               (hi (byte-at bytes (+ pos 2))))
                                      (mem (disp12 bytes (+ pos 4)) (hi (byte-at bytes (+ pos 4))))))))))

;;; ------------------------------------------------------------------
;;; Listings

(defun as-octets (sequence)
  (if (and (typep sequence '(simple-array (unsigned-byte 8) (*))))
      sequence
      (make-array (length sequence) :element-type '(unsigned-byte 8)
                                    :initial-contents sequence)))

(defun hex-bytes (bytes pos len)
  (format nil "~{~(~2,'0X~)~^ ~}" (loop for i from pos below (+ pos len) collect (aref bytes i))))

(defun format-objdump-lines (sequence &key (base 0))
  "One line per instruction in objdump's `mnemonic<TAB>operands` shape, with
branch targets printed as absolute hex offsets from BASE. For the oracle test."
  (let ((bytes (as-octets sequence)))
    (with-output-to-string (out)
      (loop with pos = 0
            while (< pos (length bytes))
            do (multiple-value-bind (mnemonic ops len)
                   (decode-instruction bytes pos
                                       :target-printer (lambda (target)
                                                         (format nil "0x~(~X~)" (+ base target))))
                 (if mnemonic
                     (format out "~A~A~A~%" mnemonic (if (string= ops "") "" #\Tab) ops)
                     (format out ".byte~A~{0x~(~2,'0X~)~^,~}~%" #\Tab
                             (loop for i from pos below (+ pos len) collect (aref bytes i))))
                 (incf pos len))))))

(defun format-native-listing (sequence)
  "DISASSEMBLE's native section: `  +OFFSET:  BYTES   MNEMONIC OPERANDS`, with
relative branch destinations shown as +OFFSET into the same function."
  (let ((bytes (as-octets sequence)))
    (with-output-to-string (out)
      (loop with pos = 0
            while (< pos (length bytes))
            do (multiple-value-bind (mnemonic ops len)
                   (decode-instruction bytes pos
                                       :target-printer (lambda (target) (format nil "+~(~4,'0X~)" target)))
                 (format out "  +~(~4,'0X~):  ~18A " pos (hex-bytes bytes pos len))
                 (if mnemonic
                     (format out "~A~@[ ~A~]~%" mnemonic (if (string= ops "") nil ops))
                     (format out ".byte ~{0x~(~2,'0X~)~^, ~}~%"
                             (loop for i from pos below (+ pos len) collect (aref bytes i))))
                 (incf pos len))))))
