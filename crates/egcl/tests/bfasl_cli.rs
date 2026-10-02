// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! End-to-end `.bfasl` compile/load through the CLI (bliss-lb6.6, spec §6.11):
//! compile a source file to a `.bfasl`, load it in a *fresh* process, and verify
//! version-mismatch rejection.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn workdir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("egcl-bfasl-{}-{}", std::process::id(), tag));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn run(program: &str) -> std::process::Output {
    Command::new(BIN)
        .args(["--eval", program])
        .output()
        .expect("spawn egcl")
}

fn bfasl_section(bytes: &[u8], wanted: u16) -> Option<&[u8]> {
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
    let mut pos = 24usize;
    let checksum_at = bytes.len().checked_sub(4)?;
    for _ in 0..count {
        if pos + 6 > checksum_at {
            return None;
        }
        let kind = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap());
        let len = u32::from_le_bytes(bytes[pos + 2..pos + 6].try_into().unwrap()) as usize;
        pos += 6;
        if pos + len > checksum_at {
            return None;
        }
        if kind == wanted {
            return Some(&bytes[pos..pos + len]);
        }
        pos += len;
    }
    None
}

fn bbu_counts(bytes: &[u8]) -> (u32, u32, u32) {
    let bbu = bfasl_section(bytes, 12).expect("compile-file must emit BYTECODE_UNIT");
    assert_eq!(&bbu[..4], b"BBU\0", "BYTECODE_UNIT has BBU magic");
    let flags = u32::from_le_bytes(bbu[8..12].try_into().unwrap());
    assert_ne!(flags & 1, 0, "new writers must emit a complete BBU");
    (
        u32::from_le_bytes(bbu[12..16].try_into().unwrap()),
        u32::from_le_bytes(bbu[16..20].try_into().unwrap()),
        u32::from_le_bytes(bbu[20..24].try_into().unwrap()),
    )
}

fn bbu_action_start(bbu: &[u8]) -> usize {
    let constant_count = u32::from_le_bytes(bbu[12..16].try_into().unwrap()) as usize;
    let function_count = u32::from_le_bytes(bbu[16..20].try_into().unwrap()) as usize;
    let mut pos = 40usize;
    for _ in 0..constant_count {
        let tag = bbu[pos];
        pos += 1;
        match tag {
            0 | 1 => {}
            2 | 6 => pos += 8,
            3 => {
                pos += 1;
                let len = u32::from_le_bytes(bbu[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4 + len;
            }
            5 | 7 | 12 => pos += 4,
            8 => {
                let len = u32::from_le_bytes(bbu[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4 + len;
            }
            10 => {
                pos += 4;
                let count = u32::from_le_bytes(bbu[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4 + count * 4;
            }
            14 => {
                let count = u32::from_le_bytes(bbu[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4 + count * 4;
            }
            11 => pos += 9,
            13 | 16 | 17 => pos += 8,
            18 => pos += 4,
            19 => {
                let rank = u32::from_le_bytes(bbu[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4 + rank * 8;
                let count = u32::from_le_bytes(bbu[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4 + count * 4;
            }
            other => panic!("test BBU parser does not know constant tag {other}"),
        }
    }
    for _ in 0..function_count {
        pos += 24;
        let code_len = u32::from_le_bytes(bbu[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4 + code_len;
        let literal_count = u32::from_le_bytes(bbu[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4 + literal_count * 4;
        let handler_count = u32::from_le_bytes(bbu[pos..pos + 4].try_into().unwrap());
        assert_eq!(handler_count, 0);
        pos += 4;
        let pc_info_count = u32::from_le_bytes(bbu[pos..pos + 4].try_into().unwrap());
        assert_eq!(pc_info_count, 0);
        pos += 8;
    }
    pos
}

#[test]
fn macro_generated_flet_captures_compile_without_source_fallback() {
    let dir = workdir("macro-flet-capture");
    let src = dir.join("capture.lisp");
    let out = dir.join("capture.bfasl");
    fs::write(
        &src,
        r#"
      (defmacro with-hidden-local (&body body)
        `(flet ((reader () ,@body)) (reader)))
      (defun macro-flet-capture (x)
        (with-hidden-local (setq x (+ x 1)) x))
      (defun macro-flet-heap (x)
        (with-hidden-local (cons :head x)))
      (format t "CAPTURE ~S ~S~%" (macro-flet-capture 41) (macro-flet-heap (list :tail)))
    "#,
    )
    .unwrap();
    for stress in [false, true] {
        let mut command = Command::new(BIN);
        command.args([
            "--no-init",
            "--no-bootstrap",
            "--eval",
            &format!("(compile-file {src:?} :output-file {out:?})"),
        ]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_VERIFY", "1");
        }
        let compiled = command.output().unwrap();
        assert!(
            compiled.status.success(),
            "compile stress={stress}: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let bytes = fs::read(&out).unwrap();
        assert!(bfasl_section(&bytes, 11).is_none());
        let bbu = bfasl_section(&bytes, 12).unwrap();
        let start = bbu_action_start(bbu);
        let (_, _, count) = bbu_counts(&bytes);
        for action in bbu[start..start + count as usize * 14].chunks_exact(14) {
            assert_ne!(
                action[0], 9,
                "macro-generated FLET fell back to EvalSource, stress={stress}"
            );
        }
    }
    fs::remove_file(src).unwrap();
    for stress in [false, true] {
        let mut command = Command::new(BIN);
        command
            .args(["--no-init", "--no-bootstrap", "--load"])
            .arg(&out);
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_VERIFY", "1");
        }
        let loaded = command.output().unwrap();
        assert!(
            loaded.status.success(),
            "{}",
            String::from_utf8_lossy(&loaded.stderr)
        );
        assert!(String::from_utf8_lossy(&loaded.stdout).contains("CAPTURE 42 (:HEAD :TAIL)"));
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn load_time_values_are_initialized_once_per_fasl_load() {
    let dir = workdir("load-time-values");
    let src = dir.join("cells.lisp");
    let out = dir.join("cells.bfasl");
    fs::write(
        &src,
        r#"
      (defun ltv-a () (load-time-value (list (incf *ltv-count*))))
      (defun ltv-b () (load-time-value (list (incf *ltv-count*))))
      (defun ltv-maker (x)
        (lambda () (cons x (load-time-value (list (incf *ltv-count*))))))
      (defun ltv-scope (ltv-global)
        (load-time-value (list ltv-global *ltv-dynamic*)))
      (defun ltv-primary () (load-time-value (values 7 8) t))
      (defun ltv-recursive () (load-time-value (list (load-time-value (list :inner) nil))))
      (defun ltv-symbol-scope ()
        (symbol-macrolet ((ltv-global :shadow)) (load-time-value ltv-global)))
      (defmacro ltv-expand () :global-macro)
      (defun ltv-macro-scope ()
        (macrolet ((ltv-expand () :local-macro)) (load-time-value (ltv-expand))))
    "#,
    )
    .unwrap();
    let compiled = run(&format!(
        "(setq *ltv-count* 0) (compile-file {src:?} :output-file {out:?}) (format t \"COMPILE-COUNT ~S~%\" *ltv-count*)"
    ));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    assert!(String::from_utf8_lossy(&compiled.stdout).contains("COMPILE-COUNT 0"));
    let bytes = fs::read(&out).unwrap();
    assert!(bfasl_section(&bytes, 11).is_none());
    assert!(
        bbu_counts(&bytes).1 >= 6,
        "LTV functions and nested closure must compile"
    );
    let bbu = bfasl_section(&bytes, 12).unwrap();
    let start = bbu_action_start(bbu);
    let count = u32::from_le_bytes(bbu[20..24].try_into().unwrap()) as usize;
    for action in bbu[start..start + count * 14].chunks_exact(14) {
        assert_ne!(action[0], 9, "LOAD-TIME-VALUE fell back to EvalSource");
    }
    fs::remove_file(src).unwrap();
    let program = format!(
        r#"
      (setq *ltv-count* 0 ltv-global :global *ltv-dynamic* :outer)
      (let ((ltv-global :lexical) (*ltv-dynamic* :dynamic)) (load {out:?}))
      (format t "LOAD-COUNT ~S~%" *ltv-count*)
      (setq *ltv-old* (ltv-a))
      (format t "CELLS ~S ~S ~S ~S~%" (ltv-a) (ltv-b) (eq (ltv-a) (ltv-a)) (eq (ltv-a) (ltv-b)))
      (setq *ltv-f* (ltv-maker 10) *ltv-g* (ltv-maker 20))
      (format t "NESTED ~S ~S ~S~%" (funcall *ltv-f*) (funcall *ltv-g*) (eq (cdr (funcall *ltv-f*)) (cdr (funcall *ltv-g*))))
      (format t "SCOPE ~S PRIMARY ~S~%" (ltv-scope :argument) (multiple-value-list (ltv-primary)))
      (format t "RECURSIVE ~S~%" (ltv-recursive))
      (format t "MACROS ~S ~S~%" (ltv-symbol-scope) (ltv-macro-scope))
      (format t "CALL-COUNT ~S~%" *ltv-count*)
      (load {out:?})
      (format t "RELOAD ~S ~S ~S ~S~%" *ltv-old* (ltv-a) (ltv-b) *ltv-count*)
    "#
    );
    for (tier, stress) in [("t0", false), ("t0", true), ("t1", true)] {
        let mut command = Command::new(BIN);
        command.args(["--no-init", "--no-bootstrap", "--eval", &program]);
        command.env("EGCL_FORCE_TIER", tier);
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_VERIFY", "1");
        }
        let loaded = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&loaded.stdout);
        assert!(
            loaded.status.success(),
            "tier={tier}, stress={stress}: {stdout}\n{}",
            String::from_utf8_lossy(&loaded.stderr)
        );
        for expected in [
            "LOAD-COUNT 3",
            "CELLS (1) (2) T NIL",
            "NESTED (10 3) (20 3) T",
            "SCOPE (:GLOBAL :DYNAMIC) PRIMARY (7)",
            "RECURSIVE ((:INNER))",
            "MACROS :GLOBAL :GLOBAL-MACRO",
            "CALL-COUNT 3",
            "RELOAD (1) (4) (5) 6",
        ] {
            assert!(
                stdout.contains(expected),
                "tier={tier}, stress={stress}, missing {expected}: {stdout}"
            );
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn load_time_value_errors_stop_loading_and_bad_actions_are_prevalidated() {
    let dir = workdir("load-time-value-errors");
    let src = dir.join("error.lisp");
    let good = dir.join("error.bfasl");
    fs::write(
        &src,
        r#"
      (defun ltv-before-error () :before)
      (defun ltv-error () (load-time-value (progn (incf *ltv-errors*) (error "LTV initializer"))))
      (defun ltv-after-error () :after)
    "#,
    )
    .unwrap();
    let compiled = run(&format!("(compile-file {src:?} :output-file {good:?})"));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&good).unwrap();
    let bbu = bfasl_section(&bytes, 12).unwrap();
    let start = bbu_action_start(bbu);
    let count = u32::from_le_bytes(bbu[20..24].try_into().unwrap()) as usize;
    let init = (0..count)
        .map(|i| start + i * 14)
        .find(|&pos| bbu[pos] == 11)
        .expect("must compile a SetLoadTimeCell action");
    let probe = |path: &PathBuf| {
        run(&format!(
            r#"
      (setq *ltv-errors* 0)
      (handler-case (load {path:?}) (error () (format t "CAUGHT~%")))
      (format t "STATE ~S ~S ~S ~S~%" *ltv-errors* (fboundp 'ltv-before-error) (fboundp 'ltv-error) (fboundp 'ltv-after-error))
    "#
        ))
    };
    let loaded = probe(&good);
    let stdout = String::from_utf8_lossy(&loaded.stdout);
    assert!(
        loaded.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert!(
        stdout.contains("CAUGHT") && stdout.contains("STATE 1 T NIL NIL"),
        "{stdout}"
    );

    for corruption in ["cell", "thunk", "role", "flags", "extra", "version"] {
        let image = egcl_rt::bfasl::load(&bytes).unwrap();
        let mut builder = egcl_rt::bfasl::BfaslBuilder::new();
        for (kind, section) in image.sections() {
            let mut section = section.to_vec();
            if kind == egcl_rt::bfasl::section::BYTECODE_UNIT {
                match corruption {
                    "cell" => section[init + 2..init + 6].copy_from_slice(&u32::MAX.to_le_bytes()),
                    "thunk" => {
                        section[init + 6..init + 10].copy_from_slice(&u32::MAX.to_le_bytes())
                    }
                    "role" => section[init + 6..init + 10].copy_from_slice(&0u32.to_le_bytes()),
                    "flags" => section[init + 1] = 1,
                    "extra" => section[init + 10..init + 14].copy_from_slice(&0u32.to_le_bytes()),
                    "version" => section[4..6].copy_from_slice(&0x010au16.to_le_bytes()),
                    _ => unreachable!(),
                }
            }
            builder = builder.section(kind, section);
        }
        let bad = dir.join(format!("bad-{corruption}.bfasl"));
        fs::write(&bad, builder.build()).unwrap();
        let loaded = probe(&bad);
        let stdout = String::from_utf8_lossy(&loaded.stdout);
        assert!(
            loaded.status.success(),
            "{corruption}: {stdout}\n{}",
            String::from_utf8_lossy(&loaded.stderr)
        );
        assert!(
            stdout.contains("CAUGHT") && stdout.contains("STATE 0 NIL NIL NIL"),
            "{corruption}: {stdout}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

/// WITH-OPEN-FILE must not force a parser's entire body back to source eval.
#[test]
fn with_open_file_bfasl_keeps_cleanup_values_and_declarations() {
    let dir = workdir("with-open-file");
    let src = dir.join("reader.lisp");
    let out = dir.join("reader.bfasl");
    let data = dir.join("data.txt");
    fs::write(&data, "hello\n").unwrap();
    fs::write(
        &src,
        r#"
      (defun read-special-stream () (declare (special file-stream-var))
        (read-line file-stream-var))
      (defun declared-file-read (path)
        (with-open-file (file-stream-var path)
          (declare (special file-stream-var))
          (read-special-stream)))
      (defun compiled-file-read (path mode)
        (with-open-file (s path :if-does-not-exist nil)
          (if s
              (case mode
                (:normal (values (read-line s) 42))
                (:zero (values))
                (:escape (throw 'file-done 77))
                (:error (error "file body")))
              :missing)))
    "#,
    )
    .unwrap();
    let compiled = run(&format!("(compile-file {:?} :output-file {:?})", src, out));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "must not embed source fallback"
    );
    assert!(
        bbu_counts(&bytes).1 >= 3,
        "must contain the compiled readers"
    );
    fs::remove_file(&src).unwrap();
    let loaded = run(&format!(
        r#"
      (load {out:?})
      (setq *file-closed* nil *file-aborts* nil)
      (defmethod close :before ((s t) &key abort)
        (setq *file-closed* s)
        (push abort *file-aborts*))
      (format t "READ ~S~%" (multiple-value-list (compiled-file-read {data:?} :normal)))
      (format t "CLOSED ~S~%" (open-stream-p *file-closed*))
      (format t "ZERO ~S~%" (multiple-value-list (compiled-file-read {data:?} :zero)))
      (format t "ESCAPE ~S~%" (catch 'file-done (compiled-file-read {data:?} :escape)))
      (format t "ERROR ~S~%" (handler-case (compiled-file-read {data:?} :error) (error () :caught)))
      (format t "MISSING ~S~%" (compiled-file-read {missing:?} :normal))
      (format t "SPECIAL ~S~%" (declared-file-read {data:?}))
      (format t "ABORTS ~S~%" (reverse *file-aborts*))
    "#,
        missing = dir.join("missing.txt")
    ));
    let stdout = String::from_utf8_lossy(&loaded.stdout);
    assert!(
        loaded.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    for expected in [
        "READ (\"hello\" 42)",
        "CLOSED NIL",
        "ZERO NIL",
        "ESCAPE 77",
        "ERROR :CAUGHT",
        "MISSING :MISSING",
        "SPECIAL \"hello\"",
        "ABORTS (NIL NIL T T NIL)",
    ] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
    let stress = Command::new(BIN)
        .args([
            "--no-init",
            "--no-bootstrap",
            "--eval",
            &format!(
                "(load {out:?}) (format t \"STRESS ~S~%\" (multiple-value-list (compiled-file-read {data:?} :normal)))"
            ),
        ])
        .env("EGCL_GC_STRESS", "1")
        .env("EGCL_GC_POISON", "1")
        .env("EGCL_GC_VERIFY", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&stress.stdout);
    assert!(
        stress.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&stress.stderr)
    );
    assert!(stdout.contains("STRESS (\"hello\" 42)"), "{stdout}");
    fs::remove_dir_all(dir).unwrap();
}

/// `handler-case` (and therefore `ignore-errors`) must serialize to a source-free
/// BBU — its clause tables (type name, clause body PC, var slot) round-trip in an
/// auxiliary table — and dispatch correctly from a fresh process.
#[test]
fn handler_case_bfasl_round_trip() {
    let dir = workdir("handler-case");
    let src = dir.join("h.lisp");
    let out = dir.join("h.bfasl");
    fs::write(
        &src,
        "(defun safe-div (x y)\n\
        \x20 (handler-case (/ x y) (division-by-zero () :div-zero) (error (c) (list :err c))))\n\
         (defun ie (x) (ignore-errors (/ 10 x)))\n\
         (defun guard (x) (handler-case (if (< x 0) (error \"neg\") (* x 2)) (error () -1)))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(bfasl_section(&fs::read(&out).unwrap(), 11).is_none());

    let l = run(&format!(
        "(progn (load \"{}\") \
           (list (safe-div 10 2) (safe-div 10 0) (ie 5) (ie 0) (guard 5) (guard -3)))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "(5 :DIV-ZERO 2 NIL 10 -1)",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// bliss-9u6d: a nested closure (lambda / flet) that captures an enclosing
/// frame-slot local only through a macro expansion inside the closure body must
/// not miscompile in a portable `.bfasl`. The macro-blind capture pre-scan never
/// boxes the local, so a naively-compiled closure resolves it as a global and the
/// loaded fasl raises `unbound variable`. The compiler must instead bail such a
/// definition to the source fallback (it loads and runs on the tree-walker),
/// giving the correct captured value after a round trip.
#[test]
fn macro_hidden_closure_capture_bfasl_round_trip() {
    let dir = workdir("macro-hidden-capture");
    let src = dir.join("mh.lisp");
    let out = dir.join("mh.bfasl");
    fs::write(
        &src,
        "(defvar *o* :untouched)\n\
         (defmacro gx () 'x)\n\
         (defun via-lambda () (let ((x 42)) (funcall (lambda () (setf *o* (gx)))) *o*))\n\
         (defun via-flet () (let ((x 7)) (flet ((g () (setf *o* (gx)))) (g)) *o*))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );

    let l = run(&format!(
        "(progn (load \"{}\") (list (via-lambda) (via-flet)))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed (macro-hidden capture must not raise unbound-variable): {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "(42 7)");

    let _ = fs::remove_dir_all(&dir);
}

/// A user `(defun (setf f) …)` writer and its use `(setf (f …) v)` must both
/// lower to source-free bytecode (the writer installed under a canonical symbol,
/// the use dispatched through it), and `setf` of composed `c[ad]+r` places must
/// work. This is the shape of alexandria's `(setf lastcar)`.
#[test]
fn setf_function_and_cadr_places_bfasl_round_trip() {
    let dir = workdir("setf-fn");
    let src = dir.join("s.lisp");
    let out = dir.join("s.bfasl");
    fs::write(
        &src,
        "(defun (setf my2) (val list) (setf (cadr list) val) val)\n\
         (defun use-writer (l) (setf (my2 l) 99) l)\n\
         (defun set-caddr (l) (setf (caddr l) 7) l)\n\
         (defun set-cddr (l) (setf (cddr l) '(x)) l)\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(bfasl_section(&fs::read(&out).unwrap(), 11).is_none());

    let l = run(&format!(
        "(progn (load \"{}\") \
           (list (use-writer (list 1 2 3)) (set-caddr (list 1 2 3)) (set-cddr (list 1 2 3))))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "((1 99 3) (1 2 7) (1 2 X))",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// The extended LOOP grammar (`with`, `for … {in|on} … by`, conditional
/// `collect … and collect`, `finally (return …)`, numeric `from … below`) and
/// `setf` of a `cdr` place must all lower to source-free bytecode. This is the
/// shape of alexandria's plist LOOP functions (remove/delete-from-plist).
#[test]
fn extended_loop_and_setf_place_bfasl_round_trip() {
    let dir = workdir("loop-setf");
    let src = dir.join("l.lisp");
    let out = dir.join("l.bfasl");
    fs::write(
        &src,
        "(defun rmplist (plist &rest keys)\n\
        \x20 (loop for (k . rest) on plist by #'cddr\n\
        \x20       unless (member k keys :test #'eq)\n\
        \x20       collect k and collect (first rest)))\n\
         (defun delplist (plist &rest keys)\n\
        \x20 (loop with head = plist with tail = nil\n\
        \x20       for (k . rest) on plist by #'cddr\n\
        \x20       do (if (member k keys :test #'eq)\n\
        \x20              (let ((next (cdr rest)))\n\
        \x20                (if tail (setf (cdr tail) next) (setf head next)))\n\
        \x20              (setf tail rest))\n\
        \x20       finally (return head)))\n\
         (defun squares (n) (loop for i from 0 below n collect (* i i)))\n\
         (defun total (l) (loop for x in l sum x))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(bfasl_section(&fs::read(&out).unwrap(), 11).is_none());

    let l = run(&format!(
        "(progn (load \"{}\") \
           (list (rmplist '(:a 1 :b 2 :c 3) :b) \
                 (delplist (list :a 1 :b 2 :c 3) :b) \
                 (squares 4) (total '(1 2 3 4))))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "((:A 1 :C 3) (:A 1 :C 3) (0 1 4 9) 10)",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// R4.24 / R4.38: portable quasiquote allocation must not prevent either T1
/// invocation promotion or OSR of a one-shot loop loaded from a FASL.
#[test]
fn portable_quasiquote_runs_natively_after_fasl_load() {
    let dir = workdir("native-quasiquote");
    let src = dir.join("quasiquote.lisp");
    let out = dir.join("quasiquote.bfasl");
    fs::write(
        &src,
        "(defun qq-loop (n) (let ((result nil)) \
          (dotimes (i n result) (setq result `(,i . ,result)))))",
    )
    .unwrap();
    let compiled = Command::new(BIN)
        .args(["--no-bootstrap", "--no-init", "--eval"])
        .arg(format!(
            "(compile-file \"{}\" :output-file \"{}\")",
            src.display(),
            out.display()
        ))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    fs::remove_file(src).unwrap();
    for (threshold, expected_tier, expected_osr) in [("1", 1, false), ("100000", 0, true)] {
        let loaded = Command::new(BIN)
            .args(["--no-bootstrap", "--no-init", "--eval"])
            .arg(format!(
                "(progn (load \"{}\") (let ((v (qq-loop 100))) \
                 (format t \"NATIVE-QQ ~S~%\" \
                  (list (length v) (car v) (nth 99 v) \
                   (egcl-ext:function-tier 'qq-loop) \
                   (> (egcl-ext:function-osr-count 'qq-loop) 0)))))",
                out.display()
            ))
            .env("EGCL_T0_T1_THRESHOLD", threshold)
            .env("EGCL_OSR_THRESHOLD", "20")
            .env("EGCL_DISABLE_T2", "1")
            .env("EGCL_GC_STRESS", "7")
            .env("EGCL_GC_POISON", "1")
            .output()
            .unwrap();
        assert!(
            loaded.status.success(),
            "{}",
            String::from_utf8_lossy(&loaded.stderr)
        );
        let expected = format!(
            "NATIVE-QQ (100 99 0 {expected_tier} {})",
            if expected_osr { "T" } else { "NIL" }
        );
        assert!(
            String::from_utf8_lossy(&loaded.stdout).contains(&expected),
            "threshold {threshold}: {}",
            String::from_utf8_lossy(&loaded.stdout)
        );
    }
    let _ = fs::remove_dir_all(dir);
}

/// A nested function's literal pool must stay rooted while MAKE-CLOSURE
/// allocates its installed lambda list, before registry installation.
#[test]
fn closure_literals_survive_gc_during_fasl_closure_construction() {
    let dir = workdir("closure-literal-roots");
    let src = dir.join("closure.lisp");
    let out = dir.join("closure.bfasl");
    fs::write(
        &src,
        r#"(defun literal-factory ()
              (lambda (a b) (list '(kept (nested datum)) a b)))
            (format t "CLOSURE-CHECK ~S~%" (funcall (literal-factory) 1 2))"#,
    )
    .unwrap();
    let compiled = Command::new(BIN)
        .args(["--no-bootstrap", "--no-init", "--eval"])
        .arg(format!(
            "(compile-file \"{}\" :output-file \"{}\")",
            src.display(),
            out.display()
        ))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    // Every-allocation stress can promote the literal before closure creation
    // and miss the bug. Strides exercise it while still in the nursery.
    for stride in ["7", "13", "31"] {
        let loaded = Command::new(BIN)
            .args(["--no-bootstrap", "--no-init", "--load"])
            .arg(&out)
            .env("EGCL_GC_STRESS", stride)
            .env("EGCL_GC_POISON", "1")
            .output()
            .unwrap();
        assert!(
            loaded.status.success(),
            "stride {stride}: {}",
            String::from_utf8_lossy(&loaded.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&loaded.stdout).trim(),
            "CLOSURE-CHECK ((KEPT (NESTED DATUM)) 1 2)",
            "stride {stride}"
        );
    }
    let _ = fs::remove_dir_all(dir);
}

/// R5.06 / R6.71: numeric predicates on runtime-created heap numbers retain
/// exact signs through source-free native calls and moving collections.
#[test]
fn numeric_predicates_load_without_source_under_gc_stress() {
    let dir = workdir("numeric-predicates");
    let src = dir.join("predicates.lisp");
    let out = dir.join("predicates.bfasl");
    fs::write(
        &src,
        r#"
      (defun predicates (x) (list (zerop x) (plusp x) (minusp x)))
      (defun check-predicates ()
        (let* ((big (ash 1 2000)) (tiny (/ 1 big)))
          (list (predicates 0) (predicates big) (predicates (- big))
                (predicates tiny) (predicates (- tiny))
                (zerop (complex 0.0 -0.0)) (zerop (complex 0 tiny)))))
      (format t "PRED-FASL ~S~%" (check-predicates))
    "#,
    )
    .unwrap();
    let compiled = run(&format!("(compile-file {:?} :output-file {:?})", src, out));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "must not embed legacy source"
    );
    let bbu = bfasl_section(&bytes, 12).expect("compiled bytecode unit");
    let start = bbu_action_start(bbu);
    let (_, _, count) = bbu_counts(&bytes);
    for action in bbu[start..start + count as usize * 14].chunks_exact(14) {
        assert_ne!(action[0], 9, "numeric predicates fell back to EvalSource");
    }
    fs::remove_file(&src).unwrap();
    for stride in ["1", "7", "31"] {
        let loaded = Command::new(BIN)
            .args(["--no-init", "--no-bootstrap", "--load"])
            .arg(&out)
            .env("EGCL_FORCE_TIER", "t1")
            .env("EGCL_GC_STRESS", stride)
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_VERIFY", "1")
            .output()
            .unwrap();
        assert!(
            loaded.status.success(),
            "stride {stride}: {}",
            String::from_utf8_lossy(&loaded.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&loaded.stdout).trim(),
            "PRED-FASL ((T NIL NIL) (NIL T NIL) (NIL NIL T) (NIL T NIL) (NIL NIL T) T NIL)",
            "stride {stride}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

/// R6.71/R6.75: heap numeric literals must not turn a compiled unit back into
/// source. Nested constants and exact ratios survive a fresh moving-GC load.
#[test]
fn numeric_literals_load_without_source_under_gc_stress() {
    let dir = workdir("numeric-literals");
    let src = dir.join("literals.lisp");
    let out = dir.join("literals.bfasl");
    fs::write(
        &src,
        r#"
      (defun numeric-literals ()
        '(1.0000000000000002d0 -0.0d0
          #(1267650600228229401496703205377 -1267650600228229401496703205377)
          1/1267650600228229401496703205376))
      (let ((v (numeric-literals)))
        (format t "LITERALS ~S~%"
          (list (= (first v) 1.0000000000000002d0)
                (> (first v) 1.0d0)
                (= (second v) -0.0d0)
                (= (aref (third v) 0) (+ (ash 1 100) 1))
                (= (aref (third v) 1) (- (+ (ash 1 100) 1)))
                (= (car (cdr (cdr (cdr v)))) (/ 1 (ash 1 100))))))
    "#,
    )
    .unwrap();
    let compiled = run(&format!("(compile-file {:?} :output-file {:?})", src, out));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "must not embed legacy source"
    );
    let bbu = bfasl_section(&bytes, 12).expect("numeric literals require bytecode");
    let start = bbu_action_start(bbu);
    let (_, _, count) = bbu_counts(&bytes);
    for action in bbu[start..start + count as usize * 14].chunks_exact(14) {
        assert_ne!(action[0], 9, "numeric literals fell back to EvalSource");
    }
    fs::remove_file(&src).unwrap();
    for stride in ["1", "7", "31"] {
        let loaded = Command::new(BIN)
            .args(["--no-init", "--no-bootstrap", "--load"])
            .arg(&out)
            .env("EGCL_FORCE_TIER", "t1")
            .env("EGCL_GC_STRESS", stride)
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_VERIFY", "1")
            .output()
            .unwrap();
        assert!(
            loaded.status.success(),
            "stride {stride}: {}",
            String::from_utf8_lossy(&loaded.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&loaded.stdout).trim(),
            "LITERALS (T T T T T T)",
            "stride {stride}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

/// R5.06 / R5.131 / R6.71: direct value and sequence calls preserve relocated
/// heap arguments and multiple values without requiring their source file.
#[test]
fn value_bridges_load_without_source_under_gc_stress() {
    let dir = workdir("value-bridges");
    let src = dir.join("bridges.lisp");
    let out = dir.join("bridges.bfasl");
    fs::write(
        &src,
        r#"
      (defun bridge (xs)
        (multiple-value-call #'list (values-list (reverse xs))
          (values (endp xs) (< 1 2 3) (>= 3 2 1))))
      (defun check-bridge ()
        (bridge (list (reverse "ahpla") (reverse "ateb") (reverse "ammag"))))
      (format t "BRIDGE ~S~%" (check-bridge))
    "#,
    )
    .unwrap();
    let compiled = run(&format!("(compile-file {:?} :output-file {:?})", src, out));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "must not embed legacy source"
    );
    let bbu = bfasl_section(&bytes, 12).expect("compiled bytecode unit");
    let start = bbu_action_start(bbu);
    let (_, _, count) = bbu_counts(&bytes);
    for action in bbu[start..start + count as usize * 14].chunks_exact(14) {
        assert_ne!(action[0], 9, "value bridge fell back to EvalSource");
    }
    fs::remove_file(&src).unwrap();
    for stride in ["1", "7", "31"] {
        let loaded = Command::new(BIN)
            .args(["--no-init", "--no-bootstrap", "--load"])
            .arg(&out)
            .env("EGCL_FORCE_TIER", "t1")
            .env("EGCL_GC_STRESS", stride)
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_VERIFY", "1")
            .output()
            .unwrap();
        assert!(
            loaded.status.success(),
            "stride {stride}: {}",
            String::from_utf8_lossy(&loaded.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&loaded.stdout).trim(),
            "BRIDGE (\"gamma\" \"beta\" \"alpha\" NIL T T)",
            "stride {stride}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sequence_clos_bridges_load_without_source_under_gc_stress() {
    let dir = workdir("sequence-clos-bridges");
    let src = dir.join("bridges.lisp");
    let out = dir.join("bridges.bfasl");
    fs::write(
        &src,
        r#"
      (defclass bridge-base () ((item :initarg :item)))
      (defclass bridge-child (bridge-base) ())
      (defmethod bridge-next ((x bridge-base) n)
        (values (slot-value x 'item) n))
      (defmethod bridge-next ((x bridge-child) n) (call-next-method))
      (defmethod bridge-next :around ((x bridge-child) n)
        (call-next-method x (+ n 1)))
      (defun bridge-store (x value) (setf (slot-value x 'item) value))
      (defun bridge-join (head tail) (append head tail))
      (defun bridge-convert (x type) (coerce x type))
      (defun bridge-slice (x) (subseq x 1 4))
      (let* ((x (make-instance 'bridge-child :item nil))
             (head (list (reverse "ahpla")))
             (tail (list (reverse "ateb")))
             (joined (bridge-join head tail)))
        (format t "SEQUENCE-CLOS ~S~%"
          (list joined (eq tail (cdr joined)) (not (eq head joined))
            (bridge-convert '(#\g #\a #\m #\m #\a) 'string)
            (bridge-slice (reverse "abcde"))
            (bridge-store x (list (reverse "atled")))
            (multiple-value-list (bridge-next x 4)))))
    "#,
    )
    .unwrap();
    let compiled = run(&format!("(compile-file {:?} :output-file {:?})", src, out));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "must not embed legacy source"
    );
    assert!(
        bbu_counts(&bytes).1 >= 4,
        "must contain compiled bridge functions"
    );
    fs::remove_file(&src).unwrap();
    for stride in ["0", "1", "7", "31"] {
        let loaded = Command::new(BIN)
            .args(["--no-init", "--no-bootstrap", "--load"])
            .arg(&out)
            .env("EGCL_FORCE_TIER", "t1")
            .env("EGCL_T1_THRESHOLD", "1")
            .env("EGCL_GC_STRESS", stride)
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_VERIFY", "1")
            .output()
            .unwrap();
        assert!(
            loaded.status.success(),
            "stride {stride}: {}",
            String::from_utf8_lossy(&loaded.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&loaded.stdout).trim(),
            "SEQUENCE-CLOS ((\"alpha\" \"beta\") T T \"gamma\" \"dcb\" (\"delta\") ((\"delta\") 5))",
            "stride {stride}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

/// R5.06 / R6.71: source-free ASH calls use the same allocating integer kernel
/// in compiled code, with operands kept live across bignum result allocation.
#[test]
fn integer_shifts_load_without_source_under_gc_stress() {
    let dir = workdir("integer-shifts");
    let src = dir.join("shifts.lisp");
    let out = dir.join("shifts.bfasl");
    fs::write(
        &src,
        r#"
      (defun shift-pair (n count)
        (let ((big (ash n count)))
          (list big (ash big (- count)) (ash (- big 1) (- count)))))
      (format t "ASH-FASL ~S~%" (list (shift-pair 1 130) (shift-pair -1 130)))
    "#,
    )
    .unwrap();
    let compiled = run(&format!("(compile-file {:?} :output-file {:?})", src, out));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "must not embed legacy source"
    );
    let bbu = bfasl_section(&bytes, 12).expect("compiled bytecode unit");
    let start = bbu_action_start(bbu);
    let (_, _, count) = bbu_counts(&bytes);
    for action in bbu[start..start + count as usize * 14].chunks_exact(14) {
        assert_ne!(action[0], 9, "integer shifts fell back to EvalSource");
    }
    fs::remove_file(&src).unwrap();
    for stride in ["1", "7", "31"] {
        let loaded = Command::new(BIN)
            .args(["--no-init", "--no-bootstrap", "--load"])
            .arg(&out)
            .env("EGCL_FORCE_TIER", "t1")
            .env("EGCL_GC_STRESS", stride)
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_VERIFY", "1")
            .output()
            .unwrap();
        assert!(
            loaded.status.success(),
            "stride {stride}: {}",
            String::from_utf8_lossy(&loaded.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&loaded.stdout).trim(),
            "ASH-FASL ((1361129467683753853853498429727072845824 1 0) (-1361129467683753853853498429727072845824 -1 -2))",
            "stride {stride}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

/// R4.23 / R6.71: Babel builds reverse encoding tables with capturing ACROSS
/// loops and typed numeric drivers. These must be compiled load thunks, not
/// EvalSource actions that rebuild the tables in the tree walker.
#[test]
fn encoding_table_loops_load_without_source_fallback() {
    let dir = workdir("encoding-table-loops");
    let src = dir.join("tables.lisp");
    let out = dir.join("tables.bfasl");
    fs::write(
        &src,
        r#"
        (defparameter *reverse-table*
          (let ((h (make-hash-table)))
            (flet ((flip (codes start)
                     (loop with row = start with col = 1
                           for code across codes
                           do (unless (= code 0)
                                (setf (gethash code h) (+ (* row 16) col)))
                              (incf col)
                              (when (= col 4) (incf row) (setf col 1)))))
              (flip #(10 0 12 13 14) 2)
              h)))
        (defparameter *typed-table*
          (let ((h (make-hash-table)))
            (loop for row of-type (unsigned-byte 8) from 1 to 3
                  do (loop for col of-type fixnum from 1 to 2
                           for code of-type integer = (+ (* row 10) col)
                           unless (= code 22)
                             do (setf (gethash code h) (+ row col))))
            h))
        (defun scan-vector (v)
          (loop for x across v collect x))
        (defun scan-once ()
          (let ((calls 0))
            (list (loop for x across (progn (incf calls) #(3 4)) sum x)
                  calls)))
        (defun scan-mutable ()
          (let ((v (vector 1 2 3)))
            (loop for x across v
                  do (when (= x 1) (setf (aref v 1) 9))
                  collect x)))
        (defun scan-drivers ()
          (list (loop for x of-type fixnum across #(1 2 3)
                      for i of-type fixnum from 1 sum (* x i))
                (loop for x across #(1 2 3) until (= x 2) collect x)
                (loop for x across #(1 2 3) for y across #(4)
                      collect (+ x y))))
    "#,
    )
    .unwrap();
    let compiled = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    let bbu = bfasl_section(&bytes, 12).expect("compiled bytecode unit");
    let start = bbu_action_start(bbu);
    let (_, _, count) = bbu_counts(&bytes);
    for action in bbu[start..start + count as usize * 14].chunks_exact(14) {
        assert_ne!(action[0], 9, "encoding table loop fell back to EvalSource");
    }
    fs::remove_file(&src).unwrap();
    let loaded = run(&format!(
        r#"(progn (load "{}")
        (list (hash-table-count *reverse-table*)
              (gethash 10 *reverse-table*) (gethash 13 *reverse-table*)
              (hash-table-count *typed-table*) (gethash 32 *typed-table*)
              (scan-vector #()) (scan-vector "az") (scan-vector #*101)
              (scan-vector (make-array 4 :initial-contents '(7 8 9 10) :fill-pointer 2))
              (scan-once) (scan-mutable) (scan-drivers)
              (handler-case (scan-vector '(1 2)) (type-error () :bad-vector))))"#,
        out.display()
    ));
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&loaded.stdout).trim(),
        "(4 33 49 5 5 NIL (#\\a #\\z) (1 0 1) (7 8) (7 1) (1 9 3) (14 (1) (5)) :BAD-VECTOR)"
    );
    let _ = fs::remove_dir_all(dir);
}

/// The LOOP conditional-execution grammar (`when TEST do …`, `when TEST return
/// …`, and `when TEST … else …`) must lower to source-free bytecode. This is the
/// shape of alexandria's `ends-with-subseq` / `map-derangements`.
#[test]
fn loop_conditional_selectable_clauses_bfasl_round_trip() {
    let dir = workdir("loop-when");
    let src = dir.join("w.lisp");
    let out = dir.join("w.bfasl");
    fs::write(
        &src,
        "(defun first-big (n) (loop for i from 0 below n when (> i 3) do (return-from first-big i) finally (return -1)))\n\
         (defun tag-parity (n) (loop for i from 0 below n when (evenp i) collect (list :e i) else collect (list :o i)))\n\
         (defun only-when (n) (loop for i from 0 below n when (oddp i) collect i))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(bfasl_section(&fs::read(&out).unwrap(), 11).is_none());

    let l = run(&format!(
        "(progn (load \"{}\") (list (first-big 10) (tag-parity 4) (only-when 6)))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "(4 ((:E 0) (:O 1) (:E 2) (:O 3)) (1 3 5))",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `setf` of a bit-array element (`bit`/`sbit`) must lower to source-free
/// bytecode (via the shared array-element store) and match the tree-walker.
/// Bit-array setf is alexandria's `map-derangements` mask update.
#[test]
fn bit_array_setf_bfasl_round_trip() {
    let dir = workdir("bit-setf");
    let src = dir.join("b.lisp");
    let out = dir.join("b.bfasl");
    fs::write(
        &src,
        "(defun mask3 ()\n\
        \x20 (let ((m (make-array 4 :element-type 'bit :initial-element 0)))\n\
        \x20   (setf (bit m 1) 1)\n\
        \x20   (setf (sbit m 3) 1)\n\
        \x20   (list (bit m 0) (bit m 1) (bit m 2) (bit m 3))))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );

    let l = run(&format!("(progn (load \"{}\") (mask3))", out.display()));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "(0 1 0 1)");

    let _ = fs::remove_dir_all(&dir);
}

/// A `(setf (place …) …)` whose `(defun (setf place) …)` writer lives in a
/// *separate* unit loaded from `.bfasl` (installed on the mangled writer symbol,
/// not `GLOBAL_SETF_FNS`) must still lower to the portable writer call. This is
/// alexandria's `(setf (lastcar …) …)` in `sequences.lisp` using the writer from
/// `lists.lisp`.
#[test]
fn cross_unit_setf_writer_bfasl_round_trip() {
    let dir = workdir("xunit-setf");
    let wsrc = dir.join("writer.lisp");
    let wout = dir.join("writer.bfasl");
    let usrc = dir.join("user.lisp");
    let uout = dir.join("user.bfasl");
    fs::write(
        &wsrc,
        "(defun (setf second-of) (val list) (setf (cadr list) val) val)\n",
    )
    .unwrap();
    fs::write(&usrc, "(defun poke (l) (setf (second-of l) 42) l)\n").unwrap();

    // Compile the writer, load it (installs on the mangled symbol), THEN compile
    // the user unit — its SETF lowering must recognise the bfasl-loaded writer.
    let c = run(&format!(
        "(progn (compile-file \"{}\" \"{}\") (load \"{}\") (compile-file \"{}\" \"{}\"))",
        wsrc.display(),
        wout.display(),
        wout.display(),
        usrc.display(),
        uout.display(),
    ));
    assert!(
        c.status.success(),
        "compile/load/compile failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(bfasl_section(&fs::read(&uout).unwrap(), 11).is_none());

    let l = run(&format!(
        "(progn (load \"{}\") (load \"{}\") (poke (list 1 2 3)))",
        wout.display(),
        uout.display(),
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "(1 42 3)");

    let _ = fs::remove_dir_all(&dir);
}

/// `(setf (symbol-function s) v)` / `(setf (fdefinition s) v)` must install a
/// value that is actually callable by the name `s` — for every function
/// designator, not just an existing `(symbol-function q)`. The FUNCTION form
/// yields a bare symbol for `#'q` and a closure cons for `(lambda …)`, neither
/// an interpreted-function object, so a naive verbatim store left `s`
/// "undefined"/empty (bliss-57m). Round-trips through `.bfasl`: a bfasl-loaded
/// function is callable only under its registered name, so an aliased name must
/// dispatch through the resolved object's own name. This is alexandria's
/// `sequences.lisp` (`(setf (symbol-function 'emptyp) (symbol-function
/// 'sequence:emptyp))`).
#[test]
fn setf_symbol_function_install_bfasl_round_trip() {
    let dir = workdir("setf-symfn");
    let src = dir.join("s.lisp");
    let out = dir.join("s.bfasl");
    fs::write(
        &src,
        // A source lambda installed via SETF SYMBOL-FUNCTION, a `#'name` alias,
        // and a `(symbol-function name)` alias — all called by their new names,
        // directly and through funcall/apply/mapcar.
        "(defun base-sum (a b) (+ a b))\n\
         (setf (symbol-function 'inst-lambda) (lambda (a b) (* a b)))\n\
         (setf (symbol-function 'alias-sharp) #'base-sum)\n\
         (setf (fdefinition 'alias-fdef) (symbol-function 'base-sum))\n\
         (defun use-all (x y)\n\
           (list (inst-lambda x y)\n\
                 (alias-sharp x y)\n\
                 (funcall #'alias-fdef x y)\n\
                 (apply #'inst-lambda (list x y))\n\
                 (mapcar #'alias-sharp (list x) (list y))))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    // Fully lowered: no source-text fallback section.
    assert!(bfasl_section(&fs::read(&out).unwrap(), 11).is_none());

    let l = run(&format!(
        "(progn (load \"{}\") (use-all 3 4))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    // (* 3 4)=12, (+ 3 4)=7, (+ 3 4)=7, (* 3 4)=12, (mapcar #'+ '(3) '(4))=(7)
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "(12 7 7 12 (7))",);

    let _ = fs::remove_dir_all(&dir);
}

/// A CAPTURING closure installed into a global function cell — both a top-level
/// `(let … (lambda …))` and the value returned by a factory `defun` — must round-
/// trip source-free and, when called by its installed name, reach the captured
/// lexicals. The by-name call dispatches through the closure object's own
/// registered name, and its body runs against the captured environment rather
/// than the caller frame (bliss-jtc.23.3).
#[test]
fn setf_symbol_function_capturing_closure_bfasl_round_trip() {
    let dir = workdir("setf-capfn");
    let src = dir.join("c.lisp");
    let out = dir.join("c.bfasl");
    fs::write(
        &src,
        "(setf (symbol-function 'adder5) (let ((n 5)) (lambda (x) (+ x n))))\n\
         (defun mk-mul (k) (lambda (x) (* x k)))\n\
         (setf (symbol-function 'mul3) (mk-mul 3))\n\
         (setf (symbol-function 'ctr) (let ((c 0)) (lambda () (setf c (+ c 1)) c)))\n\
         (defun use-all ()\n\
           (list (adder5 10) (mul3 10) (ctr) (ctr) (funcall #'adder5 20)))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(bfasl_section(&fs::read(&out).unwrap(), 11).is_none());

    let l = run(&format!("(progn (load \"{}\") (use-all))", out.display()));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    // (+ 10 5)=15, (* 10 3)=30, ctr=1, ctr=2, (+ 20 5)=25
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "(15 30 1 2 25)",);

    let _ = fs::remove_dir_all(&dir);
}

/// A function whose name is an EXPORTED symbol of a package defined in a
/// *separately loaded* `.bfasl` must install on the package's canonical symbol,
/// so it is callable by the package-qualified name in a fresh process. The BBU
/// materialized an external symbol constant under a raw `"PKG:NAME"` (single
/// colon) rt key, diverging from the `"PKG::NAME"` key the package system /
/// reader use, so the function installed on a duplicate symbol and was invisible
/// (bliss-q6f). This is the shape of every exported alexandria function loaded
/// across its per-file `.bfasl`s.
#[test]
fn cross_unit_exported_function_bfasl_round_trip() {
    let dir = workdir("xunit-export");
    let psrc = dir.join("pkg.lisp");
    let pout = dir.join("pkg.bfasl");
    let usrc = dir.join("use.lisp");
    let uout = dir.join("use.bfasl");
    // The package (with its EXPORT) lives in one unit; the exported function's
    // definition in another — the units are separate `.bfasl`s.
    fs::write(
        &psrc,
        "(defpackage :xp (:use :cl) (:export #:add1 #:twice))\n",
    )
    .unwrap();
    fs::write(
        &usrc,
        "(in-package :xp)\n\
         (defun add1 (x) (+ x 1))\n\
         (defun twice (x) (* x 2))\n",
    )
    .unwrap();

    // Compile pkg, load it (creates + exports the symbols), THEN compile use so
    // its defun names are the external symbols of the already-defined package.
    let c = run(&format!(
        "(progn (compile-file \"{}\" \"{}\") (load \"{}\") (compile-file \"{}\" \"{}\"))",
        psrc.display(),
        pout.display(),
        pout.display(),
        usrc.display(),
        uout.display(),
    ));
    assert!(
        c.status.success(),
        "compile/load/compile failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(bfasl_section(&fs::read(&uout).unwrap(), 11).is_none());

    // Fresh process: load both units, then call the exported functions. Resolve
    // the names with FIND-SYMBOL at run time so the program text carries no
    // package-qualified literal to read before XP exists (the loads create it).
    let l = run(&format!(
        "(progn (load \"{}\") (load \"{}\") \
           (list (funcall (find-symbol \"ADD1\" \"XP\") 41) \
                 (funcall (find-symbol \"TWICE\" \"XP\") 21) \
                 (if (fboundp (find-symbol \"ADD1\" \"XP\")) t nil)))",
        pout.display(),
        uout.display(),
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "(42 42 T)");

    let _ = fs::remove_dir_all(&dir);
}

/// A local `macrolet` whose macro introduces references to enclosing lexicals
/// (invisible in the unexpanded source) must be expanded before capture
/// analysis, so the captured variables are boxed and the capturing `labels`
/// closures lower to source-free bytecode. This is alexandria's
/// `gaussian-random` shape.
#[test]
fn macrolet_in_body_with_capture_bfasl_round_trip() {
    let dir = workdir("macrolet-cap");
    let src = dir.join("m.lisp");
    let out = dir.join("m.bfasl");
    fs::write(
        &src,
        "(defun clamp-sum (lo hi)\n\
        \x20 (macrolet ((ok (x) `(<= lo ,x hi)))\n\
        \x20   (labels ((gen () (+ lo hi))\n\
        \x20            (pick (x) (if (ok x) x (gen))))\n\
        \x20     (list (pick 0) (pick lo) (gen)))))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(bfasl_section(&fs::read(&out).unwrap(), 11).is_none());

    let l = run(&format!(
        "(progn (load \"{}\") (clamp-sum 2 5))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    // (pick 0): 0 not in [2,5] -> (gen)=7; (pick 2): in range -> 2; (gen)=7.
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "(7 2 7)");

    let _ = fs::remove_dir_all(&dir);
}

/// The LOOP `for VAR = INIT then STEP` stepping clause and ratio constants
/// (`1/2`) must lower to source-free bytecode. This is alexandria's `iota` /
/// `median` shape.
#[test]
fn loop_for_then_and_ratio_constant_bfasl_round_trip() {
    let dir = workdir("for-then-ratio");
    let src = dir.join("n.lisp");
    let out = dir.join("n.bfasl");
    fs::write(
        &src,
        "(defun steps (n) (loop for i = 10 then (+ i 5) repeat n collect i))\n\
         (defun halves (n) (loop for i from 1 to n collect (* 1/2 i)))\n\
         (defun a-third () 1/3)\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );

    let l = run(&format!(
        "(progn (load \"{}\") (list (steps 4) (halves 4) (a-third)))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    // steps: 10,15,20,25. halves: 1/2,1,3/2,2. a-third: 1/3.
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "((10 15 20 25) (1/2 1 3/2 2) 1/3)",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `destructuring-bind` must lower to source-free portable bytecode (required,
/// `&optional` with defaults, and `&rest`) and execute correctly from a fresh
/// process. This is a prerequisite for compiling real macros (e.g. alexandria's
/// once-only) without retaining source.
#[test]
fn destructuring_bind_bfasl_round_trips() {
    let dir = workdir("dbind-bfasl");
    let src = dir.join("d.lisp");
    let out = dir.join("d.bfasl");
    fs::write(
        &src,
        "(defun db-req (s) (destructuring-bind (a b) s (list b a)))\n\
         (defun db-opt (s) (destructuring-bind (a &optional (b 99)) s (list a b)))\n\
         (defun db-rest (s) (destructuring-bind (a &rest r) s (list a r)))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(c.status.success(), "compile-file failed");
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "destructuring-bind must compile to a complete source-free BBU"
    );

    let l = run(&format!(
        "(progn (load \"{}\") \
           (list (db-req '(1 2)) (db-opt '(1)) (db-opt '(1 2)) (db-rest '(1 2 3))))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "((2 1) (1 99) (1 2) (1 (2 3)))",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A macro whose expander uses nested quasiquote (`` `` `` with `,,g` and
/// `,,@x`) must compile to source-free bytecode and behave identically to the
/// tree-walker. This uses alexandria's exact ONCE-ONLY (the macro that
/// motivated the nested-quasiquote and destructuring-bind lowering); its
/// single-evaluation contract is the observable check.
#[test]
fn nested_quasiquote_macro_bfasl_round_trips() {
    let dir = workdir("nestedqq-bfasl");
    let src = dir.join("q.lisp");
    let out = dir.join("q.bfasl");
    // alexandria's ONCE-ONLY verbatim, with a self-contained MAKE-GENSYM-LIST.
    fs::write(
        &src,
        "(defun make-gensym-list (n &optional (x \"G\"))\n\
        \x20 (let ((s (if (typep x '(integer 0)) x (string x))))\n\
        \x20   (loop repeat n collect (gensym s))))\n\
         (defmacro once-only (specs &body forms)\n\
        \x20 (let ((gensyms (make-gensym-list (length specs) \"ONCE-ONLY\"))\n\
        \x20       (names-and-forms\n\
        \x20         (mapcar (lambda (spec)\n\
        \x20                   (etypecase spec\n\
        \x20                     (list (destructuring-bind (name form) spec (cons name form)))\n\
        \x20                     (symbol (cons spec spec))))\n\
        \x20                 specs)))\n\
        \x20   `(let ,(mapcar (lambda (g n) (list g `(gensym ,(string (car n)))))\n\
        \x20                  gensyms names-and-forms)\n\
        \x20      `(let (,,@(mapcar (lambda (g n) ``(,,g ,,(cdr n)))\n\
        \x20                        gensyms names-and-forms))\n\
        \x20         ,(let ,(mapcar (lambda (n g) (list (car n) g)) names-and-forms gensyms)\n\
        \x20            ,@forms)))))\n\
         (defmacro cons1 (x) (once-only (x) `(cons ,x ,x)))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "a nested-quasiquote macro must compile to a complete source-free BBU"
    );

    // Load and use the macro: CONS1 must evaluate its argument exactly once.
    let l = run(&format!(
        "(progn (load \"{}\") \
           (let ((n 0)) (list (cons1 (progn (incf n) 5)) n)))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load/use failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "((5 . 5) 1)",
        "once-only must evaluate its argument exactly once"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A non-local `return-from` to an enclosing block — from the function's own
/// implicit block, from a capturing `labels` local, and from a noncapturing
/// lambda passed to `mapcar` — must compile source-free and unwind correctly
/// across the closure-call boundary from a fresh process.
#[test]
fn nonlocal_return_from_bfasl_round_trips() {
    let dir = workdir("nonlocal-return");
    let src = dir.join("r.lisp");
    let out = dir.join("r.bfasl");
    fs::write(
        &src,
        "(defun direct-rf (x) (if (> x 0) (return-from direct-rf :pos) :nonpos))\n\
         (defun find-even (xs)\n\
        \x20 (block found\n\
        \x20   (labels ((scan (l) (when l (if (evenp (car l)) (return-from found (car l)) (scan (cdr l))))))\n\
        \x20     (scan xs))\n\
        \x20   :none))\n\
         (defun any-big (xs)\n\
        \x20 (block done\n\
        \x20   (mapcar (lambda (x) (when (> x 100) (return-from done :big))) xs)\n\
        \x20   :all-small))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(bfasl_section(&fs::read(&out).unwrap(), 11).is_none());

    let l = run(&format!(
        "(progn (load \"{}\") \
           (list (direct-rf 5) (direct-rf -1) \
                 (find-even '(1 3 4 5)) (find-even '(1 3 5)) \
                 (any-big '(1 200 3)) (any-big '(1 2 3))))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "(:POS :NONPOS 4 :NONE :BIG :ALL-SMALL)",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Parallel `loop for X in L1 for Y in L2 collect …` must lower to source-free
/// bytecode and step the lists in lockstep, stopping when the shorter list is
/// exhausted (CL parallel-iteration semantics).
#[test]
fn parallel_loop_for_in_bfasl_round_trips() {
    let dir = workdir("parallel-loop");
    let src = dir.join("p.lisp");
    let out = dir.join("p.bfasl");
    fs::write(
        &src,
        "(defun zip-add (as bs) (loop for a in as for b in bs collect (+ a b)))\n\
         (defun zip-uneven (as bs) (loop for a in as for b in bs collect (list a b)))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(c.status.success(), "compile-file failed");
    assert!(bfasl_section(&fs::read(&out).unwrap(), 11).is_none());

    let l = run(&format!(
        "(progn (load \"{}\") (list (zip-add '(1 2 3) '(4 5 6)) (zip-uneven '(1 2 3) '(4 5))))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "((5 7 9) ((1 4) (2 5)))",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Capturing `flet`/`labels` locals — closures over enclosing lexicals — and
/// capturing lambdas must compile to source-free bytecode (env-capturing
/// `MakeClosure`) and run correctly from a fresh process, including read
/// capture, shared mutation, mutual recursion, and a lambda that closes over a
/// parameter.
#[test]
fn capturing_closures_bfasl_round_trip() {
    let dir = workdir("capturing-closures");
    let src = dir.join("c.lisp");
    let out = dir.join("c.bfasl");
    fs::write(
        &src,
        "(defun cap-read (x) (labels ((h (y) (+ x y))) (h 10)))\n\
         (defun cap-mutate (x)\n\
        \x20 (let ((acc 0))\n\
        \x20   (flet ((add (y) (setf acc (+ acc (* x y)))))\n\
        \x20     (add 1) (add 2) (add 3) acc)))\n\
         (defun cap-mutual (n)\n\
        \x20 (labels ((ev (k) (if (= k 0) t (od (- k 1))))\n\
        \x20          (od (k) (if (= k 0) nil (ev (- k 1)))))\n\
        \x20   (list (ev n) (od n))))\n\
         (defun cap-lambda (mult xs) (mapcar (lambda (x) (* x mult)) xs))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "capturing closures must compile to a complete source-free BBU"
    );

    let l = run(&format!(
        "(progn (load \"{}\") \
           (list (cap-read 5) (cap-mutate 10) (cap-mutual 4) (cap-mutual 3) \
                 (cap-lambda 3 '(1 2 3))))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load/call failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "(15 60 (T NIL) (NIL T) (3 6 9))",
        "capturing closures must run correctly from a fresh process",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A function installed from a `.bfasl` carries a NIL fallback body (its real
/// code lives in the bytecode registry). Calling it indirectly via
/// `funcall`/`apply`/`mapcar` — not just in operator position — must dispatch
/// through the registered bytecode, not run the empty body and return NIL
/// (bliss-mwe). ASDF/Babel funcall loaded functions constantly.
#[test]
fn funcall_and_apply_on_bfasl_function_dispatch_correctly() {
    let dir = workdir("funcall-bfasl");
    let src = dir.join("f.lisp");
    let out = dir.join("f.bfasl");
    fs::write(&src, "(defun bf-triple (n) (* n 3))\n").unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(c.status.success(), "compile-file failed");

    let l = run(&format!(
        "(progn (load \"{}\") \
           (list (bf-triple 4) \
                 (funcall 'bf-triple 4) \
                 (funcall #'bf-triple 4) \
                 (apply 'bf-triple '(4)) \
                 (mapcar #'bf-triple '(1 2 3))))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load/call failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "(12 12 12 12 (3 6 9))",
        "operator-position and funcall/apply/mapcar must all dispatch the bytecode"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// FNV-1a low-32 checksum over `bytes`, matching `egcl_rt::bfasl`'s framing.
/// Used to re-seal a `.bfasl` after deliberately corrupting a BBU byte so the
/// loader's structural verifier — not the container checksum — is what rejects
/// the tampered unit.
fn reseal_bfasl_checksum(bytes: &mut [u8]) {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let body_len = bytes.len() - 4;
    for &b in &bytes[..body_len] {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    bytes[body_len..].copy_from_slice(&(h as u32).to_le_bytes());
}

/// A noncapturing `(lambda …)` passed to a higher-order function must serialize
/// as a source-free `MakeClosure` referencing a nested bytecode function
/// (bliss-jtc.23.3), so the whole file compiles to a complete BBU with no
/// retained source and still executes correctly in a fresh process.
#[test]
fn noncapturing_lambda_bfasl_round_trips_source_free() {
    let dir = workdir("closure-roundtrip");
    let src = dir.join("c.lisp");
    let out = dir.join("c.bfasl");
    let source = "(defun nc-map (xs) (mapcar (lambda (x) (* x x)) xs))\n\
                  (defun nc-sum (xs) (reduce (lambda (a b) (+ a b)) xs :initial-value 0))\n";
    fs::write(&src, source).unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "a closure-bearing file must compile to a complete BBU with no TOPLEVEL_FORMS"
    );
    assert!(
        !bytes.windows(6).any(|w| w == b"LAMBDA"),
        "the artifact must not retain an executable LAMBDA source form"
    );
    let (_, function_count, load_action_count) = bbu_counts(&bytes);
    // Two named defuns plus their two nested lambda bodies.
    assert!(
        function_count >= 4,
        "nested lambda bodies get their own function records (got {function_count})"
    );
    assert!(load_action_count >= 2, "both defuns install at load");

    let l = run(&format!(
        "(progn (load \"{}\") (list (nc-map '(1 2 3)) (nc-sum '(4 5 6))))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "((1 4 9) 15)",
        "the fresh-loaded closures must compute correct results"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A `MakeClosure` reference that is not a valid backwards/topological pointer
/// to a nested function must be rejected by the loader's verifier before any
/// function is installed (bliss-jtc.23.3). The container checksum is repaired
/// after tampering so it is the structural verifier that does the rejecting.
#[test]
fn corrupt_makeclosure_reference_is_rejected() {
    let dir = workdir("closure-corrupt");
    let src = dir.join("cc.lisp");
    let out = dir.join("cc.bfasl");
    fs::write(&src, "(defun cc (xs) (mapcar (lambda (x) x) xs))\n").unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(c.status.success(), "compile-file failed");
    let mut bytes = fs::read(&out).unwrap();

    // The sole nested lambda is serialized first (global index 0), so the owner
    // encodes `MakeClosure` as opcode 0x0b + u32 index 0 + u16 capture-count 0.
    let pattern = [0x0b, 0, 0, 0, 0, 0, 0];
    let positions: Vec<usize> = bytes
        .windows(pattern.len())
        .enumerate()
        .filter(|(_, w)| *w == pattern)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        positions.len(),
        1,
        "expected exactly one MAKE_CLOSURE encoding to corrupt"
    );
    // Rewrite the global function index to a forward/out-of-range value.
    bytes[positions[0] + 1..positions[0] + 5].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
    reseal_bfasl_checksum(&mut bytes);
    fs::write(&out, &bytes).unwrap();

    let l = run(&format!("(load \"{}\")", out.display()));
    assert!(
        !l.status.success(),
        "loader must reject an out-of-range closure reference; stdout={} stderr={}",
        String::from_utf8_lossy(&l.stdout),
        String::from_utf8_lossy(&l.stderr)
    );
    // Not a checksum failure — the structural verifier is what rejects it.
    let stderr = String::from_utf8_lossy(&l.stderr);
    assert!(
        !stderr.contains("checksum"),
        "rejection must come from the closure-reference verifier, not the checksum: {stderr}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A `egcl-ext:` builtin called from a source-free `.bfasl` must dispatch to
/// the builtin. The constant pool records the callee symbol under its registry
/// key, which can be the internal `EGCL-EXT::NAME` spelling, while the builtin
/// dispatch arms use the external `EGCL-EXT:NAME` spelling — so a compiled
/// reference resolved to a distinct, undefined symbol. This is uiop's
/// `#+egcl (egcl-ext:raw-command-line-arguments)` (bliss-lb6).
#[test]
fn egcl_ext_builtin_call_bfasl_round_trips() {
    let dir = workdir("egcl-ext-builtin");
    let src = dir.join("b.lisp");
    let out = dir.join("b.bfasl");
    // getenv is a stable egcl-ext builtin with a deterministic result here.
    fs::write(
        &src,
        "(defun home-set-p () (if (egcl-ext:getenv \"EGCL_TEST_MARKER\") t nil))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );

    // Fresh process: calling the compiled function must reach the builtin (not
    // raise "undefined function: EGCL-EXT::GETENV").
    let l = run(&format!(
        "(progn (load \"{}\") (home-set-p))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load/call failed (egcl-ext builtin not dispatched?): {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "NIL");

    let _ = fs::remove_dir_all(&dir);
}

/// A top-level `(defsetf access-fn lambda-list (store-vars) body)` must not be
/// thunk-compiled: the bytecode lowerer doesn't understand DEFSETF, so it would
/// miscompile it as an ordinary call and evaluate the access-fn name and store
/// vars as variables — loading the unit then died with "unbound variable" (uiop's
/// `(defsetf getenv (x) (val) …)` => unbound UIOP/OS::GETENV). It must reach the
/// load-source fallback, where the tree-walker registers the setf expander
/// (bliss-lb6).
#[test]
fn defsetf_top_level_bfasl_round_trips() {
    let dir = workdir("defsetf");
    let src = dir.join("d.lisp");
    let out = dir.join("d.bfasl");
    fs::write(
        &src,
        "(defun myenv (x) (declare (ignore x)) nil)\n\
         (defsetf myenv (x) (val)\n\
           (declare (ignorable x val))\n\
           '(error \"not implemented\"))\n\
         (defun loaded-ok () :loaded)\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );

    // The whole point: loading the unit runs the defsetf form without error.
    let l = run(&format!("(progn (load \"{}\") (loaded-ok))", out.display()));
    assert!(
        l.status.success(),
        "load failed (defsetf miscompiled?): {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), ":LOADED");

    let _ = fs::remove_dir_all(&dir);
}

/// A `#p"…"` pathname literal in a top-level form must survive compile-file:
/// the portable compiler can't lower a `defun`, so it falls back to serialising
/// the form as data — which requires the constant pool to represent the pathname
/// (as its namestring, rebuilt with parse-namestring on load). uiop's
/// `null-device-pathname` returns `#p"/dev/null"`; without pooling support the
/// whole file failed to compile (bliss-lb6).
#[test]
fn pathname_literal_bfasl_round_trips() {
    let dir = workdir("pathname-lit");
    let src = dir.join("p.lisp");
    let out = dir.join("p.bfasl");
    fs::write(
        &src,
        "(defun devnull () #p\"/dev/null\")\n\
         (defun in-dir () #p\"/tmp/sub/file.txt\")\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );

    let l = run(&format!(
        "(progn (load \"{}\") \
           (list (pathnamep (devnull)) (namestring (devnull)) \
                 (pathnamep (in-dir)) (namestring (in-dir))))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "(T \"/dev/null\" T \"/tmp/sub/file.txt\")",
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A `PushRestartCase` selects its clause table by index at run time
/// (`func.restart_cases[rc]`); the loader must reject an out-of-range index
/// before the instruction can execute, rather than panic on the Vec index
/// (bliss-jtc.23.4).
#[test]
fn corrupt_restart_case_index_is_rejected() {
    let dir = workdir("rc-corrupt");
    let src = dir.join("rc.lisp");
    let out = dir.join("rc.bfasl");
    fs::write(
        &src,
        "(defun rc (x) (restart-case (if (< x 0) (error \"neg\") x) (use-it (v) v)))\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(c.status.success(), "compile-file failed");
    let mut bytes = fs::read(&out).unwrap();

    // The single restart-case is index 0, so `PushRestartCase` encodes as opcode
    // 0x2c + u32 rc=0 + u32 resume_bcp + u16 sp_restore. Find the opcode+rc=0
    // prefix and rewrite rc to an out-of-range value.
    let pattern = [0x2c, 0, 0, 0, 0];
    let positions: Vec<usize> = bytes
        .windows(pattern.len())
        .enumerate()
        .filter(|(_, w)| *w == pattern)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        positions.len(),
        1,
        "expected exactly one PUSH_RESTART_CASE with rc=0 to corrupt"
    );
    bytes[positions[0] + 1..positions[0] + 5].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
    reseal_bfasl_checksum(&mut bytes);
    fs::write(&out, &bytes).unwrap();

    let l = run(&format!("(load \"{}\")", out.display()));
    assert!(
        !l.status.success(),
        "loader must reject an out-of-range restart-case index; stdout={} stderr={}",
        String::from_utf8_lossy(&l.stdout),
        String::from_utf8_lossy(&l.stderr)
    );
    let stderr = String::from_utf8_lossy(&l.stderr);
    // A CLEAN verifier rejection — not a checksum failure and, crucially, not a
    // panic/abort from indexing the clause table out of bounds.
    assert!(
        stderr.contains("restart-case index out of range"),
        "expected the restart-case index verifier to reject cleanly, got: {stderr}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Craft a unit whose sole `CallNamed` has its `nargs` operand rewritten, then
/// reseal the checksum. `patched_nargs` beyond the available operands must be
/// rejected as an underflow (the interpreter's unguarded `pop_op` would read
/// below the operand stack); `patched_nargs` of 0 leaves an extra operand that
/// pushes the result past the declared `max_stack` (an out-of-frame write).
/// Both must be clean load-time rejections, before any action runs
/// (bliss-jtc.23.4).
fn assert_corrupt_callnamed_nargs_rejected(tag: &str, patched_nargs: u16, expected: &str) {
    let dir = workdir(tag);
    let src = dir.join("v.lisp");
    let out = dir.join("v.bfasl");
    // `voof` is undefined at compile time, so the call stays a CallNamed.
    fs::write(&src, "(defun vg (a b) (voof a b))\n").unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(c.status.success(), "compile-file failed");
    let mut bytes = fs::read(&out).unwrap();

    // CallNamed encodes as opcode 0x0d + u32 callee sym + u16 nargs (= 2 here).
    let positions: Vec<usize> = (0..bytes.len().saturating_sub(7))
        .filter(|&i| bytes[i] == 0x0d && bytes[i + 5] == 0x02 && bytes[i + 6] == 0x00)
        .collect();
    assert_eq!(
        positions.len(),
        1,
        "expected exactly one CALL_NAMED encoding to corrupt"
    );
    bytes[positions[0] + 5..positions[0] + 7].copy_from_slice(&patched_nargs.to_le_bytes());
    reseal_bfasl_checksum(&mut bytes);
    fs::write(&out, &bytes).unwrap();

    let l = run(&format!("(load \"{}\")", out.display()));
    assert!(
        !l.status.success(),
        "loader must reject the corrupt stack discipline; stdout={} stderr={}",
        String::from_utf8_lossy(&l.stdout),
        String::from_utf8_lossy(&l.stderr)
    );
    let stderr = String::from_utf8_lossy(&l.stderr);
    assert!(
        stderr.contains(expected),
        "expected a clean stack-verifier rejection ({expected}), got: {stderr}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_call_arity_stack_underflow_is_rejected() {
    assert_corrupt_callnamed_nargs_rejected("stack-underflow", 9, "operand stack underflow");
}

#[test]
fn corrupt_call_arity_stack_overflow_is_rejected() {
    assert_corrupt_callnamed_nargs_rejected(
        "stack-overflow",
        0,
        "operand stack exceeds declared max_stack",
    );
}

#[test]
fn compile_file_then_load_round_trips_in_a_fresh_process() {
    let dir = workdir("roundtrip");
    let src = dir.join("u.lisp");
    let out = dir.join("u.bfasl");
    let source = "(defun bf-sq (x) (* x x))\n(defvar *bf-g* 7)\n";
    fs::write(&src, source).unwrap();

    // Process 1: compile source → .bfasl.
    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    assert!(out.exists(), "compile-file produced no .bfasl");
    let bytes = fs::read(&out).unwrap();
    assert_eq!(&bytes[..6], b"BFASL\0", "output is a real .bfasl");
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "new .bfasl writers must not emit legacy TOPLEVEL_FORMS"
    );
    assert!(
        !bytes
            .windows(source.len())
            .any(|window| window == source.as_bytes()),
        "the artifact must not retain its source text"
    );
    let (_, function_count, load_action_count) = bbu_counts(&bytes);
    assert!(
        function_count > 0,
        "BYTECODE_UNIT contains bytecode functions"
    );
    assert!(load_action_count > 0, "BYTECODE_UNIT contains a load plan");

    // Process 2 (fresh runtime): load the .bfasl and call the compiled function.
    let l = run(&format!(
        "(progn (load \"{}\") (list (bf-sq 12) *bf-g*))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "(144 7)",
        "loaded .bfasl must define the function and the variable"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn extensionless_load_uses_defaulted_bfasl() {
    let dir = workdir("load-default-bfasl");
    let stem = dir.join("m");
    let src = dir.join("m.lisp");
    let out = dir.join("m.bfasl");
    fs::write(&src, "(defun defaulted-load-value () 31)\n").unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    fs::remove_file(&src).unwrap();

    let l = run(&format!(
        "(progn (load \"{}\") (defaulted-load-value))",
        stem.display()
    ));
    assert!(
        l.status.success(),
        "extensionless load failed: stdout={} stderr={}",
        String::from_utf8_lossy(&l.stdout),
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "31");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn explicit_source_load_does_not_prefer_bfasl() {
    let dir = workdir("load-explicit-source");
    let src = dir.join("s.lisp");
    let out = dir.join("s.bfasl");
    fs::write(&src, "(defun explicit-source-value () 1)\n").unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(c.status.success());
    fs::write(&src, "(defun explicit-source-value () 2)\n").unwrap();

    let l = run(&format!(
        "(progn (load \"{}\") (explicit-source-value))",
        src.display()
    ));
    assert!(
        l.status.success(),
        "explicit source load failed: stdout={} stderr={}",
        String::from_utf8_lossy(&l.stdout),
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "2");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn extensionless_load_rejects_stale_bfasl() {
    let dir = workdir("load-stale-bfasl");
    let stem = dir.join("stale");
    let src = dir.join("stale.lisp");
    let out = dir.join("stale.bfasl");
    fs::write(&src, "(defun stale-value () 1)\n").unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(c.status.success());
    std::thread::sleep(Duration::from_millis(1100));
    fs::write(&src, "(defun stale-value () 2)\n").unwrap();

    let l = run(&format!("(load \"{}\")", stem.display()));
    assert!(
        !l.status.success(),
        "stale extensionless load should fail; stdout={} stderr={}",
        String::from_utf8_lossy(&l.stdout),
        String::from_utf8_lossy(&l.stderr)
    );
    assert!(
        String::from_utf8_lossy(&l.stderr).contains("older than source"),
        "stale error should explain the conflict: {}",
        String::from_utf8_lossy(&l.stderr)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn compile_file_prepass_handles_eval_when_and_read_time_constants() {
    let dir = workdir("eval-when");
    let src = dir.join("e.lisp");
    let out = dir.join("e.bfasl");
    fs::write(
        &src,
        "(eval-when (compile) (defparameter +cf-read+ 12))
         (eval-when (load) (defun cf-load-short () 23))
         (defun cf-readtime () #.(+ +cf-read+ 5))
         (defun cf-limit () #.most-positive-fixnum)\n",
    )
    .unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "compile-file failed: stdout={} stderr={}",
        String::from_utf8_lossy(&c.stdout),
        String::from_utf8_lossy(&c.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    let (_, function_count, load_action_count) = bbu_counts(&bytes);
    assert!(
        function_count >= 3,
        "expected all three functions in BYTECODE_UNIT"
    );
    assert!(
        load_action_count >= 3,
        "expected load actions for all three functions"
    );

    let l = run(&format!(
        "(progn (load \"{}\") (list (cf-readtime) (cf-load-short)))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "short-form EVAL-WHEN bfasl failed to load: stdout={} stderr={}",
        String::from_utf8_lossy(&l.stdout),
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&l.stdout).trim(), "(17 23)");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn compile_file_round_trips_define_package_without_source() {
    let dir = workdir("define-package");
    let src = dir.join("p.lisp");
    let out = dir.join("p.bfasl");
    let source =
        "(define-package :bf/pkg (:nicknames :bf-pkg) (:use :common-lisp) (:export #:pkg-value))
         (in-package :bf-pkg)
         (defun pkg-value () 42)\n";
    fs::write(&src, source).unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        c.status.success(),
        "package compilation failed: stdout={} stderr={}",
        String::from_utf8_lossy(&c.stdout),
        String::from_utf8_lossy(&c.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "package BFASLs must not retain executable source forms"
    );
    assert!(
        !bytes
            .windows(source.len())
            .any(|window| window == source.as_bytes()),
        "package BFASLs must not retain their source text"
    );

    let l = run(&format!(
        "(progn (load \"{}\")
                (list (eval (read-from-string \"(bf-pkg:pkg-value)\"))
                      (eq (find-package \"BF/PKG\") (find-package \"BF-PKG\"))
                      (nth-value 1 (find-symbol \"+\" \"BF-PKG\"))
                      (nth-value 1 (find-symbol \"PKG-VALUE\" \"BF-PKG\"))))",
        out.display()
    ));
    assert!(
        l.status.success(),
        "fresh package load failed: stdout={} stderr={}",
        String::from_utf8_lossy(&l.stdout),
        String::from_utf8_lossy(&l.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&l.stdout).trim(),
        "(42 T :INHERITED :EXTERNAL)"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn circular_literals_round_trip_without_source() {
    let dir = workdir("circular-literals");
    let src = dir.join("circles.lisp");
    let out = dir.join("circles.bfasl");
    fs::write(
        &src,
        r#"
      (defun cf-car-circle () '#1=(#1#))
      (defun cf-cdr-circle () '#2=(42 . #2#))
      (defun cf-vector-circle ()
        '#.(let ((v (vector nil))) (setf (aref v 0) v) v))
      (defun cf-mixed-circle () '#4=(#(#4# #4#)))
      (defun cf-array-circle ()
        '#.(let ((a (make-array '(1 2))))
             (setf (aref a 0 0) a (aref a 0 1) a) a))
    "#,
    )
    .unwrap();
    let compiled = run(&format!("(compile-file {src:?} :output-file {out:?})"));
    assert!(
        compiled.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    bbu_counts(&bytes);
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "circular constants must not use source replay"
    );
    fs::remove_file(&src).unwrap();
    let loaded = run(&format!(
        r#"(progn (load {out:?})
      (let ((a (cf-car-circle)) (b (cf-cdr-circle))
            (c (cf-vector-circle)) (d (cf-mixed-circle)) (e (cf-array-circle)))
        (list (eq a (car a)) (eq b (cdr b)) (= 42 (car b))
              (eq c (aref c 0)) (eq d (aref (car d) 0))
              (eq d (aref (car d) 1)) (eq e (aref e 0 0))
              (eq e (aref e 0 1)))))"#
    ));
    assert!(
        loaded.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&loaded.stdout).trim(),
        "(T T T T T T T T)"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn read_time_multidimensional_array_is_portable_without_compile_time_helper() {
    let dir = workdir("read-time-md-array");
    let src = dir.join("array.lisp");
    let out = dir.join("array.bfasl");
    fs::write(
        &src,
        "(eval-when (:compile-toplevel)\n\
           (defun cf-build-grid ()\n\
             (let ((grid (make-array '(2 2) :initial-element 0)))\n\
               (setf (aref grid 1 0) 42)\n\
               grid)))\n\
         (defparameter *cf-grid* #.(cf-build-grid))\n\
         (defun cf-grid-value () (aref *cf-grid* 1 0))\n",
    )
    .unwrap();

    let compiled = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        compiled.status.success(),
        "compile-file failed: stdout={} stderr={}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "a read-time array must be encoded in the portable BBU, not by replaying source"
    );
    assert!(
        bfasl_section(&bytes, 12).is_some(),
        "compile-file must emit an authoritative BYTECODE_UNIT"
    );

    let loaded = run(&format!(
        "(progn (load \"{}\") (list (cf-grid-value) (fboundp 'cf-build-grid)))",
        out.display()
    ));
    assert!(
        loaded.status.success(),
        "fresh-process load failed: stdout={} stderr={}",
        String::from_utf8_lossy(&loaded.stdout),
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&loaded.stdout).trim(), "(42 NIL)");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn compiled_in_package_affects_load_time_exports_and_restores_caller() {
    let dir = workdir("load-time-package");
    let src = dir.join("package-effects.lisp");
    let out = dir.join("package-effects.bfasl");
    fs::write(
        &src,
        r#"
      (defpackage :load-package-a (:use :cl))
      (defpackage :load-package-b (:use :cl))
      (in-package :load-package-a)
      (defmacro publish (name)
        `(eval-when (:load-toplevel :execute) (export ',name)))
      (publish first-name)
      (defparameter *loaded-package* (package-name *package*))
      (in-package :load-package-b)
      (eval-when (:load-toplevel :execute) (export 'second-name))
      (defparameter *loaded-package* (package-name *package*))
    "#,
    )
    .unwrap();
    let compiled = run(&format!("(compile-file {:?} {:?})", src, out));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "must remain source-free"
    );
    fs::remove_file(&src).unwrap();
    let loaded = run(&format!(
        r#"
      (let ((*package* (find-package :cl-user)))
        (load {out:?})
        (assert (eq *package* (find-package :cl-user)))
        (assert (eq :external (nth-value 1 (find-symbol "FIRST-NAME" :load-package-a))))
        (assert (eq :external (nth-value 1 (find-symbol "SECOND-NAME" :load-package-b))))
        (assert (string= "LOAD-PACKAGE-A" (symbol-value (find-symbol "*LOADED-PACKAGE*" :load-package-a))))
        (assert (string= "LOAD-PACKAGE-B" (symbol-value (find-symbol "*LOADED-PACKAGE*" :load-package-b))))
        (format t "LOAD-PACKAGE-EFFECTS-OK~%"))
    "#
    ));
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert!(String::from_utf8_lossy(&loaded.stdout).contains("LOAD-PACKAGE-EFFECTS-OK"));
}

#[test]
fn compile_file_preserves_shadowed_symbol_identity() {
    let dir = workdir("package-shadow");
    let src = dir.join("shadow.lisp");
    let out = dir.join("shadow.bfasl");
    fs::write(
        &src,
        "(defpackage :bf/shadow (:use :common-lisp) (:shadow #:car))
         (in-package :bf/shadow)
         (defun shadow-is-distinct ()
           (list (not (eq 'car 'cl:car))
                 (package-name (symbol-package 'car))))\n",
    )
    .unwrap();

    let compiled = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        compiled.status.success(),
        "shadow package fixture failed: stdout={} stderr={}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );

    let loaded = run(&format!(
        "(progn (load \"{}\")
                (eval (read-from-string \"(bf/shadow::shadow-is-distinct)\")))",
        out.display()
    ));
    assert!(
        loaded.status.success(),
        "shadow package BFASL failed: stdout={} stderr={}",
        String::from_utf8_lossy(&loaded.stdout),
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&loaded.stdout).trim(),
        "(T \"BF/SHADOW\")"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Generated data files such as cl-unicode contain quoted lists with tens of
/// thousands of cons cells.  Constant-pool encoding must walk their spines
/// without consuming one native stack frame per cons.
#[test]
fn compile_file_encodes_deep_list_constants_iteratively() {
    let dir = workdir("deep-list-constant");
    let src = dir.join("deep.lisp");
    let out = dir.join("deep.bfasl");
    let mut source = String::from("(defun deep-list-value () '(7");
    for _ in 0..50_000 {
        source.push_str(" 0");
    }
    source.push_str("))\n");
    fs::write(&src, source).unwrap();

    let compiled = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        compiled.status.success(),
        "deep constant compilation failed: stdout={} stderr={}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );

    let loaded = run(&format!(
        "(progn (load \"{}\") (list (length (deep-list-value)) (car (deep-list-value))))",
        out.display()
    ));
    assert!(
        loaded.status.success(),
        "deep constant load failed: stdout={} stderr={}",
        String::from_utf8_lossy(&loaded.stdout),
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&loaded.stdout).trim(), "(50001 7)");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn malformed_package_action_is_rejected_before_package_creation() {
    let dir = workdir("invalid-package-action");
    let src = dir.join("bad-package.lisp");
    let good = dir.join("bad-package.bfasl");
    let bad = dir.join("bad.bfasl");
    fs::write(
        &src,
        "(defpackage :bbu-must-not-exist (:use :common-lisp) (:export #:x))\n",
    )
    .unwrap();
    let compiled = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        good.display()
    ));
    assert!(
        compiled.status.success(),
        "fixture compilation failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let image = egcl_rt::bfasl::load(&fs::read(&good).unwrap()).unwrap();
    let mut builder = egcl_rt::bfasl::BfaslBuilder::new();
    for (kind, section) in image.sections() {
        let mut section = section.to_vec();
        if kind == egcl_rt::bfasl::section::BYTECODE_UNIT {
            let action_start = bbu_action_start(&section);
            assert_eq!(section[action_start], 1, "first action is EnsurePackage");
            section[action_start + 2..action_start + 6].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        builder = builder.section(kind, section);
    }
    fs::write(&bad, builder.build()).unwrap();

    let loaded = run(&format!(
        "(progn (handler-case (load \"{}\") (error () nil))
                (find-package \"BBU-MUST-NOT-EXIST\"))",
        bad.display()
    ));
    assert!(loaded.status.success());
    assert_eq!(
        String::from_utf8_lossy(&loaded.stdout).trim(),
        "NIL",
        "verification must finish before EnsurePackage mutates the registry"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn malformed_set_package_action_is_rejected_before_any_load_effects() {
    let dir = workdir("invalid-set-package");
    let src = dir.join("set-package.lisp");
    let good = dir.join("good.bfasl");
    fs::write(
        &src,
        "(defpackage :package-action-guard (:use :cl))\n(in-package :package-action-guard)\n",
    )
    .unwrap();
    assert!(
        run(&format!("(compile-file {:?} {:?})", src, good))
            .status
            .success()
    );
    let image = egcl_rt::bfasl::load(&fs::read(&good).unwrap()).unwrap();
    for mutation in ["index", "flags", "argument", "version"] {
        let bad = dir.join(format!("{mutation}.bfasl"));
        let mut builder = egcl_rt::bfasl::BfaslBuilder::new();
        for (kind, bytes) in image.sections() {
            let mut section = bytes.to_vec();
            if kind == egcl_rt::bfasl::section::BYTECODE_UNIT {
                let count = u32::from_le_bytes(section[20..24].try_into().unwrap()) as usize;
                let start = bbu_action_start(&section);
                let action = (0..count)
                    .map(|i| start + 14 * i)
                    .find(|&i| section[i] == 12)
                    .expect("SetPackage emitted");
                match mutation {
                    "index" => {
                        section[action + 2..action + 6].copy_from_slice(&u32::MAX.to_le_bytes())
                    }
                    "flags" => section[action + 1] = 1,
                    "argument" => {
                        section[action + 6..action + 10].copy_from_slice(&0u32.to_le_bytes())
                    }
                    "version" => section[4..6].copy_from_slice(&0x010bu16.to_le_bytes()),
                    _ => unreachable!(),
                }
            }
            builder = builder.section(kind, section);
        }
        fs::write(&bad, builder.build()).unwrap();
        let output = run(&format!(
            r#"
          (progn
            (assert (handler-case (progn (load {bad:?}) nil) (error () t)))
            (assert (null (find-package "PACKAGE-ACTION-GUARD")))
            (format t "REJECTED-BEFORE-EFFECTS~%"))
        "#
        ));
        assert!(
            output.status.success(),
            "{mutation}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("REJECTED-BEFORE-EFFECTS"));
    }
}

#[test]
fn macro_expanded_single_value_tail_survives_fasl() {
    let dir = workdir("macro-multiple-values");
    let src = dir.join("values.lisp");
    let out = dir.join("values.bfasl");
    fs::write(
        &src,
        r#"
      (defmacro hidden-tail () '(let ((s "abc")) (values 2 3) s))
      (defun single-tail () (hidden-tail))
      (defun multiple-tail () (let ((s "abc")) (values s 3)))
    "#,
    )
    .unwrap();
    let compiled = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(
        bfasl_section(&bytes, 11).is_none(),
        "must compile without retained source"
    );
    bbu_counts(&bytes);
    fs::remove_file(&src).unwrap();
    let loaded = run(&format!(
        "(progn (load \"{}\") (list (multiple-value-list (single-tail)) (multiple-value-list (multiple-tail))))",
        out.display()
    ));
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&loaded.stdout).trim(),
        "((\"abc\") (\"abc\" 3))"
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn macro_and_compiler_macro_expanders_round_trip_as_bytecode() {
    let dir = workdir("macro-bytecode");
    let src = dir.join("macros.lisp");
    let out = dir.join("macros.bfasl");
    let source = "(defmacro bbu-twice (x) (list '+ x x))
                  (defun bbu-use-twice (x) (bbu-twice x))
                  (defun bbu-cm-target (x) (+ x 1))
                  (define-compiler-macro bbu-cm-target (x) (list '+ x 10))
                  (defun bbu-cm-compiled () (bbu-cm-target 5))
                  (defun bbu-cm-decline (x) (+ x 1))
                  (define-compiler-macro bbu-cm-decline (&whole whole x)
                    (if (constantp x) (list '+ x 20) whole))\n";
    fs::write(&src, source).unwrap();
    let compiled = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        compiled.status.success(),
        "macro fixture compilation failed: stdout={} stderr={}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    let bytes = fs::read(&out).unwrap();
    assert!(bfasl_section(&bytes, 11).is_none());
    assert!(
        !bytes
            .windows(source.len())
            .any(|window| window == source.as_bytes()),
        "macro BFASL must not retain executable source text"
    );

    let loaded = run(&format!(
        "(progn
           (load \"{}\")
           (defun bbu-cm-later () (bbu-cm-target 2))
           (defun bbu-cm-declined-later (x) (bbu-cm-decline x))
           (list (bbu-twice 9)
                 (bbu-use-twice 7)
                 (bbu-cm-compiled)
                 (bbu-cm-later)
                 (bbu-cm-declined-later 5)))",
        out.display()
    ));
    assert!(
        loaded.status.success(),
        "fresh macro load failed: stdout={} stderr={}",
        String::from_utf8_lossy(&loaded.stdout),
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&loaded.stdout).trim(),
        "(18 14 15 12 6)",
        "compile diagnostics: {}\nload diagnostics: {}",
        String::from_utf8_lossy(&compiled.stderr),
        String::from_utf8_lossy(&loaded.stderr)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn methods_expand_enclosing_local_macros_before_the_scope_exits() {
    let dir = workdir("method-enclosing-macro");
    let src = dir.join("method.lisp");
    let out = dir.join("method.bfasl");
    let source = r#"
(macrolet ((define-modes (&rest names &environment env)
             `(progn ,@(loop for name in names collect (macroexpand `(,name) env))))
           (mode-lambda (&body body) `(lambda (start) ,@body)))
  (macrolet ((mode-crypt ()
               `(defmethod make-mode ((cipher t))
                  (let ((key cipher))
                    (values (mode-lambda (+ key start))
                            (mode-lambda (- key start)))))))
    (define-modes mode-crypt)))
"#;
    fs::write(&src, source).unwrap();
    let call = "(multiple-value-bind (e d) (make-mode 10) (list (funcall e 3) (funcall d 3)))";
    let interpreted = run(&format!("(progn {source} {call})"));
    assert!(
        interpreted.status.success(),
        "{}",
        String::from_utf8_lossy(&interpreted.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&interpreted.stdout).trim(),
        "(13 7)"
    );
    let compiled = run(&format!(
        "(compile-file {:?} {:?})",
        src.to_str().unwrap(),
        out.to_str().unwrap()
    ));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    fs::remove_file(&src).unwrap();
    let loaded = run(&format!(
        "(progn (load {:?}) {call})",
        out.to_str().unwrap()
    ));
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&loaded.stdout).trim(), "(13 7)");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn local_macros_compile_in_each_forms_package() {
    let dir = workdir("local-macro-package");
    let src = dir.join("local-macro.lisp");
    let out = dir.join("local-macro.bfasl");
    fs::write(
        &src,
        r#"
(defpackage :local-macro-one (:use :cl) (:export :via-function :via-method))
(defpackage :local-macro-two (:use :cl) (:export :via-function))
(in-package :local-macro-one)
(defun helper () 42)
(defun via-function ()
  (macrolet ((call-helper () (list (intern "HELPER")))) (call-helper)))
(defmethod via-method ((x t))
  (declare (ignore x))
  (flet ((inner ()
           (macrolet ((call-helper () (list (intern "HELPER")))) (call-helper))))
    (inner)))
(in-package :local-macro-two)
(defun helper () 17)
(defun via-function ()
  (macrolet ((call-helper () (list (intern "HELPER")))) (call-helper)))
"#,
    )
    .unwrap();
    let compiled = run(&format!(
        "(compile-file {:?} {:?})",
        src.to_str().unwrap(),
        out.to_str().unwrap()
    ));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    fs::remove_file(&src).unwrap();
    let loaded = run(&format!(
        r#"(progn (load {:?})
      (list (eval (read-from-string "(local-macro-one:via-function)"))
            (eval (read-from-string "(local-macro-one:via-method nil)"))
            (eval (read-from-string "(local-macro-two:via-function)"))
            (package-name *package*)))"#,
        out.to_str().unwrap()
    ));
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&loaded.stdout).trim(),
        "(42 42 17 \"COMMON-LISP-USER\")"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn loaded_compiler_macro_uses_definition_package_without_leaking_it() {
    let dir = workdir("compiler-macro-package");
    let src = dir.join("package-macro.lisp");
    let out = dir.join("package-macro.bfasl");
    fs::write(
        &src,
        "(defpackage :bbu-cm-target (:use :common-lisp) (:export #:target))
         (defpackage :bbu-cm-def (:use :common-lisp))
         (progn
           (in-package :bbu-cm-def)
           (defun bbu-cm-target:target () :ordinary)
           (define-compiler-macro bbu-cm-target:target ()
             (list 'quote (package-name *package*))))\n",
    )
    .unwrap();

    let compiled = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        compiled.status.success(),
        "compiler-macro package fixture failed: stdout={} stderr={}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );

    let loaded = run(&format!(
        "(progn
           (load \"{}\")
           (list (eval (read-from-string \"(bbu-cm-target:target)\"))
                 (package-name *package*)))",
        out.display()
    ));
    assert!(
        loaded.status.success(),
        "loaded compiler macro failed: stdout={} stderr={}",
        String::from_utf8_lossy(&loaded.stdout),
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&loaded.stdout).trim(),
        "(\"BBU-CM-DEF\" \"COMMON-LISP-USER\")"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Regression (bliss-c3i): COMPILE-FILE writes the fasl atomically (temp +
/// rename), so a successful compile leaves exactly the `.bfasl` and no temp
/// remnant, and the result loads cleanly.
#[test]
fn compile_file_writes_fasl_atomically_without_temp_leak() {
    let dir = workdir("atomic-write");
    let src = dir.join("atomic.lisp");
    let out = dir.join("atomic.bfasl");
    fs::write(&src, "(defun c3i-sq (x) (* x x))\n").unwrap();

    let compiled = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        compiled.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    assert!(out.exists(), "the .bfasl must exist after compile");

    // No leftover temp file (the atomic writer uses a `.atomic.bfasl.tmp…`
    // sibling that must have been renamed away).
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "atomic write left temp files behind: {leftovers:?}"
    );

    let loaded = run(&format!(
        "(progn (load \"{}\") (format t \"~s\" (c3i-sq 9)))",
        out.display()
    ));
    assert!(
        loaded.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert!(
        String::from_utf8_lossy(&loaded.stdout).contains("81"),
        "loaded fasl gave wrong result: {}",
        String::from_utf8_lossy(&loaded.stdout)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn variadic_lambda_lists_and_declared_types_survive_bbu_round_trip() {
    let dir = workdir("variadic-metadata");
    let src = dir.join("variadic.lisp");
    let out = dir.join("variadic.bfasl");
    fs::write(
        &src,
        "(defun bbu-variadic (a &optional (b 2) &rest tail) (list a b tail))
         (defun bbu-typed (x) (declare (fixnum x)) (+ x 1))
         (defmacro bbu-listing (&body forms) (cons 'list forms))\n",
    )
    .unwrap();
    let compiled = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(
        compiled.status.success(),
        "variadic fixture compilation failed: stdout={} stderr={}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );

    let loaded = run(&format!(
        "(progn (load \"{}\")
                (list (bbu-variadic 1)
                      (bbu-variadic 1 3 4 5)
                      (bbu-typed 8)
                      (handler-case (bbu-typed \"bad\") (type-error () :typed))
                      (bbu-listing 6 7)))",
        out.display()
    ));
    assert!(
        loaded.status.success(),
        "fresh variadic load failed: stdout={} stderr={}",
        String::from_utf8_lossy(&loaded.stdout),
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&loaded.stdout).trim(),
        "((1 2 NIL) (1 3 (4 5)) 9 :TYPED (6 7))"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn version_mismatch_is_rejected() {
    let dir = workdir("version");
    let src = dir.join("v.lisp");
    let out = dir.join("v.bfasl");
    fs::write(&src, "(defun bf-id (x) x)\n").unwrap();

    let c = run(&format!(
        "(compile-file \"{}\" \"{}\")",
        src.display(),
        out.display()
    ));
    assert!(c.status.success());

    // Corrupt the major version byte (u16 LE at offset 6; high byte at 7).
    let mut bytes = fs::read(&out).unwrap();
    bytes[7] = bytes[7].wrapping_add(1);
    fs::write(&out, &bytes).unwrap();

    // Loading the incompatible file must be rejected — caught here as an error.
    let l = run(&format!(
        "(handler-case (load \"{}\") (error (e) e (print :rejected)))",
        out.display()
    ));
    let stdout = String::from_utf8_lossy(&l.stdout);
    assert!(
        stdout.to_uppercase().contains("REJECTED") || !l.status.success(),
        "a version-incompatible .bfasl must be rejected; stdout={stdout:?} stderr={:?}",
        String::from_utf8_lossy(&l.stderr)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn present_bbu_is_authoritative_and_never_falls_back_to_legacy_source() {
    let dir = workdir("authoritative-bbu");
    let out = dir.join("bad.bfasl");
    let image = egcl_rt::bfasl::BfaslBuilder::new()
        .section(egcl_rt::bfasl::section::BYTECODE_UNIT, b"BBU\0".to_vec())
        .section(
            egcl_rt::bfasl::section::TOPLEVEL_FORMS,
            b"(defparameter *source-fallback-ran* t)".to_vec(),
        )
        .build();
    fs::write(&out, image).unwrap();

    let loaded = run(&format!("(load \"{}\")", out.display()));
    assert!(
        !loaded.status.success(),
        "an invalid BBU must be rejected even when legacy source is present"
    );
    assert!(
        String::from_utf8_lossy(&loaded.stderr).contains("invalid BBU"),
        "the authoritative BBU error should be reported: {}",
        String::from_utf8_lossy(&loaded.stderr)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn source_only_legacy_bfasl_remains_read_compatible() {
    let dir = workdir("legacy-source");
    let out = dir.join("legacy.bfasl");
    let image = egcl_rt::bfasl::BfaslBuilder::new()
        .section(
            egcl_rt::bfasl::section::TOPLEVEL_FORMS,
            b"(defun legacy-bfasl-value () 73)".to_vec(),
        )
        .build();
    fs::write(&out, image).unwrap();

    let loaded = run(&format!(
        "(progn (load \"{}\") (legacy-bfasl-value))",
        out.display()
    ));
    assert!(
        loaded.status.success(),
        "legacy source-only artifact should remain readable: {}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&loaded.stdout).trim(), "73");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn compiled_capturing_closure_is_a_function_for_typep_and_functionp() {
    // bliss-aid: a capturing flet/labels closure compiled to bytecode (bliss-1ja)
    // is a heap interpreted-function object — callable, but FUNCTIONP / (typep x
    // 'function) used to return NIL for it (is_function_value only knew TAG_FUNCTION
    // values and (EGCL::CLOSURE . id) conses). UIOP's ENSURE-FUNCTION dispatches
    // on (etypecase fun (function ...) ...), so such a closure fell through every
    // clause and crashed ASDF's source-registry scan with "ETYPECASE: no clause
    // matched". Compile-file forces the bytecode representation the REPL's
    // interpreted closures don't exhibit.
    let dir = workdir("capturing-closure-functionp");
    let src = dir.join("cc.lisp");
    let out = dir.join("cc.bfasl");
    // make-adder returns a *capturing* local function; the enclosing defun is
    // compiled, so #'adder is the compiled heap closure representation.
    fs::write(
        &src,
        "(defun make-adder (n) (flet ((adder (x) (+ x n))) #'adder))\n",
    )
    .unwrap();

    let prog = format!(
        "(progn (compile-file #p\"{src}\" :output-file #p\"{out}\") (load \"{out}\") \
           (let ((f (make-adder 10))) \
             (format t \"RESULT[~a ~a ~a]\" (functionp f) (typep f 'function) (funcall f 5))))",
        src = src.display(),
        out = out.display(),
    );
    let r = run(&prog);
    let stdout = String::from_utf8_lossy(&r.stdout);
    assert!(
        r.status.success(),
        "compile+load+check failed: {stdout}\n{}",
        String::from_utf8_lossy(&r.stderr),
    );
    // compile-file prints progress chatter to stdout, so match the marker.
    assert!(
        stdout.contains("RESULT[T T 15]"),
        "compiled capturing closure must be FUNCTIONP and (typep _ 'function); \
         got: {stdout}, stderr: {}",
        String::from_utf8_lossy(&r.stderr),
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn bfasl_setf_writer_dispatches_from_top_level_setf() {
    // bliss-d0b: a `(defun (setf place) …)` writer compiled into a .bfasl is
    // installed on the mangled EGCL-INTERNAL %SETF-WRITER-place symbol. When a
    // FRESH process loads that .bfasl it lands in the symbol's function cell as an
    // interpreted function (not the bytecode registry). A subsequent tree-walked
    // `(setf (place …) v)` must still find and call it — the SETF store path used
    // to gate on the bytecode registry and look the symbol up via the reader
    // (which normalises PKG::NAME to PKG:NAME, a different symbol), so it failed
    // with "SETF: unsupported place". This is how babel's
    // `(setf (get-abstract-mapping …))` broke under `asdf:load-system`.
    let dir = workdir("bfasl-setf-writer");
    let src = dir.join("w.lisp");
    let out = dir.join("w.bfasl");
    fs::write(
        &src,
        "(defparameter *h* (make-hash-table))\n\
         (defun gm (e) (gethash e *h*))\n\
         (defun (setf gm) (v e) (setf (gethash e *h*) v))\n",
    )
    .unwrap();

    let compile = run(&format!(
        "(compile-file #p\"{}\" :output-file #p\"{}\")",
        src.display(),
        out.display()
    ));
    assert!(compile.status.success(), "compile-file failed");

    // Fresh process: load the .bfasl, then a TOP-LEVEL (tree-walked) setf.
    let loaded = run(&format!(
        "(progn (load \"{}\") (setf (gm :y) 99) (format t \"R[~A]\" (gm :y)))",
        out.display()
    ));
    let stdout = String::from_utf8_lossy(&loaded.stdout);
    assert!(
        loaded.status.success(),
        "top-level setf of a bfasl-loaded writer failed: {stdout}\n{}",
        String::from_utf8_lossy(&loaded.stderr),
    );
    assert!(
        stdout.contains("R[99]"),
        "expected R[99]; got: {stdout}, stderr: {}",
        String::from_utf8_lossy(&loaded.stderr),
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn asdf_bfasl_load_asd_runs_under_bytecode() {
    // bliss-a27: ASDF loaded from the bundled .bfasl must behave the same when
    // called by the bytecode backend as it does under the tree-walker. This
    // explicit LOAD-ASD path is the smallest stable slice of the Babel/OCICL
    // corruption: it fails before system lookup needs the user OCICL runtime.
    let root = repo_root();
    // SELF-CONTAINED, deliberately. This used to read `lib/asdf.bfasl` and an
    // ocicl-fetched `babel.asd`, neither of which is tracked — .gitignore excludes
    // `*.bfasl` and `ocicl/`, and neither has ever been in git. So it asserted a
    // working tree only a provisioned machine has, and failed on every clean
    // checkout, taking the `test` and `test-ffi-gnu` CI jobs down with a missing
    // FILE rather than a broken loader (bliss-d3smh).
    //
    // Both inputs are now built from tracked sources, so the test runs everywhere
    // instead of being skipped or red: ASDF is compiled from `lib/asdf.lisp` (~7s)
    // into the test's own directory, and the .asd it loads is this repository's
    // own `egcl-jvm.asd`. The bliss-a27 claim is unchanged — ASDF loaded from a
    // BFASL must behave under the bytecode backend as it does under the
    // tree-walker — and compiling it here exercises that bfasl being produced as
    // well as consumed.
    let dir = workdir("asdf-load-asd");
    let asdf_src = root.join("lib/asdf.lisp");
    let asdf = dir.join("asdf.bfasl");
    let target_asd = root.join("lib/egcl-jvm/egcl-jvm.asd");
    assert!(asdf_src.is_file(), "tracked source missing: {}", asdf_src.display());
    assert!(target_asd.is_file(), "tracked asd missing: {}", target_asd.display());

    let compiled = run(&format!(
        "(compile-file {asdf_src:?} :output-file {asdf:?})"
    ));
    assert!(
        compiled.status.success(),
        "compiling ASDF failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    assert!(asdf.is_file(), "no bfasl produced at {}", asdf.display());

    // Separate top-level forms, NOT a progn: `asdf:load-asd` must be READ after
    // ASDF exists, and one form is read in full before any of it is evaluated.
    let form = format!(
        "#-asdf (load #P\"{asdf}\") \
         (asdf:load-asd #P\"{target_asd}\" :name \"egcl-jvm\") \
         (format t \"LOAD-ASD-OK\")",
        asdf = asdf.display(),
        target_asd = target_asd.display()
    );
    let output = Command::new(BIN)
        .env("EGCL_T1_THRESHOLD", "999999999")
        .args(["--no-init", "--eval", &form])
        .output()
        .expect("spawn egcl");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "ASDF bfasl LOAD-ASD failed under bytecode\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("LOAD-ASD-OK"),
        "missing success marker\nstdout: {stdout}\nstderr: {stderr}"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// A compiled `(setf (slot-value o 's) v)` must lower to the internal store
/// primitive, not to a call to the bootstrap's `(setf slot-value)` writer.
///
/// boot.lisp defines `(defun (setf slot-value) …)` so that `#'(setf slot-value)`
/// is a real function designator (bliss-6buay). The lowerer probed
/// `user_setf_writer_place` BEFORE its own direct `SET-SLOT-VALUE` arm, so that
/// definition silently converted every compiled slot store into a `CallNamed`
/// on `EGCL-INTERNAL::%SETF-WRITER-SLOT-VALUE` — a function only the bootstrap
/// defines. The .bfasl then failed to load under `--no-bootstrap` with
/// "undefined function" (bliss-42oty).
///
/// Both halves matter. The byte check pins the lowering DECISION, so reordering
/// the place-dispatch chain again fails here for the right reason; the load
/// proves the consequence the user actually saw.
#[test]
fn compiled_slot_value_store_does_not_call_the_bootstrap_setf_writer() {
    let dir = workdir("slot-value-store");
    let src = dir.join("store.lisp");
    let out = dir.join("store.bfasl");
    fs::write(
        &src,
        r#"
      (defclass holder () ((item :initarg :item)))
      (defun holder-store (x v) (setf (slot-value x 'item) v))
      (let ((o (make-instance 'holder :item nil)))
        (format t "STORED ~S~%" (list (holder-store o 42) (slot-value o 'item))))
    "#,
    )
    .unwrap();
    let compiled = run(&format!("(compile-file {:?} :output-file {:?})", src, out));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let bytes = fs::read(&out).unwrap();
    let needle = b"%SETF-WRITER-SLOT-VALUE";
    assert!(
        !bytes.windows(needle.len()).any(|w| w == needle),
        "the .bfasl names the bootstrap writer, so the place took the writer \
         call instead of the direct EGCL::SET-SLOT-VALUE store"
    );

    // Source-free, and with no bootstrap to supply the writer even if it were
    // referenced.
    fs::remove_file(&src).unwrap();
    let loaded = Command::new(BIN)
        .args(["--no-init", "--no-bootstrap", "--load"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        loaded.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&loaded.stdout).trim(),
        "STORED (42 42)"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// A macro's `&environment` must describe the CALL SITE, whether the macro was
/// loaded from source or from a `.bfasl`.
///
/// A compiled expander ran through a registered callback that built a fresh
/// `Env` for the expansion and discarded the caller's environment, so
/// `(macroexpand sym env)` inside it could not see an enclosing
/// SYMBOL-MACROLET. The same macro then answered differently depending on how it
/// had been loaded — source said the symbol expanded, `.bfasl` said it did not
/// (bliss-1pve). SBCL answers as the source case does.
///
/// The third probe is the one that matters: it returns the EXPANSION rather than
/// a flag, and before the fix it came back as the bare symbol `B` instead of
/// `(+ 2 3)` — a macro emitting silently wrong code, with nothing to signal it.
/// The fourth is the negative control: a symbol with no binding must still be
/// reported as unexpanded, so a fix cannot pass by answering "known" always.
#[test]
fn a_compiled_macros_environment_sees_the_call_sites_symbol_macrolet() {
    let dir = workdir("macro-environment");
    let macros = dir.join("menv-macros.lisp");
    let fasl = dir.join("menv-macros.bfasl");
    let user = dir.join("menv-user.lisp");
    fs::write(
        &macros,
        r#"
      (defmacro menv-knows-sym (s &environment env)
        (multiple-value-bind (x p) (macroexpand s env)
          (declare (ignore x))
          (if p :known :unknown)))
      (defmacro menv-expands-to (s &environment env)
        (list 'quote (macroexpand s env)))
    "#,
    )
    .unwrap();
    fs::write(
        &user,
        r#"
      (defun menv-p1 () (symbol-macrolet ((a 1)) (menv-knows-sym a)))
      (defun menv-p2 () (symbol-macrolet ((b (+ 2 3))) (menv-expands-to b)))
      (defun menv-p3 () (menv-knows-sym never-bound))
      (format t "MENV ~A ~S ~A~%" (menv-p1) (menv-p2) (menv-p3))
    "#,
    )
    .unwrap();

    let compiled = run(&format!(
        "(compile-file {macros:?} :output-file {fasl:?})"
    ));
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    // Source and .bfasl must agree, and must agree on the RIGHT answer.
    for (label, loaded) in [("source", &macros), ("bfasl", &fasl)] {
        let out = Command::new(BIN)
            .args([
                "--eval",
                &format!("(load {loaded:?})"),
                "--eval",
                &format!("(load {user:?})"),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{label} load failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("MENV KNOWN (+ 2 3) UNKNOWN"),
            "{label}: &environment did not describe the call site\n{stdout}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hot_nested_defun_preserves_lexical_captures() {
    let dir = workdir("hot-nested-defun");
    let src = dir.join("capture.lisp");
    let out = dir.join("capture.bfasl");
    fs::write(
        &src,
        "(let ((captured 42) (other 7) (counter 0))
           (defun hot-capture (&optional use-other)
             (flet ((inner () other))
               (incf counter)
               (list (if use-other (inner) captured) counter))))",
    )
    .unwrap();
    let exercise = "(let ((captured 999) (other 888) (counter -1))
                      (declare (ignorable captured other counter))
                      (dotimes (i 500) (hot-capture))
                      (list (hot-capture) (hot-capture t)))";
    for input in [&src, &out] {
        if input == &out {
            let compiled = run(&format!(
                "(compile-file {:?} {:?})",
                src.to_str().unwrap(),
                out.to_str().unwrap()
            ));
            assert!(
                compiled.status.success(),
                "{}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            assert!(bfasl_section(&fs::read(&out).unwrap(), 11).is_none());
            fs::remove_file(&src).unwrap();
        }
        let result = run(&format!(
            "(progn (load {:?}) {exercise})",
            input.to_str().unwrap()
        ));
        assert!(
            result.status.success(),
            "{}: {}",
            input.display(),
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stdout).trim(),
            "((42 501) (7 502))"
        );
    }
    let _ = fs::remove_dir_all(dir);
}
