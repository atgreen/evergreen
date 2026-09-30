//! INITIALIZE-INSTANCE and SHARED-INITIALIZE receive the initargs the caller
//! supplied, under the names the caller used (CLHS 7.1.2). MAKE-INSTANCE keyed
//! them by SLOT NAME instead, so a method's `&key components` saw NIL whenever
//! the initarg named a slot — iolib's FILE-PATH checks its :components in an
//! :after method and found nothing (bliss-vhr6e).
use std::process::Command;

fn eval(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("RESULT:"))
        .unwrap_or_else(|| panic!("no RESULT: line in output:\n{stdout}\n{stderr}"))
        .trim()
        .to_string()
}

#[test]
fn an_after_method_sees_an_initarg_that_names_a_slot() {
    // Every answer here is SBCL's on the same program.
    assert_eq!(
        eval(
            r#"(progn (defclass p () ((components :initarg :components :initform nil)))
                      (defmethod initialize-instance :after ((o p) &rest initargs &key components
                                                             &allow-other-keys)
                        (format t "RESULT:~s" (list initargs components)))
                      (make-instance 'p :components '(1 2)))"#
        ),
        "((:COMPONENTS (1 2)) (1 2))"
    );
}

#[test]
fn the_initarg_name_is_what_the_caller_used_not_the_slot_name() {
    // A slot whose :initarg differs from its name: the method's parameter is
    // named after the INITARG, which is what must arrive.
    assert_eq!(
        eval(
            r#"(progn (defclass q () ((slotname :initarg :argname :initform :unset)))
                      (defmethod initialize-instance :after ((o q) &key argname)
                        (format t "RESULT:~s" (list argname (slot-value o 'slotname))))
                      (make-instance 'q :argname 7))"#
        ),
        "(7 7)"
    );
}

#[test]
fn shared_initialize_sees_them_too_and_a_default_initarg_arrives_by_name() {
    assert_eq!(
        eval(
            r#"(progn (defclass r () ((x :initarg :x :initform :unset))
                        (:default-initargs :x 5))
                      (defmethod shared-initialize :after ((o r) slots &key x &allow-other-keys)
                        (declare (ignore slots))
                        (format t "RESULT:~s" (list x (slot-value o 'x))))
                      (make-instance 'r))"#
        ),
        "(5 5)"
    );
}

#[test]
fn an_initarg_with_no_slot_still_arrives() {
    // CLHS 7.1.2: an initarg is valid if an applicable method declares it, even
    // with no slot behind it. This already worked; it must keep working.
    assert_eq!(
        eval(
            r#"(progn (defclass s () ((kept :initarg :kept :initform nil)))
                      (defmethod initialize-instance :after ((o s) &key kept extra)
                        (format t "RESULT:~s" (list kept extra)))
                      (make-instance 's :kept 1 :extra 2))"#
        ),
        "(1 2)"
    );
}

#[test]
fn a_method_on_a_superclass_sees_the_subclass_instance_initargs() {
    // iolib's exact shape: the :after method is on the superclass, the instance
    // is of the subclass, and the initarg names a slot of the superclass.
    assert_eq!(
        eval(
            r#"(progn (defclass base () ((components :initarg :components :initform nil)))
                      (defclass derived (base) ())
                      (defmethod initialize-instance :after ((o base) &key components)
                        (check-type components (and (not null) list))
                        (format t "RESULT:~s" components))
                      (make-instance 'derived :components '(:root "a")))"#
        ),
        "(:ROOT \"a\")"
    );
}
