// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! SYMBOL-PACKAGE, STRING, SYMBOL-NAME, FIND-SYMBOL and GET from compiled
//! callers (bliss-gg3i1.7.19). SYMBOL-PACKAGE remembers that a symbol is
//! present in its home package until a symbol next leaves a package, so each
//! way of leaving one must still be seen by a warm caller.

use std::process::Command;

const PROGRAM: &str = r#"
(defun home (s) (symbol-package s))
(defun name (s) (symbol-name s))
(defun str (s) (string s))
(defun fs (n p) (multiple-value-list (find-symbol n p)))
(defun getp (s i) (get s i))
(defmacro check (form want)
  `(let ((got ,form))
     (unless (equal got ,want)
       (error "~S => ~S, want ~S" ',form got ,want))))
(defpackage "GG-A" (:use) (:export "EXT"))
(defpackage "GG-B" (:use "GG-A"))
(defun ext () (find-symbol "EXT" "GG-A"))
(dotimes (i 50)
  (home 'car) (home (ext)) (name 'car) (str :k) (fs "CAR" "CL") (getp 'car 'p))
(check (home 'car) (find-package "COMMON-LISP"))
(check (home :key) (find-package "KEYWORD"))
(check (home (ext)) (find-package "GG-A"))
(check (home (intern "EXT" "GG-B")) (find-package "GG-A"))
;; UNINTERN from the home package: the remembered answer must not survive.
(let ((s (intern "GONE" "GG-A")))
  (check (home s) (find-package "GG-A"))
  (check (home s) (find-package "GG-A"))
  (unintern s "GG-A")
  (check (home s) nil)
  ;; IMPORT of the now-homeless symbol gives it a home again.
  (import s "GG-B")
  (check (home s) (find-package "GG-B")))
;; SHADOWING-IMPORT displacing a present symbol.
(let ((old (intern "DISP" "GG-B")) (new (make-symbol "DISP")))
  (check (home old) (find-package "GG-B"))
  (shadowing-import new "GG-B")
  (check (home old) nil)
  (check (fs "DISP" "GG-B") (list new :internal)))
;; DELETE-PACKAGE.
(let* ((p (make-package "GG-DEL" :use nil)) (s (intern "Z" p)))
  (check (home s) p)
  (delete-package p)
  (check (home s) nil))
(check (home (make-symbol "U")) nil)
(check (home (gensym)) nil)
;; Names: package markers stripped, colons inside a name kept.
(check (name 'car) "CAR")
(check (name (ext)) "EXT")
(check (name :|A:B|) "A:B")
(check (name (intern "P::Q" "GG-A")) "P::Q")
(check (name (make-symbol "M:N")) "M:N")
(check (str '|lower|) "lower")
(check (str nil) "NIL")
(check (str t) "T")
(check (str "abc") "abc")
(check (str #\x) "x")
(let ((a (str 'car)) (b (str 'car)))
  (check (equal a b) t))
;; FIND-SYMBOL designators and statuses.
(check (fs "CAR" "CL") (list 'car :external))
(check (fs "CAR" :common-lisp) (list 'car :external))
(check (fs "CAR" (find-package "COMMON-LISP")) (list 'car :external))
(check (fs "EXT" "GG-B") (list (ext) :inherited))
(check (fs "HOME" "CL-USER") (list 'home :internal))
(check (fs "NO-SUCH-SYMBOL-HERE" "CL") (list nil nil))
(check (handler-case (fs "CAR" "NO-SUCH-PACKAGE-GG") (package-error () :package-error))
       :package-error)
;; GET with and without a default.
(setf (get (ext) 'color) 'red)
(check (getp (ext) 'color) 'red)
(check (getp (ext) 'size) nil)
(check (get (ext) 'size 42) 42)
(check (handler-case (getp 3 'color) (type-error () :type-error)) :type-error)
(format t "SYMBOL-BODIES-OK~%")
"#;

#[test]
fn symbol_builtins_answer_correctly_from_warm_compiled_callers() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("SYMBOL-BODIES-OK"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
