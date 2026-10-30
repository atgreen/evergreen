// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::{METHOD_COMPILED, METHOD_CONTEXT_UNUSED, METHOD_RAW_READERS, raw_slot_reader};

fn read(env: &mut Env, source: &str) -> EgclVal {
    crate::cli::read_eval_all_env(source, env).unwrap()
}

fn boot(env: &mut Env) {
    crate::cli::BOOT_COMPLETE.with(|ready| ready.set(false));
    read(env, crate::cli::EMBEDDED_BOOT_LISP);
    crate::cli::BOOT_COMPLETE.with(|ready| ready.set(true));
    read(env, "(in-package :cl-user)");
}

fn method(env: &Env, name: &str) -> EgclVal {
    let key = if name.contains(':') {
        name.to_owned()
    } else {
        let symbol = crate::cli::find_symbol_in_package(env, "CL-USER", name)
            .unwrap_or_else(|| panic!("method symbol missing: {name}"))
            .0;
        crate::cli::sym_name(symbol)
    };
    let methods = env.methods.borrow();
    methods
        .get(&key)
        .and_then(|list| list.first())
        .unwrap_or_else(|| {
            panic!(
                "method {name}, resolved key {key}; available keys {:?}",
                methods.keys().collect::<Vec<_>>()
            )
        })
        .method_id
}

fn assert_raw_proof(method: EgclVal, name: &str) {
    if METHOD_RAW_READERS.borrow().contains_key(&method.0) {
        return;
    }
    let callable = METHOD_COMPILED.borrow().get(&method.0).copied();
    let body = callable.and_then(|function| {
        bytecode_for_callable(egcl_rt::function::definition_index(function), function)
    });
    if let Some((_, body)) = body {
        panic!(
            "missing raw proof: {name}, method {method:?}, arity/min/max/variadic {:?}, params {:?}, types {:?}, code {:?}, names {:?}, constants {:?}",
            (body.arity, body.min_args, body.max_args, body.variadic),
            body.param_layout,
            body.param_types,
            body.code,
            body.names,
            body.constants
        );
    }
    panic!("missing raw proof and compiled body: {name}, method {method:?}, callable {callable:?}");
}

// Env::new resets packages/CLOS registries but deliberately retains real global
// function cells. Replaying the full boot prelude twice in one test process can
// therefore call an old finalization function while its new package is partial.
// Each case gets a fresh harness process, including after another case panics.
// The child inherits the build owner's finite cgroup and wall-time controls.
fn isolated_case(case: &str) -> bool {
    let module = module_path!().split_once("::").unwrap().1;
    let filter = format!("{module}::{case}");
    if std::env::var("EGCL_RAW_READER_TEST_CASE").ok().as_deref() == Some(filter.as_str()) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &filter, "--nocapture"])
        .env("EGCL_RAW_READER_TEST_CASE", &filter)
        .output()
        .expect("launch isolated raw-reader case");
    let stdout = String::from_utf8_lossy(&output.stdout);
    print!("{stdout}");
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "isolated {filter} failed: {}",
        output.status
    );
    assert!(
        stdout.contains("running 1 test")
            && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "isolated filter must execute exactly its intended one test: {filter}"
    );
    true
}

#[test]
fn lowered_raw_reader_preserves_helpers_and_runs_guarded_native_leaf() {
    if isolated_case("lowered_raw_reader_preserves_helpers_and_runs_guarded_native_leaf") {
        return;
    }
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    boot(&mut env);
    // Exercise actual boot DEFMETHOD, including its implicit BLOCK and
    // generated lexical continuations, rather than a synthetic source shape.
    let name = crate::cli::sym_name(read(&mut env, "'egcl-mop:slot-definition-name"));
    let boot_method = method(&env, &name);
    assert_raw_proof(boot_method, &name);
    let _mop_observation = raw_slot_reader::observe_method(boot_method);
    assert!(!METHOD_CONTEXT_UNUSED.borrow().contains_key(&boot_method.0));
    read(
        &mut env,
        "
      (defclass raw-leaf-probe () ((payload :initform 17) (other :initform 23)))
      (defmethod raw-leaf-reader ((object raw-leaf-probe))
        (egcl-internal::%raw-slot-value object 'payload))",
    );
    read(
        &mut env,
        "(egcl-mop:finalize-inheritance (find-class 'raw-leaf-probe))
         (defparameter *raw-leaf-object* (make-instance 'raw-leaf-probe))",
    );
    egcl_rt::rooted!(
        slot = read(
            &mut env,
            "(first (egcl-mop:class-slots (find-class 'raw-leaf-probe)))"
        )
    );
    egcl_rt::rooted!(payload = read(&mut env, "'payload"));
    let before = raw_slot_reader::hits();
    assert_eq!(
        crate::cli::invoke_generic_function(&name, &[*slot], &mut env).unwrap(),
        *payload
    );
    assert_eq!(
        raw_slot_reader::hits(),
        before + 1,
        "the real boot MOP reader and effective slot-definition must take the leaf"
    );
    let id = method(&env, "RAW-LEAF-READER");
    let _reader_observation = raw_slot_reader::observe_method(id);
    assert_raw_proof(id, "RAW-LEAF-READER");
    let proof = METHOD_RAW_READERS.borrow().get(&id.0).cloned().unwrap();
    egcl_rt::rooted!(callable = METHOD_COMPILED.borrow()[&id.0]);
    let (_, body) =
        bytecode_for_callable(egcl_rt::function::definition_index(*callable), *callable).unwrap();
    assert_eq!(
        body.code
            .iter()
            .filter(|i| matches!(i, Instr::MakeClosure { .. }))
            .count(),
        2,
        "the guarded fast path must not remove slow-path lexical helpers"
    );
    assert!(!METHOD_CONTEXT_UNUSED.borrow().contains_key(&id.0));
    assert!(method_no_context_body_matches(*callable, &proof.body));
    egcl_rt::rooted!(object = read(&mut env, "*raw-leaf-object*"));
    // Explicit FINALIZE-INHERITANCE always publishes a new wrapper, retiring
    // the previous one. Construct after it so this positive case establishes
    // the current-layout precondition; the guard test covers obsolete misses.
    let current_layout = egcl_stdlib::clos::class_layout_key(egcl_stdlib::class_of(*object))
        .expect("finalized positive class layout");
    assert_eq!(
        egcl_stdlib::clos::instance_layout_key(*object),
        Some(current_layout),
        "positive reader fixture must have the current nonobsolete wrapper"
    );
    let before = raw_slot_reader::hits();
    assert_eq!(
        read(&mut env, "(raw-leaf-reader *raw-leaf-object*)"),
        EgclVal::from_fixnum(17)
    );
    assert_eq!(raw_slot_reader::hits(), before + 1);

    // An actual installed mapped-T2 caller must reach the leaf. The observer
    // asserts its physical native owner through the real segment links; an
    // interpreter fallback cannot satisfy it.
    // As in the established ordinary-slot-access fixture, place the private
    // observer operator in a pinned T0 function. The opportunistic compiler
    // intentionally rejects that private operator as a direct named call.
    egcl_rt::rooted!(
        observer_name = crate::cli::read_next_form_at(
            &"raw-leaf-observe-entry".chars().collect::<Vec<_>>(),
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
        definition = crate::cli::vec_to_list(&[*defun, *observer_name, NIL, *observer_body])
    );
    crate::cli::eval_form(*definition, &mut env).unwrap();
    let _observer_pin =
        native_transfer_tests::ordinary_t2::RecursiveFixturePins::new(&[&crate::cli::sym_name(
            *observer_name,
        )]);
    // Read actual package-owned argument/reader symbols in the boot-loaded
    // environment; the compiled caller sees a real named observer function.
    egcl_rt::rooted!(
        params =
            crate::cli::read_next_form_at(&"(object)".chars().collect::<Vec<_>>(), 0, &mut env)
                .unwrap()
                .0
    );
    egcl_rt::rooted!(
        forms = crate::cli::read_next_form_at(
            &"((raw-leaf-observe-entry) (raw-leaf-reader object))"
                .chars()
                .collect::<Vec<_>>(),
            0,
            &mut env
        )
        .unwrap()
        .0
    );
    let symbol = reader::intern_symbol("RAW-LEAF-NATIVE-CALLER");
    let body = Arc::new(
        compile_function(
            "RAW-LEAF-NATIVE-CALLER",
            *params,
            *forms,
            &env,
            false,
            false,
        )
        .unwrap_or_else(|| panic!("native caller lowering: {:?}", last_bail_reason())),
    );
    let (symbol, code) = native_transfer_tests::ordinary_t2::install_body(symbol, body);
    assert!(code.is_t2);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped native caller");
    };
    assert!(!mapped.needs_recompile());
    native_transfer_tests::ordinary_t2::expect_entry(&code);
    let deopts = deopt_count();
    let before = raw_slot_reader::hits();
    assert_eq!(
        run_native(&code, symbol, &[*object], &mut env).unwrap(),
        EgclVal::from_fixnum(17)
    );
    assert_eq!(raw_slot_reader::hits(), before + 1);
    assert_eq!(deopt_count(), deopts);

    // Bypass must preserve raw semantics even when public USING-CLASS is
    // overridden. Generated ordinary accessors have a different contract.
    read(
        &mut env,
        "
      (defmethod egcl-mop:slot-value-using-class :around
          ((class standard-class) (object raw-leaf-probe) slot)
        (declare (ignore class object slot)) 91)
      (assert (= (slot-value *raw-leaf-object* 'payload) 91))
      (assert (= (raw-leaf-reader *raw-leaf-object*) 17))",
    );
}

#[test]
fn raw_reader_rebinding_auxiliary_methods_and_layout_misses_take_full_path() {
    if isolated_case("raw_reader_rebinding_auxiliary_methods_and_layout_misses_take_full_path") {
        return;
    }
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    boot(&mut env);
    read(
        &mut env,
        "
      (defclass raw-guard-probe () ((payload :initform 31)))
      (defparameter *raw-guard-object* (make-instance 'raw-guard-probe))
      (defmethod raw-guard-reader ((object raw-guard-probe))
        (egcl-internal::%raw-slot-value object 'payload))
      (assert (= (raw-guard-reader *raw-guard-object*) 31))
      (defvar *raw-guard-effects* 0)
      (defmethod raw-guard-reader :before ((object raw-guard-probe))
        (declare (ignore object)) (incf *raw-guard-effects*))",
    );
    let observed_method = method(&env, "RAW-GUARD-READER");
    let _reader_observation = raw_slot_reader::observe_method(observed_method);
    let before = raw_slot_reader::hits();
    read(
        &mut env,
        "
      (assert (= (raw-guard-reader *raw-guard-object*) 31))
      (assert (= *raw-guard-effects* 1))
      (remove-method #'raw-guard-reader
        (find-method #'raw-guard-reader '(:before) (list (find-class 'raw-guard-probe))))",
    );
    assert_eq!(raw_slot_reader::hits(), before);
    read(
        &mut env,
        "(assert (= (raw-guard-reader *raw-guard-object*) 31))",
    );
    assert_eq!(raw_slot_reader::hits(), before + 1);

    read(
        &mut env,
        "
      (defmethod raw-noninstance-reader ((object t))
        (egcl-internal::%raw-slot-value object 'payload))",
    );
    let before = raw_slot_reader::hits();
    read(
        &mut env,
        "(assert (handler-case (progn (raw-noninstance-reader 17) nil)
                             (error () t)))",
    );
    assert_eq!(raw_slot_reader::hits(), before);

    // Restore the raw primitive cell even if the assertion unwinds. No global
    // test-only reset changes real allocation counters or method identities.
    let id = method(&env, "RAW-GUARD-READER");
    let actual_proof = METHOD_RAW_READERS
        .borrow()
        .get(&id.0)
        .cloned()
        .expect("selected method proof");
    let raw = actual_proof.primitive;
    egcl_rt::rooted!(old_binding = egcl_rt::symbols::symbol_function(raw).unwrap());
    struct Restore(u32, EgclVal);
    impl Drop for Restore {
        fn drop(&mut self) {
            egcl_rt::symbols::set_symbol_function(self.0, self.1);
        }
    }
    // The old default binding is immediate, not a movable callable.
    assert_eq!(*old_binding, egcl_rt::value::UNBOUND);
    let restore = Restore(raw, *old_binding);
    read(
        &mut env,
        "(setf (symbol-function 'egcl-internal::%raw-slot-value)
                        (lambda (object slot) (declare (ignore object slot)) 79))",
    );
    let before = raw_slot_reader::hits();
    read(
        &mut env,
        "(assert (= (raw-guard-reader *raw-guard-object*) 79))",
    );
    assert_eq!(raw_slot_reader::hits(), before);
    drop(restore);
    read(
        &mut env,
        "(assert (= (raw-guard-reader *raw-guard-object*) 31))",
    );
    assert_eq!(raw_slot_reader::hits(), before + 1);

    let initializer = actual_proof.helpers[0];
    egcl_rt::rooted!(old_initializer = egcl_rt::symbols::symbol_function(initializer).unwrap());
    assert_eq!(*old_initializer, egcl_rt::value::UNBOUND);
    let restore = Restore(initializer, *old_initializer);
    read(
        &mut env,
        "(setf (symbol-function 'egcl-internal::%method-context)
       (lambda () (incf *raw-guard-effects*) nil))",
    );
    let before = raw_slot_reader::hits();
    read(
        &mut env,
        "(assert (= (raw-guard-reader *raw-guard-object*) 31))",
    );
    assert_eq!(raw_slot_reader::hits(), before);
    assert_eq!(
        read(&mut env, "*raw-guard-effects*"),
        EgclVal::from_fixnum(2)
    );
    drop(restore);

    // Same effective method remains applicable; an obsolete receiver must
    // migrate through the full update protocol before a later leaf can hit.
    read(
        &mut env,
        "(defclass raw-guard-probe () ((added :initform 7) (payload :initform 41)))",
    );
    let before = raw_slot_reader::hits();
    read(
        &mut env,
        "(assert (= (raw-guard-reader *raw-guard-object*) 31))",
    );
    assert_eq!(raw_slot_reader::hits(), before);
    read(
        &mut env,
        "(assert (= (raw-guard-reader *raw-guard-object*) 31))",
    );
    assert_eq!(raw_slot_reader::hits(), before + 1);
    read(&mut env, "(slot-makunbound *raw-guard-object* 'payload)");
    let before = raw_slot_reader::hits();
    read(
        &mut env,
        "(assert (handler-case (progn (raw-guard-reader *raw-guard-object*) nil)
                             (unbound-slot () t)))",
    );
    assert_eq!(raw_slot_reader::hits(), before);
}

#[test]
fn raw_reader_proof_uses_final_lowering_and_rejects_effects_and_binders() {
    if isolated_case("raw_reader_proof_uses_final_lowering_and_rejects_effects_and_binders") {
        return;
    }
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    boot(&mut env);
    read(&mut env, "
      (defclass raw-shape-probe () ((first :initform 11) (second :initform 22)))
      (defvar *raw-shape-effects* 0)
      (defmacro raw-shape-call (object) `(egcl-internal::%raw-slot-value ,object 'second))
      (defmethod raw-expanded-reader ((object raw-shape-probe)) (raw-shape-call object))
      (defmethod raw-wrong-result ((object raw-shape-probe))
        (egcl-internal::%raw-slot-value object 'first) 99)
      (defmethod raw-effect-reader ((object raw-shape-probe))
        (incf *raw-shape-effects*) (egcl-internal::%raw-slot-value object 'first))
      (defmethod raw-optional-reader ((object raw-shape-probe) &optional (ignored (incf *raw-shape-effects*)))
        (egcl-internal::%raw-slot-value object 'first))
      (defmethod raw-aux-reader ((object raw-shape-probe) &aux (ignored (incf *raw-shape-effects*)))
        (egcl-internal::%raw-slot-value object 'first))
      (defmethod raw-checked-reader ((object raw-shape-probe))
        (declare (type cons object)) (egcl-internal::%raw-slot-value object 'first))
      (defmethod raw-special-reader ((object raw-shape-probe))
        (declare (special object)) (egcl-internal::%raw-slot-value object 'first))
      (defmethod raw-result-reader ((object raw-shape-probe))
        (the cons (egcl-internal::%raw-slot-value object 'first)))
      (defmethod raw-after-effect-reader ((object raw-shape-probe))
        (prog1 (egcl-internal::%raw-slot-value object 'first) (incf *raw-shape-effects*)))
      (defmethod raw-local-reader ((object raw-shape-probe))
        (flet ((egcl-internal::%raw-slot-value (object slot)
                 (declare (ignore object slot)) 88))
          (egcl-internal::%raw-slot-value object 'first)))
      (defmethod raw-local-macro-reader ((object raw-shape-probe))
        (macrolet ((egcl-internal::%raw-slot-value (object slot)
                     (declare (ignore object slot)) '(incf *raw-shape-effects*)))
          (egcl-internal::%raw-slot-value object 'first)))
      (defmethod raw-closure-reader ((object raw-shape-probe))
        (let ((f (lambda () object))) (funcall f)))");
    let expanded = method(&env, "RAW-EXPANDED-READER");
    assert_raw_proof(expanded, "RAW-EXPANDED-READER");
    // THE is explicitly erased by the existing lowerer (not an emitted result
    // check). Admission follows actual final semantics, not source spelling.
    let erased_the = method(&env, "RAW-RESULT-READER");
    assert_raw_proof(
        erased_the,
        "RAW-RESULT-READER (THE erased by baseline lowering)",
    );
    for name in [
        "RAW-WRONG-RESULT",
        "RAW-EFFECT-READER",
        "RAW-OPTIONAL-READER",
        "RAW-AUX-READER",
        "RAW-CHECKED-READER",
        "RAW-CLOSURE-READER",
        "RAW-SPECIAL-READER",
        "RAW-AFTER-EFFECT-READER",
        "RAW-LOCAL-READER",
        "RAW-LOCAL-MACRO-READER",
    ] {
        let id = method(&env, name);
        assert!(!METHOD_RAW_READERS.borrow().contains_key(&id.0), "{name}");
    }
    read(
        &mut env,
        "
      (defparameter *raw-shape-object* (make-instance 'raw-shape-probe))
      (assert (= (raw-expanded-reader *raw-shape-object*) 22))
      (defmacro raw-shape-call (object) `(egcl-internal::%raw-slot-value ,object 'first))
      (assert (= (raw-expanded-reader *raw-shape-object*) 22))
      (defmethod raw-expanded-reader ((object raw-shape-probe)) (raw-shape-call object))
      (assert (= (raw-expanded-reader *raw-shape-object*) 11))
      (assert (= (raw-wrong-result *raw-shape-object*) 99))
      (assert (= (raw-result-reader *raw-shape-object*) 11))
      (assert (= (raw-effect-reader *raw-shape-object*) 11))
      (assert (= (raw-optional-reader *raw-shape-object*) 11))
      (assert (= (raw-aux-reader *raw-shape-object*) 11))
      (assert (= *raw-shape-effects* 3))",
    );
    // A retained CheckResult instruction really is observable and must reject.
    // Obtain site metadata through real scoped lowering, then mutate only this
    // independent clone; the live method and its immutable code stay intact.
    egcl_rt::rooted!(
        params =
            crate::cli::read_next_form_at(&"(object)".chars().collect::<Vec<_>>(), 0, &mut env)
                .unwrap()
                .0
    );
    egcl_rt::rooted!(
        forms = crate::cli::read_next_form_at(
            &"((egcl-internal::%raw-slot-value object 'first))"
                .chars()
                .collect::<Vec<_>>(),
            0,
            &mut env
        )
        .unwrap()
        .0
    );
    let mut metadata = None;
    *forms = crate::cli::method_continuations::wrap_body(*forms, &mut metadata).unwrap();
    let scope = method_context_use::Scope::enter(metadata.unwrap());
    let body = compile_function(
        "(METHOD RAW-CHECK-VERIFIER)",
        *params,
        *forms,
        &env,
        true,
        false,
    )
    .unwrap_or_else(|| panic!("verifier control lowering: {:?}", last_bail_reason()));
    let spec = scope
        .raw_reader()
        .unwrap_or_else(|| panic!("verifier control missing proof: {:?}", body.code));
    assert!(spec.matches(&body));
    let mut checked = body.clone();
    checked.code.insert(
        checked.code.len() - 1,
        Instr::CheckResult(DeclaredType::Fixnum),
    );
    assert!(
        !spec.matches(&checked),
        "an actual retained result check cannot be skipped"
    );
}
