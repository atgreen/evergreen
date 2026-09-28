//! Application delivery starts from a saved world, never by replaying source.
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("torcl-delivery-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_owned()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn run(exe: &str, args: &[&str]) -> Output {
    Command::new("timeout")
        .args(["--kill-after=5", "180", exe, "--no-init"])
        .args(args)
        .output()
        .unwrap()
}
fn ok(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
const BIN: &str = env!("CARGO_BIN_EXE_torcl");

#[test]
fn saved_entry_preserves_class_instances_under_gc_stress() {
    let f = Fixture::new();
    let core = f.path("entry.core");
    let program = format!(
        r#"
      (defpackage :generic-shake (:use :cl))
      (in-package :generic-shake)
      (defun dead-target () :dead)
      (defgeneric unused (x))
      (defmethod unused ((x t)) (dead-target))
      (defun live-target () 42)
      (defgeneric live (x))
      (defmethod live ((x t)) (live-target))
      (defun method-target () 43)
      (defgeneric method-rooted (x))
      (defmethod method-rooted ((x t)) (method-target))
      (set '*method* (find-method 'method-rooted nil (list (find-class 't))))
      (defclass box () ((value :initarg :value :accessor value
                              :reader read-value :writer write-value)))
      (defun main ()
        (let ((box (make-instance 'box :value 7)))
          (funcall (intern "WRITE-VALUE" :generic-shake) 9 box)
          (write-line
            (if (and (= 42 (live nil))
                     (= 43 (funcall (intern "METHOD-ROOTED" :generic-shake) nil))
                     (eq *method* (find-method (intern "METHOD-ROOTED" :generic-shake)
                                               nil (list (find-class 't))))
                     (= 9 (funcall (intern "VALUE" :generic-shake) box))
                     (= 9 (funcall (intern "READ-VALUE" :generic-shake) box))
                     t)
                "GENERIC-SHAKE-OK" "WRONG"))))
      (save-lisp-and-die {core:?} :toplevel (quote generic-shake::main))
    "#
    );
    ok(run(BIN, &["--no-bootstrap", "--eval", &program]));
    let output = Command::new(BIN)
        .args(["--no-init", "--image", &core])
        .env("TORCL_GC_STRESS", "1")
        .env("TORCL_GC_POISON", "1")
        .output()
        .unwrap();
    assert!(ok(output).contains("GENERIC-SHAKE-OK"));
}

#[test]
fn delivery_distinguishes_source_functions_from_bytecode_only_functions() {
    let f = Fixture::new();
    let source = f.path("walker.lisp");
    let fasl = f.path("walker.bfasl");
    let spec = f.path("walker.delivery");
    fs::write(&source, "(defpackage :walker-app (:use :cl)) (in-package :walker-app) (defun main () (write-line \"WALKER-FREE\"))").unwrap();
    fs::write(&spec, "version = 1\nentry = WALKER-APP::MAIN\nprune-package = WALKER-APP\nruntime = specialized\ndynamic = explicit\n").unwrap();
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
    ));
    for (kind, input, setup, expected) in [
        ("source", &source, "", "capabilities=tree-walker\n"),
        ("bytecode", &fasl, "", "capabilities=\n"),
        (
            "saved-builtin",
            &fasl,
            "(set 'walker-app::*saved* 'vector)",
            "capabilities=tree-walker\n",
        ),
    ] {
        let core = f.path(&format!("{kind}.core"));
        ok(run(
            BIN,
            &[
                "--no-bootstrap",
                "--eval",
                &format!("(load {input:?}) {setup} (save-lisp-and-die {core:?})"),
            ],
        ));
        let report = ok(run(
            BIN,
            &[
                "--image",
                &core,
                "--deliver",
                &spec,
                "--output",
                &f.path(kind),
                "--dry-run",
            ],
        ));
        assert!(report.contains(expected), "{kind}: {report}");
    }
}

#[test]
fn delivery_retains_hidden_source_binders_and_methods() {
    let f = Fixture::new();
    let source = f.path("hidden.lisp");
    let fasl = f.path("hidden.bfasl");
    let spec = f.path("hidden.delivery");
    fs::write(&source, "(defpackage :hidden-walker (:use :cl)) (in-package :hidden-walker) (defun optional-main (&optional (x 7)) x) (defun method-main () (method-result))").unwrap();
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
    ));
    for (entry, setup, dependency) in [
        ("OPTIONAL-MAIN", "", "variadic lambda-list binder"),
        (
            "METHOD-MAIN",
            "(let ((x 7)) (defmethod hidden-walker::method-result () x))",
            "source generic method",
        ),
    ] {
        let core = f.path(&format!("{entry}.core"));
        ok(run(
            BIN,
            &[
                "--no-bootstrap",
                "--eval",
                &format!("(load {fasl:?}) {setup} (save-lisp-and-die {core:?})"),
            ],
        ));
        fs::write(&spec, format!("version = 1\nentry = HIDDEN-WALKER::{entry}\nprune-package = HIDDEN-WALKER\nruntime = specialized\ndynamic = explicit\n")).unwrap();
        let report = ok(run(
            BIN,
            &[
                "--image",
                &core,
                "--deliver",
                &spec,
                "--output",
                &f.path(entry),
                "--dry-run",
            ],
        ));
        assert!(report.contains("capabilities=tree-walker\n"), "{report}");
        assert!(report.contains(dependency), "{report}");
    }
}

#[test]
fn delivery_follows_only_reachable_compiled_closure_bodies_and_captures() {
    let f = Fixture::new();
    let source = f.path("closures.lisp");
    let fasl = f.path("closures.bfasl");
    let core = f.path("closures.core");
    let spec = f.path("closures.delivery");
    let exe = f.path("closures-app");
    fs::write(
        &source,
        r#"
      (defpackage :closure-shake (:use :cl))
      (in-package :closure-shake)
      (defun live-target () 42)
      (defun dead-target () :dead)
      (defun dead-captured-target () :dead-captured)
      (defun make-live (f)
        ;; Both closures share the captured frame. Only the second escapes.
        (lambda () (funcall f) (dead-target))
        (lambda () (funcall f)))
      (defun make-dead (f) (lambda () (funcall f) (dead-target) (disassemble 'dead-target)))
      (defun main ()
        (write-line (if (and (= 42 (funcall *saved*))
                             (not (fboundp (intern "DEAD-TARGET" :closure-shake)))
                             (not (fboundp (intern "DEAD-CAPTURED-TARGET" :closure-shake))))
                        "CLOSURE-SHAKE-OK" "WRONG")))
    "#,
    )
    .unwrap();
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
    ));
    fs::remove_file(&source).unwrap();
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!(
                "(load {fasl:?}) (set 'closure-shake::*saved* (closure-shake::make-live #'closure-shake::live-target)) (closure-shake::make-dead #'closure-shake::dead-captured-target) (save-lisp-and-die {core:?})"
            ),
        ],
    ));
    fs::remove_file(&fasl).unwrap();
    fs::write(&spec, "version = 1\nentry = CLOSURE-SHAKE::MAIN\nprune-package = CLOSURE-SHAKE\ndynamic = explicit\nruntime = specialized\n").unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(
        report.contains("remove CLOSURE-SHAKE::DEAD-TARGET"),
        "{report}"
    );
    assert!(
        report.contains("remove CLOSURE-SHAKE::DEAD-CAPTURED-TARGET"),
        "{report}"
    );
    assert!(
        report.contains("keep CLOSURE-SHAKE::LIVE-TARGET"),
        "{report}"
    );
    assert!(report.contains("capabilities=tree-walker\n"), "{report}");
    let removed: usize = report
        .lines()
        .find_map(|line| {
            line.strip_prefix("private-code-removed = ")
                .map(|n| n.parse().unwrap())
        })
        .unwrap();
    assert!(removed >= 2, "{report}");
    // Exercise publication/restore without making an ordinary regression test
    // rebuild a separate native release runtime.
    fs::write(&spec, "version = 1\nentry = CLOSURE-SHAKE::MAIN\nprune-package = CLOSURE-SHAKE\ndynamic = explicit\n").unwrap();
    ok(run(
        BIN,
        &["--image", &core, "--deliver", &spec, "--output", &exe],
    ));
    assert!(ok(run(&exe, &[])).contains("CLOSURE-SHAKE-OK"));
    // The code must actually disappear from the serialized registry, not just
    // be ignored by analysis. A second delivery has no orphan code to remove.
    let bytes = fs::read(&exe).unwrap();
    let size = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) as usize;
    let reduced = f.path("reduced.core");
    fs::write(&reduced, &bytes[bytes.len() - 16 - size..bytes.len() - 16]).unwrap();
    let again = ok(run(
        BIN,
        &[
            "--image",
            &reduced,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(again.contains("private-code-removed = 0\n"), "{again}");
}

#[test]
fn delivery_keeps_saved_macro_expanders_and_removes_unused_definitions() {
    let f = Fixture::new();
    let core = f.path("macro-handles.core");
    let spec = f.path("macro-handles.delivery");
    let exe = f.path("macro-handles");
    let program = format!(
        r#"
        (defpackage :macro-handles (:use :cl))
        (in-package :macro-handles)
        (defun dead-helper () 99)
        (defmacro unused () (dead-helper))
        (defun live-helper (x) (list 'quote x))
        (defmacro live (x) (live-helper x))
        (set '*expander* (macro-function 'live))
        (set '*alias* *expander*)
        (defmacro kept () 43)
        (defun main ()
          (write-line
            (if (and (equal '(quote 42) (funcall *expander* '(live 42) nil))
                     (eq *expander* *alias*)
                     (= 43 (macroexpand-1 (list (intern "KEPT" :macro-handles))))
                     (null (macro-function (intern "UNUSED" :macro-handles))))
                "MACRO-HANDLES-OK" "WRONG")))
        (save-lisp-and-die {core:?})
    "#
    );
    ok(run(BIN, &["--no-bootstrap", "--eval", &program]));
    fs::write(&spec, "version = 1\nentry = MACRO-HANDLES::MAIN\nprune-package = MACRO-HANDLES\ndynamic = explicit\nkeep = MACRO-HANDLES::KEPT\n").unwrap();
    let report = ok(run(
        BIN,
        &["--image", &core, "--deliver", &spec, "--output", &exe],
    ));
    for name in ["UNUSED", "DEAD-HELPER"] {
        assert!(
            report.contains(&format!("remove MACRO-HANDLES::{name}:")),
            "{report}"
        );
    }
    for name in ["LIVE", "LIVE-HELPER", "KEPT"] {
        assert!(
            report.contains(&format!("keep MACRO-HANDLES::{name}:")),
            "{report}"
        );
    }
    assert!(ok(run(&exe, &[])).contains("MACRO-HANDLES-OK"));
    let bytes = fs::read(&exe).unwrap();
    let size = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) as usize;
    let reduced = f.path("reduced.core");
    fs::write(&reduced, &bytes[bytes.len() - 16 - size..bytes.len() - 16]).unwrap();
    let again = ok(run(
        BIN,
        &[
            "--image",
            &reduced,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(!again.contains("MACRO-HANDLES::UNUSED:"), "{again}");
    assert!(!again.contains("MACRO-HANDLES::DEAD-HELPER:"), "{again}");
}

#[test]
fn delivery_prunes_setf_writers_and_preserves_saved_writer_functions() {
    for compiled in [false, true] {
        let f = Fixture::new();
        let source = f.path("writers.lisp");
        let fasl = f.path("writers.bfasl");
        let core = f.path("writers.core");
        let spec = f.path("writers.delivery");
        let exe = f.path("writers");
        fs::write(
            &source,
            r#"
          (defpackage :writer-live (:use :cl))
          (in-package :writer-live)
          (defun dead-helper () 99)
          (defun (setf unused) (value cell) (dead-helper))
          (defun (setf live) (value cell) (setf (car cell) value))
          (defun (setf saved) (value cell) (setf (car cell) value))
          (set '*writer* #'(setf saved))
          (set '*alias* *writer*)
          (defun main ()
            (let ((cell (list 0)))
              (setf (live cell) 7)
              (write-line
                (if (and (= 7 (car cell))
                         (= 9 (funcall *writer* 9 cell))
                         (= 9 (car cell))
                         (eq *writer* *alias*)
                         (not (fboundp (list 'setf (intern "UNUSED" :writer-live)))))
                    "WRITER-LIVE-OK" "WRONG"))))
        "#,
        )
        .unwrap();
        if compiled {
            ok(run(
                BIN,
                &[
                    "--no-bootstrap",
                    "--eval",
                    &format!("(compile-file {source:?} :output-file {fasl:?})"),
                ],
            ));
        }
        let input = if compiled { &fasl } else { &source };
        let mut save = Command::new(BIN);
        save.args([
            "--no-init",
            "--no-bootstrap",
            "--eval",
            &format!("(load {input:?}) (save-lisp-and-die {core:?})"),
        ]);
        if !compiled {
            save.env("TORCL_BACKEND", "tree-walker")
                .env("TORCL_LAZY_COMPILE", "0");
        }
        ok(save.output().unwrap());
        fs::write(&spec, "version = 1\nentry = WRITER-LIVE::MAIN\nprune-package = WRITER-LIVE\ndynamic = explicit\n").unwrap();
        let report = ok(run(
            BIN,
            &["--image", &core, "--deliver", &spec, "--output", &exe],
        ));
        assert!(
            report.contains("remove (SETF WRITER-LIVE::UNUSED):"),
            "{report}"
        );
        assert!(
            report.contains("remove WRITER-LIVE::DEAD-HELPER:"),
            "{report}"
        );
        for name in ["LIVE", "SAVED"] {
            assert!(
                report.contains(&format!("keep (SETF WRITER-LIVE::{name}):")),
                "{report}"
            );
        }
        assert!(ok(run(&exe, &[])).contains("WRITER-LIVE-OK"));
        let bytes = fs::read(&exe).unwrap();
        let size = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) as usize;
        let reduced = f.path("reduced.core");
        fs::write(&reduced, &bytes[bytes.len() - 16 - size..bytes.len() - 16]).unwrap();
        let again = ok(run(
            BIN,
            &[
                "--image",
                &reduced,
                "--deliver",
                &spec,
                "--output",
                &exe,
                "--dry-run",
            ],
        ));
        assert!(!again.contains("(SETF WRITER-LIVE::UNUSED):"), "{again}");
        assert!(!again.contains("WRITER-LIVE::DEAD-HELPER:"), "{again}");
    }
}

#[test]
fn delivery_preserves_legacy_writer_with_an_ambiguous_package_owner() {
    let f = Fixture::new();
    let core = f.path("legacy-writer.core");
    let spec = f.path("legacy-writer.delivery");
    let exe = f.path("legacy-writer");
    // Model an older BFASL: only the private function cell is present, not
    // OUT.::X. The unrelated selected accessor OUT::|.X| has the same mangling.
    let program = format!(
        r#"
      (defpackage :out. (:use :cl))
      (defpackage :out (:use :cl) (:intern ".X"))
      (defun torcl-internal::%setf-writer-out...x (value target) (eval value))
      (defun out::main () (write-line "OK"))
      (save-lisp-and-die {core:?})
    "#
    );
    ok(run(BIN, &["--no-bootstrap", "--eval", &program]));
    fs::write(&spec, "version = 1\nentry = OUT::MAIN\nprune-package = OUT\ndynamic = explicit\nruntime = specialized\n").unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(
        report.contains("capabilities=disassembly,dynamic-code,tree-walker\n"),
        "{report}"
    );
    assert!(!report.contains("remove (SETF OUT::.X):"), "{report}");
}

#[test]
fn native_delivery_ignores_eval_in_an_unreachable_setf_writer() {
    let f = Fixture::new();
    let source = f.path("writer-main.lisp");
    let fasl = f.path("writer-main.bfasl");
    let core = f.path("writers.core");
    let spec = f.path("writers.delivery");
    let exe = f.path("writers");
    fs::write(&source, "(defpackage :writer-shake (:use :cl)) (in-package :writer-shake) (defun main () (write-line \"WRITER-SHAKE-OK\"))").unwrap();
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
    ));
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!(
                "(load {fasl:?}) (defun (setf writer-shake::unused) (value target) (eval value)) (save-lisp-and-die {core:?})"
            ),
        ],
    ));
    let specification = "version = 1\nentry = WRITER-SHAKE::MAIN\nprune-package = WRITER-SHAKE\ndynamic = explicit\nruntime = specialized\n";
    fs::write(&spec, specification).unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(report.contains("capabilities=\n"), "{report}");
    assert!(
        report.contains("remove (SETF WRITER-SHAKE::UNUSED):"),
        "{report}"
    );
    fs::write(
        &spec,
        format!("{specification}keep = WRITER-SHAKE::UNUSED\n"),
    )
    .unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(
        report.contains("capabilities=disassembly,dynamic-code,tree-walker\n"),
        "{report}"
    );
    assert!(
        report.contains("keep (SETF WRITER-SHAKE::UNUSED):"),
        "{report}"
    );
}

#[test]
fn native_delivery_ignores_eval_in_an_unreachable_macro() {
    let f = Fixture::new();
    let source = f.path("main.lisp");
    let fasl = f.path("main.bfasl");
    let core = f.path("macros.core");
    let spec = f.path("macros.delivery");
    let exe = f.path("macros");
    fs::write(&source, "(defpackage :macro-shake (:use :cl)) (in-package :macro-shake) (defun main () (write-line \"MACRO-SHAKE-OK\"))").unwrap();
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
    ));
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!(
                "(load {fasl:?}) (defmacro macro-shake::unused (form) (eval form)) (save-lisp-and-die {core:?})"
            ),
        ],
    ));
    let specification = "version = 1\nentry = MACRO-SHAKE::MAIN\nprune-package = MACRO-SHAKE\ndynamic = explicit\nruntime = specialized\n";
    fs::write(&spec, specification).unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(report.contains("capabilities=\n"), "{report}");
    assert!(report.contains("remove MACRO-SHAKE::UNUSED:"), "{report}");
    fs::write(
        &spec,
        format!("{specification}keep = MACRO-SHAKE::UNUSED\n"),
    )
    .unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(
        report.contains("capabilities=disassembly,dynamic-code,tree-walker\n"),
        "{report}"
    );
    assert!(report.contains("keep MACRO-SHAKE::UNUSED:"), "{report}");
}

#[test]
fn native_delivery_prunes_unused_bytecode_macro() {
    let f = Fixture::new();
    let source = f.path("compiled-macros.lisp");
    let fasl = f.path("compiled-macros.bfasl");
    let core = f.path("compiled-macros.core");
    let spec = f.path("compiled-macros.delivery");
    let exe = f.path("compiled-macros");
    fs::write(&source, "(defpackage :compiled-macros (:use :cl)) (in-package :compiled-macros) (defmacro unused (form) (eval form)) (defun main () (write-line \"OK\"))").unwrap();
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
    ));
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!("(load {fasl:?}) (save-lisp-and-die {core:?})"),
        ],
    ));
    fs::write(&spec, "version = 1\nentry = COMPILED-MACROS::MAIN\nprune-package = COMPILED-MACROS\ndynamic = explicit\nruntime = specialized\n").unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(report.contains("capabilities=\n"), "{report}");
    assert!(
        report.contains("remove COMPILED-MACROS::UNUSED:"),
        "{report}"
    );
}

#[test]
fn native_delivery_ignores_eval_in_an_unreachable_method() {
    let f = Fixture::new();
    let source = f.path("main.lisp");
    let fasl = f.path("main.bfasl");
    let core = f.path("methods.core");
    let spec = f.path("methods.delivery");
    let exe = f.path("methods");
    fs::write(&source, "(defpackage :generic-eval (:use :cl)) (in-package :generic-eval) (defun main () (write-line \"OK\"))").unwrap();
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
    ));
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!(
                r#"
        (load {fasl:?})
        (in-package :generic-eval)
        (defgeneric unused (form))
        (defmethod unused ((form t)) (eval form))
        (defgeneric start ())
        (defmethod start () (write-line "GENERIC-ENTRY-OK"))
        (save-lisp-and-die {core:?})
    "#
            ),
        ],
    ));
    let specification = "version = 1\nentry = GENERIC-EVAL::MAIN\nprune-package = GENERIC-EVAL\ndynamic = explicit\nruntime = specialized\n";
    fs::write(&spec, specification).unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(report.contains("capabilities=\n"), "{report}");
    assert!(report.contains("remove GENERIC-EVAL::UNUSED:"), "{report}");
    fs::write(
        &spec,
        format!("{specification}keep = GENERIC-EVAL::UNUSED\n"),
    )
    .unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(
        report.contains("capabilities=disassembly,dynamic-code,tree-walker\n"),
        "{report}"
    );
    assert!(report.contains("keep GENERIC-EVAL::UNUSED:"), "{report}");
    fs::write(&spec, "version = 1\nentry = GENERIC-EVAL::START\nprune-package = GENERIC-EVAL\ndynamic = explicit\n").unwrap();
    ok(run(
        BIN,
        &["--image", &core, "--deliver", &spec, "--output", &exe],
    ));
    assert!(ok(run(&exe, &[])).contains("GENERIC-ENTRY-OK"));
}

#[test]
fn delivery_prunes_generic_owners_but_keeps_methods_and_class_accessors() {
    let f = Fixture::new();
    let core = f.path("generics.core");
    let spec = f.path("generics.delivery");
    let exe = f.path("generics");
    let program = format!(
        r#"
      (defpackage :generic-shake (:use :cl))
      (in-package :generic-shake)
      (defun dead-target () :dead)
      (defgeneric unused (x))
      (defmethod unused ((x t)) (dead-target))
      (defun live-target () 42)
      (defgeneric live (x))
      (defmethod live ((x t)) (live-target))
      (defun method-target () 43)
      (defgeneric method-rooted (x))
      (defmethod method-rooted ((x t)) (method-target))
      (set '*method* (find-method 'method-rooted nil (list (find-class 't))))
      (defclass box () ((value :initarg :value :accessor value
                              :reader read-value :writer write-value)))
      (defun main ()
        (let ((box (make-instance 'box :value 7)))
          (funcall (intern "WRITE-VALUE" :generic-shake) 9 box)
          (write-line
            (if (and (= 42 (live nil))
                     (= 43 (funcall (intern "METHOD-ROOTED" :generic-shake) nil))
                     (eq *method* (find-method (intern "METHOD-ROOTED" :generic-shake)
                                               nil (list (find-class 't))))
                     (= 9 (funcall (intern "VALUE" :generic-shake) box))
                     (= 9 (funcall (intern "READ-VALUE" :generic-shake) box))
                     (not (fboundp (intern "UNUSED" :generic-shake))))
                "GENERIC-SHAKE-OK" "WRONG"))))
      (save-lisp-and-die {core:?})
    "#
    );
    ok(run(BIN, &["--no-bootstrap", "--eval", &program]));
    let specification = "version = 1\nentry = GENERIC-SHAKE::MAIN\nprune-package = GENERIC-SHAKE\ndynamic = explicit\n";
    fs::write(&spec, specification).unwrap();
    let report = ok(run(
        BIN,
        &["--image", &core, "--deliver", &spec, "--output", &exe],
    ));
    for name in ["UNUSED", "DEAD-TARGET"] {
        assert!(
            report.contains(&format!("remove GENERIC-SHAKE::{name}:")),
            "{report}"
        );
    }
    for name in [
        "LIVE",
        "LIVE-TARGET",
        "METHOD-ROOTED",
        "METHOD-TARGET",
        "VALUE",
        "READ-VALUE",
        "WRITE-VALUE",
    ] {
        assert!(
            report.contains(&format!("keep GENERIC-SHAKE::{name}:")),
            "{report}"
        );
    }
    assert!(ok(run(&exe, &[])).contains("GENERIC-SHAKE-OK"));
    // Re-delivery reads the serialized registries, so a removed generic or
    // method cannot survive unnoticed in a host-side metadata table.
    let bytes = fs::read(&exe).unwrap();
    let size = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) as usize;
    let reduced = f.path("reduced-generics.core");
    fs::write(&reduced, &bytes[bytes.len() - 16 - size..bytes.len() - 16]).unwrap();
    let second_exe = f.path("generics-again");
    let again = ok(run(
        BIN,
        &[
            "--image",
            &reduced,
            "--deliver",
            &spec,
            "--output",
            &second_exe,
        ],
    ));
    assert!(!again.contains("GENERIC-SHAKE::UNUSED:"), "{again}");
    assert!(!again.contains("GENERIC-SHAKE::DEAD-TARGET:"), "{again}");
    assert!(ok(run(&second_exe, &[])).contains("GENERIC-SHAKE-OK"));
    fs::write(
        &spec,
        format!("{specification}keep = GENERIC-SHAKE::UNUSED\n"),
    )
    .unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(report.contains("keep GENERIC-SHAKE::UNUSED:"), "{report}");
    assert!(
        report.contains("keep GENERIC-SHAKE::DEAD-TARGET:"),
        "{report}"
    );
}

#[test]
fn delivery_traces_source_closures_only_from_reachable_handles() {
    let f = Fixture::new();
    let core = f.path("source-closures.core");
    let spec = f.path("source-closures.delivery");
    let exe = f.path("source-closures");
    let program = format!(
        r#"
        (defpackage :source-closure-shake (:use :cl))
        (in-package :source-closure-shake)
        (defun live-target () 42)
        (defun dead-target () :dead)
        (eval (list 'defun 'unused nil
                    (list 'quote
                          (let ((target 'dead-target)
                                (payload (make-string 200000 :initial-element #\x)))
                            (lambda () (list payload (funcall target)))))))
        (let ((target 'live-target) (count 0))
          (set '*saved* (lambda () (setq count (1+ count)) (funcall target)))
          (set '*sibling* (lambda () count)))
        (set '*alias* *saved*)
        (defun main ()
          (write-line (if (and (= 42 (funcall *saved*))
                               (= 1 (funcall *sibling*))
                               (eq *saved* *alias*))
                          "SOURCE-CLOSURE-OK" "WRONG")))
        (save-lisp-and-die {core:?})
        "#
    );
    ok(Command::new(BIN)
        .args(["--no-init", "--no-bootstrap", "--eval", &program])
        .env("TORCL_BACKEND", "tree-walker")
        .env("TORCL_LAZY_COMPILE", "0")
        .output()
        .unwrap());
    fs::write(&spec, "version = 1\nentry = SOURCE-CLOSURE-SHAKE::MAIN\nprune-package = SOURCE-CLOSURE-SHAKE\ndynamic = explicit\n").unwrap();
    let report = ok(run(
        BIN,
        &["--image", &core, "--deliver", &spec, "--output", &exe],
    ));
    assert!(
        report.contains("remove SOURCE-CLOSURE-SHAKE::UNUSED"),
        "{report}"
    );
    assert!(
        report.contains("remove SOURCE-CLOSURE-SHAKE::DEAD-TARGET"),
        "{report}"
    );
    assert!(
        report.contains("keep SOURCE-CLOSURE-SHAKE::LIVE-TARGET"),
        "{report}"
    );
    assert!(ok(run(&exe, &[])).contains("SOURCE-CLOSURE-OK"));
    let removed: usize = report
        .lines()
        .find_map(|line| {
            line.strip_prefix("source-closures-removed = ")
                .map(|n| n.parse().unwrap())
        })
        .unwrap();
    assert!(removed > 0, "{report}");
    let bytes = fs::read(&exe).unwrap();
    let size = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) as usize;
    assert!(
        fs::metadata(&core).unwrap().len() > size as u64 + 100000,
        "unreachable captured payload must leave the compacted image"
    );
    let reduced = f.path("reduced.core");
    fs::write(&reduced, &bytes[bytes.len() - 16 - size..bytes.len() - 16]).unwrap();
    let again = ok(run(
        BIN,
        &[
            "--image",
            &reduced,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(again.contains("source-closures-removed = 0\n"), "{again}");
}

#[test]
fn delivery_prunes_unreachable_functions_and_keeps_data_and_explicit_roots() {
    let f = Fixture::new();
    let image = f.path("input.core");
    let spec = f.path("app.delivery");
    let exe = f.path("app");
    let save = format!(
        r#"
      (defpackage :delivery-test (:use :cl))
      (in-package :delivery-test)
      (defun helper () 42)
      (defun callback () :callback)
      (defun data-function () :data)
      (defparameter *dispatch* (vector 'data-function))
      (defparameter *path* #P"delivery/test.lisp")
      (defun table-function () :table)
      (defparameter *table* (make-hash-table))
      (setf (gethash :entry *table*) 'table-function)
      (setf (gethash :cycle *table*) *table*)
      (defun dead-a () (dead-b))
      (defun dead-b () (dead-a))
      (defun main ()
        (assert (= 42 (helper)))
        (assert (eq :callback (funcall (intern "CALLBACK" :delivery-test))))
        (assert (eq :data (funcall (aref *dispatch* 0))))
        (assert (string= "test" (pathname-name *path*)))
        (assert (eq :table (funcall (gethash :entry *table*))))
        (assert (not (fboundp (intern "DEAD-A" :delivery-test))))
        (format t "DELIVERED-OK~%"))
      (save-lisp-and-die {image:?} :toplevel (lambda () (error "Input entry must not run")))
    "#
    );
    ok(run(BIN, &["--eval", &save]));
    let original = fs::read(&image).unwrap();
    fs::write(&spec, "version = 1\nentry = DELIVERY-TEST::MAIN\nprune-package = DELIVERY-TEST\nkeep = DELIVERY-TEST::CALLBACK\ndynamic = explicit\n").unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &image,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(report.contains("remove DELIVERY-TEST::DEAD-A"), "{report}");
    assert!(!std::path::Path::new(&exe).exists());
    let report = ok(run(
        BIN,
        &["--image", &image, "--deliver", &spec, "--output", &exe],
    ));
    assert!(report.contains("remove DELIVERY-TEST::DEAD-B"), "{report}");
    assert!(ok(run(&exe, &[])).contains("DELIVERED-OK"));
    assert_eq!(original, fs::read(&image).unwrap());
    assert!(fs::metadata(format!("{exe}.manifest")).unwrap().len() > 0);
    ok(run(
        BIN,
        &[
            "--image",
            &image,
            "--eval",
            "(assert (fboundp 'delivery-test::dead-a))",
        ],
    ));
}

#[test]
fn resaving_an_executable_replaces_the_previous_embedded_image() {
    let f = Fixture::new();
    let first = f.path("first");
    let second = f.path("second");
    ok(run(
        BIN,
        &[
            "--eval",
            &format!("(save-lisp-and-die {first:?} :executable t)"),
        ],
    ));
    ok(run(
        &first,
        &[
            "--eval",
            &format!("(save-lisp-and-die {second:?} :executable t)"),
        ],
    ));
    fn prefix(path: &str) -> usize {
        let bytes = fs::read(path).unwrap();
        assert_eq!(&bytes[bytes.len() - 16..bytes.len() - 8], b"TORCLEXE");
        let len = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) as usize;
        bytes.len() - 16 - len
    }
    assert_eq!(
        prefix(&first),
        prefix(&second),
        "a previous image must not become part of the runtime prefix"
    );
    ok(run(&second, &["--eval", "(assert (= 3 (+ 1 2)))"]));
}

#[test]
fn delivery_of_source_free_code_keeps_closures_methods_and_unwind_paths() {
    let f = Fixture::new();
    let source = f.path("compiled.lisp");
    let fasl = f.path("compiled.bfasl");
    let image = f.path("compiled.core");
    let spec = f.path("compiled.delivery");
    let exe = f.path("compiled-app");
    fs::write(
        &source,
        r#"
      (defpackage :delivery-code (:use :cl))
      (in-package :delivery-code)
      (defun helper (n) (+ n 1))
      (defun cleanup () (format t "CLEANUP-OK~%"))
      (defun unused () (make-string 20000))
      (defun closure-maker (n) (lambda () (helper n)))
      (defmethod compute ((n integer)) (helper n))
      (defun main ()
        (assert (= 42 (funcall *saved-closure*)))
        (assert (= 42 (compute 41)))
        (assert (= 42 (handler-case
          (unwind-protect (error "exercise cold path") (cleanup))
          (error () (helper 41)))))
        (let ((sum 0))
          (dotimes (i 1000001) (incf sum (helper 1)))
          (assert (= sum 2000002)))
        ;; Exercise a different numeric type after fixnum warmup, and a
        ;; restart body whose dependencies live in nested bytecode.
        (assert (= 2.5 (helper 1.5)))
        (assert (= 42 (restart-case (invoke-restart 'answer)
          (answer () (helper 41)))))
        (assert (not (fboundp (intern "UNUSED" :delivery-code))))
        (format t "COMPILED-DELIVERY-OK~%"))
    "#,
    )
    .unwrap();
    ok(run(
        BIN,
        &[
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
    ));
    fs::remove_file(&source).unwrap();
    ok(run(
        BIN,
        &[
            "--eval",
            &format!(
                "(load {fasl:?}) (defparameter delivery-code::*saved-closure* (delivery-code::closure-maker 41)) (save-lisp-and-die {image:?})"
            ),
        ],
    ));
    fs::remove_file(&fasl).unwrap();
    fs::write(&spec, "version = 1\nentry = DELIVERY-CODE::MAIN\nprune-package = DELIVERY-CODE\ndynamic = explicit\n").unwrap();
    let report = ok(run(
        BIN,
        &["--image", &image, "--deliver", &spec, "--output", &exe],
    ));
    assert!(report.contains("remove DELIVERY-CODE::UNUSED"), "{report}");
    let output = ok(run(&exe, &[]));
    assert!(output.contains("CLEANUP-OK"), "{output}");
    assert!(output.contains("COMPILED-DELIVERY-OK"), "{output}");
}

#[test]
fn delivery_defaults_to_preserving_dynamic_targets_and_rejects_bad_specs() {
    let f = Fixture::new();
    let image = f.path("input.core");
    let spec = f.path("policy.delivery");
    let exe = f.path("app");
    ok(run(
        BIN,
        &[
            "--eval",
            &format!(
                r#"
      (defpackage :delivery-policy (:use :cl))
      (defun delivery-policy::target () 19)
      (defun delivery-policy::main ()
        (assert (= 19 (funcall (intern "TARGET" :delivery-policy))))
        (format t "DYNAMIC-OK~%"))
      (save-lisp-and-die {image:?})"#
            ),
        ],
    ));
    fs::write(
        &spec,
        "version = 1\nentry = DELIVERY-POLICY::MAIN\nprune-package = DELIVERY-POLICY\n",
    )
    .unwrap();
    let report = ok(run(
        BIN,
        &["--image", &image, "--deliver", &spec, "--output", &exe],
    ));
    assert!(report.contains("dynamic = preserve"));
    assert!(!report.contains("remove DELIVERY-POLICY::TARGET"));
    assert!(ok(run(&exe, &[])).contains("DYNAMIC-OK"));
    let previous = fs::read(&exe).unwrap();
    for bad in [
        "version = 2\nentry = DELIVERY-POLICY::MAIN\n",
        "version = 1\nentry = DELIVERY-POLICY::MISSING\n",
        "version = 1\nentry = DELIVERY-POLICY::MAIN\nprune-package = COMMON-LISP\n",
        "version = 1\nentry = DELIVERY-POLICY::MAIN\ndynamci = explicit\n",
    ] {
        fs::write(&spec, bad).unwrap();
        let result = run(
            BIN,
            &["--image", &image, "--deliver", &spec, "--output", &exe],
        );
        assert!(!result.status.success());
        assert_eq!(previous, fs::read(&exe).unwrap());
    }
    fs::write(&spec, "version = 1\nentry = DELIVERY-POLICY::MAIN\n").unwrap();
    let original = fs::read(&image).unwrap();
    assert!(
        !run(
            BIN,
            &["--image", &image, "--deliver", &spec, "--output", &image]
        )
        .status
        .success()
    );
    assert_eq!(original, fs::read(&image).unwrap());
    // Explicit delivery input wins over the image embedded in the driver.
    assert!(
        ok(run(
            &exe,
            &[
                "--image",
                &image,
                "--deliver",
                &spec,
                "--output",
                &f.path("other"),
                "--dry-run"
            ]
        ))
        .contains("entry = DELIVERY-POLICY::MAIN")
    );
}

#[test]
fn failed_publication_preserves_the_existing_manifest() {
    let f = Fixture::new();
    let image = f.path("input.core");
    let spec = f.path("app.delivery");
    let exe = f.path("directory");
    fs::create_dir(&exe).unwrap();
    let manifest = format!("{exe}.manifest");
    fs::write(&manifest, "previous manifest").unwrap();
    ok(run(
        BIN,
        &[
            "--eval",
            &format!("(defun main () 42) (save-lisp-and-die {image:?})"),
        ],
    ));
    fs::write(&spec, "version = 1\nentry = CL-USER::MAIN\n").unwrap();
    assert!(
        !run(
            BIN,
            &["--image", &image, "--deliver", &spec, "--output", &exe]
        )
        .status
        .success()
    );
    assert_eq!(fs::read_to_string(manifest).unwrap(), "previous manifest");
}

#[test]
fn delivery_reduces_the_embedded_core_when_unused_code_has_large_constants() {
    let f = Fixture::new();
    let source = f.path("large.lisp");
    let image = f.path("large.core");
    let spec = f.path("large.delivery");
    let exe = f.path("small-app");
    fs::write(
        &source,
        format!(
            r#"
      (defpackage :delivery-size (:use :cl))
      (in-package :delivery-size)
      (defun unused () "{}")
      (defun main () (format t "SMALL-OK~%"))
      (assert (> (length (unused)) 100000))
      (save-lisp-and-die {image:?})
    "#,
            "unreachable payload ".repeat(10000)
        ),
    )
    .unwrap();
    ok(run(BIN, &["--load", &source]));
    fs::remove_file(source).unwrap();
    fs::write(&spec, "version = 1\nentry = DELIVERY-SIZE::MAIN\nprune-package = DELIVERY-SIZE\ndynamic = explicit\n").unwrap();
    ok(run(
        BIN,
        &["--image", &image, "--deliver", &spec, "--output", &exe],
    ));
    let bytes = fs::read(&exe).unwrap();
    let core_size = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap());
    let input_size = fs::metadata(&image).unwrap().len();
    let payload = b"unreachable payload unreachable payload";
    assert!(
        fs::read(&image)
            .unwrap()
            .windows(payload.len())
            .any(|w| w == payload)
    );
    assert!(
        !bytes.windows(payload.len()).any(|w| w == payload),
        "unused constant must be absent, including from the restored pinned heap (offset {:?}, core starts {}): {}",
        bytes.windows(payload.len()).position(|w| w == payload),
        bytes.len() as u64 - 16 - core_size,
        fs::read_to_string(format!("{exe}.manifest")).unwrap()
    );
    let wide_payload: Vec<u8> = payload
        .iter()
        .flat_map(|&b| u32::from(b).to_ne_bytes())
        .collect();
    assert!(
        !bytes.windows(wide_payload.len()).any(|w| w == wide_payload),
        "unused wide-character constant must be absent from the restored pinned heap"
    );
    assert!(
        core_size < input_size,
        "delivered core {core_size} must be smaller than input {input_size}"
    );
    assert!(ok(run(&exe, &[])).contains("SMALL-OK"));
}

#[test]
fn saved_images_validate_native_requirements_before_restore() {
    let f = Fixture::new();
    let core = f.path("contract.core");
    ok(run(
        BIN,
        &["--eval", &format!("(save-lisp-and-die {core:?})")],
    ));
    let mut bytes = fs::read(&core).unwrap();
    let marker = b"schema=1\nsource=";
    let offset = bytes
        .windows(marker.len())
        .position(|w| w == marker)
        .expect("saved core contains native runtime requirements")
        + marker.len();
    bytes[offset] = if bytes[offset] == b'0' { b'1' } else { b'0' };
    fs::write(&core, bytes).unwrap();
    let result = run(BIN, &["--image", &core]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("runtime source mismatch"));
}

#[test]
fn native_delivery_capabilities_follow_reachable_symbols_and_dynamic_policy() {
    let f = Fixture::new();
    let core = f.path("caps.core");
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!(
                r#"
        (defpackage :native-delivery (:use :cl))
        (in-package :native-delivery)
        (defun main () (format t "NATIVE-OK~%"))
        (defun inspect-main () (disassemble 'main))
        (defun eval-main (form) (eval form))
        (defun load-main () (load "later.lisp"))
        (save-lisp-and-die {core:?})"#
            ),
        ],
    ));
    let spec = f.path("caps.delivery");
    for (entry, policy, keep, expected) in [
        ("MAIN", "explicit", "", "capabilities=tree-walker\n"),
        (
            "EVAL-MAIN",
            "explicit",
            "",
            "capabilities=disassembly,dynamic-code,tree-walker\n",
        ),
        (
            "LOAD-MAIN",
            "explicit",
            "",
            "capabilities=disassembly,dynamic-code,tree-walker\n",
        ),
        (
            "INSPECT-MAIN",
            "explicit",
            "",
            "capabilities=disassembly,tree-walker\n",
        ),
        (
            "MAIN",
            "preserve",
            "",
            "capabilities=disassembly,dynamic-code,tree-walker\n",
        ),
        (
            "MAIN",
            "explicit",
            "runtime-keep = disassembly\n",
            "capabilities=disassembly,tree-walker\n",
        ),
    ] {
        fs::write(&spec, format!("version = 1\nentry = NATIVE-DELIVERY::{entry}\nprune-package = NATIVE-DELIVERY\nruntime = specialized\ndynamic = {policy}\n{keep}")).unwrap();
        let report = ok(run(
            BIN,
            &[
                "--image",
                &core,
                "--deliver",
                &spec,
                "--output",
                &f.path("out"),
                "--dry-run",
            ],
        ));
        assert!(report.contains(expected), "{report}");
        if entry == "EVAL-MAIN" || entry == "LOAD-MAIN" {
            assert!(
                report.contains("keep NATIVE-DELIVERY::INSPECT-MAIN"),
                "{report}"
            );
        }
    }
}

#[test]
#[ignore = "builds a matching release runtime; requires Cargo, target toolchain and nm"]
fn native_delivery_removes_the_walker_for_source_free_code() {
    let f = Fixture::new();
    let source = f.path("walker-free.lisp");
    let fasl = f.path("walker-free.bfasl");
    let core = f.path("walker-free.core");
    let spec = f.path("walker-free.delivery");
    let exe = f.path("walker-free");
    fs::write(
        &source,
        r#"
        (defpackage :walker-free (:use :cl))
        (in-package :walker-free)
        (defun add-one (x) (+ x 1))
        (defun (setf unused-writer) (value target) (eval value))
        (defun main ()
          (dotimes (i 1000) (add-one i))
          (write-line (if (= 2.5 (add-one 1.5)) "WALKER-FREE-OK" "WRONG"))
          (handler-case (funcall 'add-one)
            (program-error () (write-line "ARITY-OK")))
          (handler-case (funcall 'missing)
            (undefined-function () (write-line "UNDEFINED-OK"))))
    "#,
    )
    .unwrap();
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
    ));
    fs::remove_file(&source).unwrap();
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!(
                "(load {fasl:?}) (defgeneric walker-free::unused (x)) (defmethod walker-free::unused ((x t)) (eval x)) (defmacro walker-free::unused-macro (x) (eval x)) (save-lisp-and-die {core:?})"
            ),
        ],
    ));
    fs::remove_file(&fasl).unwrap();
    fs::write(&spec, "version = 1\nentry = WALKER-FREE::MAIN\nprune-package = WALKER-FREE\nruntime = specialized\ndynamic = explicit\n").unwrap();
    let report = ok(Command::new(BIN)
        .args([
            "--no-init",
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
        ])
        .output()
        .unwrap());
    assert!(report.contains("capabilities=\n"), "{report}");
    assert!(report.contains("remove WALKER-FREE::UNUSED:"), "{report}");
    assert!(
        report.contains("remove WALKER-FREE::UNUSED-MACRO:"),
        "{report}"
    );
    assert!(
        report.contains("remove (SETF WALKER-FREE::UNUSED-WRITER):"),
        "{report}"
    );
    let symbols = ok(Command::new("nm").args(["-C", &exe]).output().unwrap());
    assert!(symbols.contains("torcl::cli::"), "missing symbol table");
    assert!(
        !symbols.contains("torcl::cli::eval_list"),
        "tree walker remains linked"
    );
    assert!(
        !symbols.contains("torcl::cli::bytecode::Lowerer::"),
        "source-to-bytecode compiler remains linked"
    );
    assert!(
        !symbols.contains("iced_x86::"),
        "disassembler remains linked"
    );
    for tier in ["t0", "t1", "t2"] {
        let output = ok(Command::new(&exe)
            .arg("--no-init")
            .env("TORCL_FORCE_TIER", tier)
            .output()
            .unwrap());
        assert_eq!(output, "WALKER-FREE-OK\nARITY-OK\nUNDEFINED-OK\n", "{tier}");
    }
}

#[test]
#[ignore = "builds a matching release runtime; requires Cargo, target toolchain and nm"]
fn native_delivery_builds_and_runs_without_decoder() {
    let f = Fixture::new();
    let core = f.path("native.core");
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!(
                r#"
        (defpackage :native-app (:use :cl))
        (in-package :native-app)
        (defun add-one (x) (+ x 1))
        (defun main ()
          (dotimes (i 1000) (add-one i))
          (write-line (if (= (add-one 1.5) 2.5) "NATIVE-OK" "WRONG")))
        (save-lisp-and-die {core:?})"#
            ),
        ],
    ));
    let spec = f.path("native.delivery");
    let exe = f.path("app");
    fs::write(&spec, "version = 1\nentry = NATIVE-APP::MAIN\nprune-package = NATIVE-APP\nruntime = specialized\ndynamic = explicit\n").unwrap();
    let output = Command::new(BIN)
        .args([
            "--no-init",
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
        ])
        .output()
        .unwrap();
    let report = ok(output);
    assert!(report.contains("capabilities=tree-walker\n"), "{report}");
    assert!(report.contains("native-bytes = "));
    assert!(ok(run(&exe, &[])).contains("NATIVE-OK"));
    for (key, value) in [("TORCL_BACKEND", "treewalker"), ("TORCL_FORCE_TIER", "t2")] {
        let output = Command::new(&exe)
            .arg("--no-init")
            .env(key, value)
            .output()
            .unwrap();
        assert!(ok(output).contains("NATIVE-OK"));
    }
    let symbols = Command::new("nm").arg("-C").arg(&exe).output().unwrap();
    let symbols = ok(symbols);
    assert!(!symbols.contains("iced_x86::"), "decoder remains linked");
    assert!(
        symbols.contains("torcl::"),
        "symbol table must be present to prove removal"
    );
    let eval = run(&exe, &["--eval", "(disassemble 'native-app::main)"]);
    assert!(!eval.status.success());
    assert!(String::from_utf8_lossy(&eval.stderr).contains("dynamic code entry points are absent"));
    let rejected = run(&exe, &["--image", &core]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("unavailable native runtime capabilities")
    );
    let original = fs::read(&exe).unwrap();
    let manifest = fs::read(format!("{exe}.manifest")).unwrap();
    let failed = run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--runtime-source",
            &f.path("missing-source"),
        ],
    );
    assert!(!failed.status.success());
    assert_eq!(fs::read(&exe).unwrap(), original);
    assert_eq!(fs::read(format!("{exe}.manifest")).unwrap(), manifest);
}

#[test]
fn explicit_image_overrides_a_saved_executables_embedded_core() {
    let f = Fixture::new();
    let exe = f.path("embedded");
    let core = f.path("explicit.core");
    ok(run(
        BIN,
        &[
            "--eval",
            &format!(
                r#"(defun embedded-main () (format t "EMBEDDED~%")) (save-lisp-and-die {exe:?} :executable t :toplevel 'embedded-main)"#
            ),
        ],
    ));
    ok(run(
        BIN,
        &[
            "--eval",
            &format!(
                r#"(defun explicit-main () (format t "EXPLICIT~%")) (save-lisp-and-die {core:?} :toplevel 'explicit-main)"#
            ),
        ],
    ));
    let output = ok(run(&exe, &["--image", &core]));
    assert!(output.contains("EXPLICIT"), "{output}");
    assert!(!output.contains("EMBEDDED"), "{output}");
}

#[test]
fn native_delivery_retains_evaluation_for_saved_raw_lambda_data() {
    let f = Fixture::new();
    let core = f.path("raw.core");
    let spec = f.path("raw.delivery");
    ok(run(
        BIN,
        &[
            "--no-bootstrap",
            "--eval",
            &format!(
                r#"
      (defpackage :raw-app (:use :cl))
      (in-package :raw-app)
      (set '*fn* '(lambda () 42))
      (defun main () (funcall *fn*))
      (save-lisp-and-die {core:?})"#
            ),
        ],
    ));
    fs::write(&spec, "version = 1\nentry = RAW-APP::MAIN\nprune-package = RAW-APP\nruntime = specialized\ndynamic = explicit\n").unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &core,
            "--deliver",
            &spec,
            "--output",
            &f.path("app"),
            "--dry-run",
        ],
    ));
    assert!(
        report.contains("capabilities=disassembly,dynamic-code,tree-walker\n"),
        "{report}"
    );
    assert!(report.contains("native-root = source lambda"), "{report}");
}
