use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_torcl");

#[test]
fn executable_advertises_torcl() {
    for flag in ["--help", "--version"] {
        let output = Command::new(BIN).arg(flag).output().unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        if flag == "--help" {
            assert!(stdout.contains("TorCL"), "{stdout}");
            assert!(stdout.contains("Usage: torcl"), "{stdout}");
        } else {
            assert!(stdout.starts_with("torcl "), "{stdout}");
        }
    }
}

#[test]
fn lisp_exposes_the_torcl_implementation_and_extension_packages() {
    let output = Command::new(BIN)
        .args([
            "--no-init",
            "--eval",
            r#"
          (format t "IDENTITY ~S~%" (lisp-implementation-type))
          (format t "FEATURE ~S~%" (not (null (member :torcl *features*))))
          (format t "PACKAGES ~S~%"
            (mapcar (lambda (name) (not (null (find-package name))))
                    '("TORCL-INTERNAL" "TORCL-EXT" "TORCL-GRAY-STREAMS")))
        "#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for expected in ["IDENTITY \"TorCL\"", "FEATURE T", "PACKAGES (T T T)"] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
}
