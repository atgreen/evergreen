// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::{METHOD_COMPILED, METHOD_CONTEXT_UNUSED};
use method_context_use::helper_values;

fn read(env: &mut Env, source: &str) -> EgclVal {
    crate::cli::read_eval_all_env(source, env).unwrap()
}

#[test]
fn typed_writer_removes_two_actual_helper_allocations_and_preserves_native_callbacks() {
    // Full prelude fixtures need fresh processes: package resets intentionally
    // do not erase global function cells retained by another test's bootstrap.
    let filter = "cli::bytecode::method_helper_elision_tests::typed_writer_removes_two_actual_helper_allocations_and_preserves_native_callbacks";
    if std::env::var("EGCL_HELPER_ELISION_CHILD").ok().as_deref() != Some(filter) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", filter, "--nocapture"])
            .env("EGCL_HELPER_ELISION_CHILD", filter)
            .output()
            .expect("isolated helper-elision fixture");
        let stdout = String::from_utf8_lossy(&output.stdout);
        print!("{stdout}");
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", output.status);
        assert!(
            stdout.contains("running 1 test")
                && stdout.contains("test result: ok. 1 passed; 0 failed;")
        );
        return;
    }
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::BOOT_COMPLETE.with(|ready| ready.set(false));
    read(&mut env, crate::cli::EMBEDDED_BOOT_LISP);
    crate::cli::BOOT_COMPLETE.with(|ready| ready.set(true));
    read(
        &mut env,
        "(in-package :cl-user)
      (defstruct elision-cell (payload 0 :type (unsigned-byte 32)))
      (defparameter *elision-cell* (make-elision-cell))
      (defparameter *elision-around*
        (defmethod (setf elision-cell-payload) :around (value (cell elision-cell))
          (call-next-method)))",
    );
    egcl_rt::rooted!(writer = read(&mut env, "'(setf elision-cell-payload)"));
    let writer_key = crate::cli::function_name_key(*writer);
    let id = env.methods.borrow()[&writer_key]
        .iter()
        .find(|method| method.qualifier == egcl_stdlib::MethodQualifier::Primary)
        .unwrap()
        .method_id;
    egcl_rt::rooted!(callable = METHOD_COMPILED.borrow()[&id.0]);
    assert!(!METHOD_CONTEXT_UNUSED.borrow().contains_key(&id.0));
    let private = egcl_rt::function::definition_index(*callable);
    let (_, body) = bytecode_for_callable(private, *callable).unwrap();
    assert_eq!(body.nested_functions.len(), 2);
    assert!(matches!(body.code[0], Instr::CallNamed { nargs: 0, .. }));
    assert!(
        matches!(body.code[3], Instr::Const(_)) && matches!(body.code[4], Instr::Const(_)),
        "actual generated writer must elide exactly the two helper sites: {:?}",
        body.code
    );
    assert!(
        !body
            .code
            .iter()
            .any(|i| matches!(i, Instr::MakeClosure { .. }))
    );
    let checker = reader::find_symbol_index("EGCL::%CHECKED-STRUCTURE-SLOT-VALUE").unwrap();
    assert!(
        body.code
            .iter()
            .any(|i| matches!(i,Instr::CallNamed{sym,nargs:3} if *sym==checker)),
        "mutable actual-slot type checker must remain"
    );
    let _observe = helper_values::observe(&body.nested_functions);
    struct RestoreBody(u32, Arc<BytecodeFunction>);
    impl Drop for RestoreBody {
        fn drop(&mut self) {
            registry_put(self.0, Arc::clone(&self.1));
        }
    }
    // Counterfactual restores only the two original allocations in a clone;
    // dynamic context/checker/store and nested templates are identical.
    let restore = RestoreBody(private, Arc::clone(&body));
    let mut original = (*body).clone();
    original.code[3] = Instr::MakeClosure {
        func: 0,
        capture_env: true,
    };
    original.code[4] = Instr::MakeClosure {
        func: 1,
        capture_env: true,
    };
    registry_put(private, Arc::new(original));
    let before = helper_values::creations();
    read(
        &mut env,
        "(assert (= (funcall #'(setf elision-cell-payload) 7 *elision-cell*) 7))",
    );
    assert_eq!(
        helper_values::creations() - before,
        2,
        "actual allocation-site witness must be live"
    );
    drop(restore);
    let before = helper_values::creations();
    read(
        &mut env,
        "(assert (= (funcall #'(setf elision-cell-payload) 11 *elision-cell*) 11))
                    (assert (= (elision-cell-payload *elision-cell*) 11))",
    );
    assert_eq!(
        helper_values::creations(),
        before,
        "same selected templates allocate zero optimized helpers"
    );

    // Existing pinned-T0 observer proves the exact installed mapped native
    // caller via real segment links; it cannot pass in interpreter fallback.
    egcl_rt::rooted!(
        observer = crate::cli::read_next_form_at(
            &"elision-observe-entry".chars().collect::<Vec<_>>(),
            0,
            &mut env
        )
        .unwrap()
        .0
    );
    let hook = EgclVal::from_symbol_index(reader::intern_symbol("%ORDINARY-T2-ENTRY-FOR-TEST"));
    egcl_rt::rooted!(observer_body = crate::cli::vec_to_list(&[hook]));
    egcl_rt::rooted!(defun = crate::cli::resolve_sym("DEFUN").unwrap());
    egcl_rt::rooted!(
        definition = crate::cli::vec_to_list(&[*defun, *observer, NIL, *observer_body])
    );
    crate::cli::eval_form(*definition, &mut env).unwrap();
    let _pin =
        native_transfer_tests::ordinary_t2::RecursiveFixturePins::new(&[&crate::cli::sym_name(
            *observer,
        )]);
    egcl_rt::rooted!(
        params = crate::cli::read_next_form_at(&"(cell)".chars().collect::<Vec<_>>(), 0, &mut env)
            .unwrap()
            .0
    );
    egcl_rt::rooted!(
        forms = crate::cli::read_next_form_at(
            &"((elision-observe-entry) (setf (elision-cell-payload cell) 19))"
                .chars()
                .collect::<Vec<_>>(),
            0,
            &mut env
        )
        .unwrap()
        .0
    );
    let caller = reader::intern_symbol("ELISION-NATIVE-CALLER");
    let caller_body = Arc::new(
        compile_function("ELISION-NATIVE-CALLER", *params, *forms, &env, false, false)
            .unwrap_or_else(|| panic!("native writer fixture: {:?}", last_bail_reason())),
    );
    let (caller, code) = native_transfer_tests::ordinary_t2::install_body(caller, caller_body);
    assert!(code.is_t2);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped native caller");
    };
    assert!(!mapped.needs_recompile());
    egcl_rt::rooted!(cell = read(&mut env, "*elision-cell*"));
    native_transfer_tests::ordinary_t2::expect_entry(&code);
    let deopts = deopt_count();
    let before = helper_values::creations();
    assert_eq!(
        run_native(&code, caller, &[*cell], &mut env).unwrap(),
        EgclVal::from_fixnum(19)
    );
    assert_eq!(deopt_count(), deopts);
    assert_eq!(helper_values::creations(), before);
    read(
        &mut env,
        "(assert (= (elision-cell-payload *elision-cell*) 19))
      (defvar *elision-callbacks* 0)
      (defvar *elision-base-calls* 0)
      (defmethod (setf elision-cell-payload) (value (cell t))
        (declare (ignore cell)) (incf *elision-base-calls*) value)
      (defun elision-check-callback (value cell slot)
        (declare (ignore cell slot))
        (elision-observe-entry)
        (incf *elision-callbacks*)
        (let ((context (egcl-internal::%method-context)))
          (assert (egcl-internal::%method-next-p context))
          (assert (= (egcl-internal::%method-continue context nil) value)))
        value)",
    );
    egcl_rt::rooted!(old_checker = egcl_rt::symbols::symbol_function(checker).unwrap());
    assert!(egcl_rt::function::is_interpreted_function(*old_checker));
    // The guard reads the still-live registered root on drop, never an
    // unscanned value copy or a shared reference held across moving GC.
    // Direct RT cell replacement leaves Env's definition metadata unchanged.
    struct RestoreCell(u32, *const EgclVal);
    impl Drop for RestoreCell {
        fn drop(&mut self) {
            // Every guard is dropped before its stable rooted backing local.
            egcl_rt::symbols::set_symbol_function(self.0, unsafe { *self.1 });
        }
    }
    egcl_rt::rooted!(replacement = read(&mut env, "#'elision-check-callback"));
    let restore = RestoreCell(checker, &*old_checker);
    egcl_rt::symbols::set_symbol_function(checker, *replacement);
    // Install the selected method's exact immutable body, leaving its callable
    // cell unchanged. The checker calls the pinned observer before any other
    // callback, proving the affected method itself is the physical native owner.
    let (_, method_native) =
        native_transfer_tests::ordinary_t2::install_body(private, Arc::clone(&body));
    assert!(method_native.is_t2);
    native_transfer_tests::ordinary_t2::expect_entry(&method_native);
    let deopts = deopt_count();
    let before = helper_values::creations();
    read(
        &mut env,
        "(assert (= (funcall #'(setf elision-cell-payload) 23 *elision-cell*) 23))
      (assert (= *elision-callbacks* 1)) (assert (= *elision-base-calls* 1))
      (assert (= (elision-cell-payload *elision-cell*) 23))",
    );
    // The callback counter is incremented only after the physical-owner hook
    // returned successfully, so the assertion above also proves it ran.
    assert_eq!(deopt_count(), deopts);
    assert_eq!(helper_values::creations(), before);
    drop(restore);
    read(
        &mut env,
        "(remove-method #'(setf elision-cell-payload) *elision-around*)
      (defvar *elision-initializers* 0)
      (defun elision-open-values (mode)
        (cond ((eq mode :zero) (values))
              ((eq mode :single) mode)
              (t (values mode :body-secondary))))
      (defgeneric elision-values (mode))
      (defmethod elision-values ((mode t)) (elision-open-values mode))",
    );
    let Instr::CallNamed {
        sym: initializer,
        nargs: 0,
    } = body.code[0]
    else {
        unreachable!()
    };
    egcl_rt::rooted!(old_initializer = egcl_rt::symbols::symbol_function(initializer).unwrap());
    assert_eq!(*old_initializer, egcl_rt::value::UNBOUND);
    egcl_rt::rooted!(
        replacement = read(
            &mut env,
            "#'(lambda () (incf *elision-initializers*) (values nil :secondary))"
        )
    );
    let restore = RestoreCell(initializer, &*old_initializer);
    egcl_rt::symbols::set_symbol_function(initializer, *replacement);
    read(
        &mut env,
        "(assert (= (funcall #'(setf elision-cell-payload) 29 *elision-cell*) 29))
      (assert (= *elision-initializers* 1))
      (assert (= (elision-cell-payload *elision-cell*) 29))
      (assert (null (multiple-value-list (elision-values :zero))))
      (assert (equal (multiple-value-list (elision-values :single)) '(:single)))
      (assert (equal (multiple-value-list (elision-values :multi)) '(:multi :body-secondary)))",
    );
    drop(restore);
}
