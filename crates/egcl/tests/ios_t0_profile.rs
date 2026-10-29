// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Run through an application-hosted iOS test harness, not process spawning.
//! Future matrix: inherit EGCL_FORCE_TIER=t2 and low promotion/OSR thresholds;
//! repeat with post-init GC stress and poison. No environment is changed here.
#![cfg(all(target_os = "ios", target_arch = "aarch64"))]

#[test]
fn source_loading_and_hot_lisp_remain_in_the_t0_profile() {
    struct SourceFile(std::path::PathBuf);
    impl Drop for SourceFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let source = SourceFile(
        std::env::temp_dir().join(format!("egcl-ios-profile-{}.lisp", std::process::id())),
    );
    std::fs::write(&source.0, "(defun ios-loaded-source (x) (+ x 2))")
        .expect("application source fixture");
    // Common Lisp strings escape backslashes and quotes, rather than JSON's
    // Unicode/control escape convention. Preserve all other pathname chars.
    let source_path = source
        .0
        .to_str()
        .expect("UTF-8 source path")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let worker = std::thread::Builder::new()
        .name("egcl-ios-profile-test".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            egcl::run(&[
                "egcl".into(),
                "--no-init".into(),
                "--eval".into(),
                format!(
                    r#"(progn
                    (load "{}")
                    (assert (member :ios *features*))
                    (assert (member :darwin *features*))
                    (assert (not (member :linux *features*)))
                    (defun ios-profile-hot (n)
                      (let ((sum 0)) (dotimes (i n sum) (incf sum i))))
                    (dotimes (i 80) (assert (= (ios-profile-hot 1000) 499500)))
                    (assert (= (egcl-ext:function-tier 'ios-profile-hot) 0))
                    (assert (= (ios-loaded-source 40) 42))
                    (assert (= (funcall (compile nil '(lambda (x) (+ x 2))) 40) 42))
                    (assert (= (eval (read-from-string "(+ 20 22)")) 42)))"#,
                    source_path
                ),
            ])
        })
        .expect("Lisp worker");
    assert_eq!(
        worker
            .join()
            .expect("Lisp worker returned")
            .expect("Lisp run succeeded"),
        0
    );
}
