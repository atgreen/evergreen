//! COMPILE-FILE must capture source paths, not the translated FASL location.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn workdir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "bliss-compile-pathnames-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}

fn run(program: &str, cwd: &Path, stress: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bliss-cli"));
    command
        .current_dir(cwd)
        .args(["--no-init", "--eval", program]);
    if stress {
        command
            .arg("--no-bootstrap")
            .env("BLISS_GC_STRESS", "1")
            .env("BLISS_GC_POISON", "1")
            .env("BLISS_GC_VERIFY", "1");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn read_time_source_path_survives_a_separate_fasl_directory() {
    let dir = workdir();
    let source = dir.join("source");
    fs::create_dir(&source).unwrap();
    let src = source.join("paths.lisp");
    let fasl = dir.join("cache/paths.bfasl");
    fs::write(source.join("data.txt"), "source-data\n").unwrap();
    fs::write(
        &src,
        r#"
      (defparameter *compile-capture* #.*compile-file-pathname*)
      (defparameter *true-capture* #.*compile-file-truename*)
      (defparameter *this-file*
        (load-time-value (or #.*compile-file-pathname* *load-pathname*)))
      (format t "PATHS ~S ~S ~S~%" *compile-capture* *true-capture* *load-pathname*)
      (with-open-file (input (merge-pathnames "data.txt" *this-file*))
        (format t "DATA ~A~%" (read-line input)))
    "#,
    )
    .unwrap();
    run(
        &format!("(compile-file {:?} :output-file {:?})", src, fasl),
        &dir,
        false,
    );
    fs::remove_file(&src).unwrap();
    for stress in [false, true] {
        let loaded = run(&format!("(load {:?})", fasl), &dir, stress);
        assert!(
            loaded.contains(&format!("PATHS #P{:?} #P{:?} #P{:?}", src, src, fasl)),
            "{loaded}"
        );
        assert!(loaded.contains("DATA source-data"), "{loaded}");
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compile_pathnames_merge_defaults_and_restore_nested_bindings() {
    let dir = workdir();
    let source = dir.join("source");
    fs::create_dir(&source).unwrap();
    let inner = source.join("inner.lisp");
    let outer = source.join("outer.lisp");
    let bad = source.join("bad.lisp");
    fs::write(
        &inner,
        "(eval-when (:compile-toplevel) (format t \"INNER ~S~%\" *compile-file-pathname*))",
    )
    .unwrap();
    fs::write(&bad, "(defun path-probe () 42)").unwrap();
    fs::write(
        &outer,
        format!(
            r#"
      (eval-when (:compile-toplevel)
        (compile-file {:?})
        (format t "OUTER ~S ~S~%" *compile-file-pathname* *compile-file-truename*))
    "#,
            inner
        ),
    )
    .unwrap();
    let program = format!(
        r#"
      (let ((*default-pathname-defaults* #P"{}/")
            (*compile-file-pathname* #P"caller.lisp")
            (*compile-file-truename* #P"caller-true.lisp"))
        (declare (special *default-pathname-defaults* *compile-file-pathname*
                          *compile-file-truename*))
        (compile-file "outer.lisp")
        (format t "RESTORED ~S ~S~%" *compile-file-pathname* *compile-file-truename*)
        (format t "ERROR-RESULT ~S~%"
          (handler-case (compile-file {:?} :output-file {:?}) (file-error () :caught)))
        (format t "ERROR-RESTORED ~S ~S~%" *compile-file-pathname* *compile-file-truename*))
    "#,
        source.display(),
        bad,
        inner.join("blocked.fasl")
    );
    for stress in [false, true] {
        let output = run(&program, &dir, stress);
        assert!(output.contains(&format!("INNER #P{:?}", inner)), "{output}");
        assert!(
            output.contains(&format!("OUTER #P{:?} #P{:?}", outer, outer)),
            "{output}"
        );
        assert!(
            output.contains("RESTORED #P\"caller.lisp\" #P\"caller-true.lisp\""),
            "{output}"
        );
        assert!(
            output.contains("ERROR-RESTORED #P\"caller.lisp\" #P\"caller-true.lisp\""),
            "{output}"
        );
        assert!(output.contains("ERROR-RESULT :CAUGHT"), "{output}");
    }
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn compile_pathname_keeps_symlink_spelling_but_truename_resolves_it() {
    let dir = workdir();
    let source = dir.join("source.lisp");
    let alias = dir.join("alias.lisp");
    fs::write(&source, "(eval-when (:compile-toplevel) (format t \"LINK ~S ~S~%\" *compile-file-pathname* *compile-file-truename*))").unwrap();
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    let output = run(&format!("(compile-file {:?})", alias), &dir, false);
    assert!(
        output.contains(&format!("LINK #P{:?} #P{:?}", alias, source)),
        "{output}"
    );
    fs::remove_dir_all(dir).unwrap();
}
