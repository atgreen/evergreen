//! End-to-end `.bfasl` compile/load through the CLI (bliss-lb6.6, spec §6.11):
//! compile a source file to a `.bfasl`, load it in a *fresh* process, and verify
//! version-mismatch rejection.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_bliss-cli");

fn workdir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("bliss-bfasl-{}-{}", std::process::id(), tag));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn run(program: &str) -> std::process::Output {
    Command::new(BIN)
        .args(["--eval", program])
        .output()
        .expect("spawn bliss-cli")
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
            2 => pos += 8,
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
            13 => pos += 8,
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

/// FNV-1a low-32 checksum over `bytes`, matching `bliss_rt::bfasl`'s framing.
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
        !bytes.windows(source.len()).any(|window| window == source.as_bytes()),
        "the artifact must not retain its source text"
    );
    let (_, function_count, load_action_count) = bbu_counts(&bytes);
    assert!(function_count > 0, "BYTECODE_UNIT contains bytecode functions");
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
        "(eval-when (:compile-toplevel) (defparameter +cf-read+ 12))
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
    assert!(function_count >= 2, "expected both functions in BYTECODE_UNIT");
    assert!(load_action_count >= 2, "expected load actions for both functions");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn compile_file_round_trips_define_package_without_source() {
    let dir = workdir("define-package");
    let src = dir.join("p.lisp");
    let out = dir.join("p.bfasl");
    let source = "(define-package :bf/pkg (:nicknames :bf-pkg) (:use :common-lisp) (:export #:pkg-value))
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

    let image = bliss_rt::bfasl::load(&fs::read(&good).unwrap()).unwrap();
    let mut builder = bliss_rt::bfasl::BfaslBuilder::new();
    for (kind, section) in image.sections() {
        let mut section = section.to_vec();
        if kind == bliss_rt::bfasl::section::BYTECODE_UNIT {
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
fn macro_and_compiler_macro_expanders_round_trip_as_bytecode() {
    let dir = workdir("macro-bytecode");
    let src = dir.join("macros.lisp");
    let out = dir.join("macros.bfasl");
    let source = "(defmacro bbu-twice (x) (list '+ x x))
                  (defun bbu-use-twice (x) (bbu-twice x))
                  (defun bbu-cm-target (x) (+ x 1))
                  (define-compiler-macro bbu-cm-target (x) (list '+ x 10))
                  (defun bbu-cm-compiled () (bbu-cm-target 5))\n";
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
           (list (bbu-twice 9)
                 (bbu-use-twice 7)
                 (bbu-cm-compiled)
                 (bbu-cm-later)))",
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
        "(18 14 15 12)"
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
    let image = bliss_rt::bfasl::BfaslBuilder::new()
        .section(
            bliss_rt::bfasl::section::BYTECODE_UNIT,
            b"BBU\0".to_vec(),
        )
        .section(
            bliss_rt::bfasl::section::TOPLEVEL_FORMS,
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
    let image = bliss_rt::bfasl::BfaslBuilder::new()
        .section(
            bliss_rt::bfasl::section::TOPLEVEL_FORMS,
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
