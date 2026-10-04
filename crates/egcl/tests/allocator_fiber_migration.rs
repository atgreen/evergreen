// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The musl Rust allocation cache must follow the carrier after a fiber moves.
//! The regression is release-sensitive: inlining once allowed cached TLS
//! addresses to survive a yielding caller and corrupt another carrier's bins.
#![cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]

#[test]
fn allocation_cache_survives_two_carrier_tree_walker_execution() {
    let source = std::env::temp_dir().join(format!(
        "egcl-allocator-fiber-migration-{}.lisp",
        std::process::id()
    ));
    std::fs::write(
        &source,
        r#"
      (handler-bind
          ((egcl-fiber:fiber-error
             (lambda (condition)
               (format t "FIBER-CAUSE: ~S~%" (egcl-fiber:fiber-error-cause condition))
               (finish-output))))
        (defvar *fiber-value* :outside)
        (let* ((fibers (loop for n below 8 collect
                        (let ((n n))
                          (egcl-fiber:make-fiber
                            (lambda ()
                              (let ((*fiber-value* n))
                                (block done
                                  (unwind-protect
                                      (return-from done (list *fiber-value* :returned))
                                    (egcl-fiber:fiber-yield)
                                    (assert (= n *fiber-value*)))))))))))
          (assert (equal (loop for n below 8 collect (list n :returned))
                         (egcl-fiber:run-fibers fibers :carrier-count 2)))
          (assert (eq :outside *fiber-value*)))
        (format t "ALLOCATOR-FIBER-OK~%"))
    "#,
    )
    .unwrap();
    // Fresh processes exercise both cold compilation and carrier-local cache
    // initialization. A single scheduling interleaving need not expose the bug.
    for attempt in 0..8 {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--load"])
            .arg(&source)
            .env("EGCL_BACKEND", "tree-walker")
            .env_remove("EGCL_FORCE_TIER")
            .env_remove("EGCL_TIME_SLICE_US")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "attempt {attempt}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("ALLOCATOR-FIBER-OK"));
    }
    std::fs::remove_file(source).unwrap();
}
