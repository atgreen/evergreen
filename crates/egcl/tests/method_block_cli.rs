use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const METHODS: &str = r#"
  (defmethod primary-return ((x t)) (return-from primary-return (values x :primary)))
  (defmethod (setf writer-return) (value (x t))
    (return-from writer-return (values value :writer)))
  (defmethod local-return ((x t))
    (flet ((escape () (return-from local-return (list x)))) (escape)))
  (defmethod shadow-return ((x t))
    (block shadow-return (return-from shadow-return :inner))
    (return-from shadow-return :outer))
  (defmethod macro-return ((x t))
    (macrolet ((finish-method () '(return-from macro-return :macro))) (finish-method)))
  (defmethod combined-return ((x t)) (return-from combined-return (list x)))
  (defmethod combined-return :around ((x t))
    (return-from combined-return (cons :around (call-next-method))))
  (format t "METHODS ~S~%"
    (list (multiple-value-list (primary-return 42))
          (setf (writer-return nil) 7)
          (local-return :nested) (shadow-return nil) (macro-return nil)
          (combined-return 5)))
"#;

#[test]
fn methods_establish_their_generic_function_block() {
    for (tier, interpreted_methods, stress) in [
        ("t0", true, false),
        ("t0", false, false),
        ("t1", false, false),
        ("t0", true, true),
        ("t1", false, true),
    ] {
        let mut command = Command::new(BIN);
        command.args(["--no-init", "--no-bootstrap", "--eval", METHODS]);
        command.env("EGCL_FORCE_TIER", tier);
        command.env_remove("EGCL_NO_METHOD_COMPILE");
        if interpreted_methods {
            command.env("EGCL_NO_METHOD_COMPILE", "1");
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_VERIFY", "1");
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "tier={tier}, interpreted={interpreted_methods}, stress={stress}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("METHODS ((42 :PRIMARY) 7 (:NESTED) :OUTER :MACRO (:AROUND 5))"),
            "{stdout}"
        );
    }
}
