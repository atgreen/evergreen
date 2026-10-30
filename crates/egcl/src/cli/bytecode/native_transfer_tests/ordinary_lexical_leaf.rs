use super::ordinary_captured_context::{closure, install_automatic, template};
use super::*;
use native_transfer_entry::ordinary_closure_context as lexical;
use std::cell::Cell;

thread_local! {
    static MOVED: Cell<Option<(u64, u64, bool)>> = const { Cell::new(None) };
}
fn poll_at_store(store: bool) {
    if store {
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        unsafe { &*execution }.request_yield();
    }
}
fn collect_outgoing(value: *const EgclVal) {
    let before = unsafe { (*value).0 };
    let ok = egcl_rt::gc::collect_t0_minor().is_ok();
    let after = unsafe { (*value).0 };
    MOVED.with(|slot| slot.set(Some((before, after, ok))));
}
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        lexical::BEFORE_LEAF.with(|hook| hook.set(None));
        lexical::STORE_OBSERVER.with(|hook| hook.set(None));
    }
}

#[test]
#[ignore = "requires a platform-supported native compiler"]
fn ordinary_lexical_poll_store_moves_outgoing_value_before_mutation() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        "((setq ordinary-native-cell next) ordinary-native-cell)",
        &env,
    );
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(function = closure(&body, NIL, &mut env));
    let code = install_automatic(*function);
    let NativeCodeStorage::Mapped(owner) = &code._storage else {
        panic!("native owner")
    };
    assert!(owner.ordinary_entry.is_some() && owner.native_callable_slot.is_some());
    assert_eq!(
        owner.run_callable(*function, &[NIL], &mut env).unwrap(),
        NIL
    );
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(value = crate::cli::arena_cons(EgclVal::from_fixnum(89), NIL));
    MOVED.with(|slot| slot.set(None));
    lexical::HITS.with(|count| count.set(0));
    lexical::PUBLISHED.with(|count| count.set(0));
    lexical::BEFORE_LEAF.with(|hook| hook.set(Some(poll_at_store)));
    lexical::STORE_OBSERVER.with(|hook| hook.set(Some(collect_outgoing)));
    let _reset = Reset;
    let result = owner.run_callable(*function, &[*value], &mut env).unwrap();
    let (before, after, ok) = MOVED
        .with(Cell::get)
        .expect("published outgoing store observed");
    assert!(ok);
    assert_ne!(
        before, after,
        "the outgoing argument must move BEFORE the setter"
    );
    assert_eq!(result.0, after);
    assert_eq!(value.0, after);
    let name = body
        .names
        .iter()
        .find(|name| name.ends_with("ORDINARY-NATIVE-CELL"))
        .unwrap();
    let frame = closure_captured_env(*function).unwrap();
    assert_eq!(Env::lookup_frame(&frame, name), Some(*value));
    assert_eq!(cp(result).0, EgclVal::from_fixnum(89));
    assert!(lexical::PUBLISHED.with(Cell::get) >= 1);
}

#[test]
#[ignore = "requires a platform-supported native compiler"]
fn ordinary_lexical_missing_binding_preserves_exact_condition_after_completed_store() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let existing_symbol = resolve_sym("ORDINARY-NATIVE-CELL").unwrap();
    let missing_symbol = resolve_sym("ORDINARY-MISSING-CELL").unwrap();
    let existing_name = crate::cli::sym_name(existing_symbol);
    let missing_name = crate::cli::sym_name(missing_symbol);
    egcl_rt::rooted!(params = reader::read_from_string("(next)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((setq ordinary-native-cell (+ ordinary-native-cell 1)) ordinary-missing-cell)"
        )
        .unwrap()
        .0
    );
    let captures = std::collections::HashSet::from([existing_name.clone(), missing_name.clone()]);
    let empty = std::collections::HashSet::new();
    let body = Arc::new(
        compile_function_in(
            "ORDINARY-LEXICAL-PREFIX-ERROR",
            *params,
            *forms,
            &env,
            &HashMap::new(),
            false,
            false,
            &empty,
            &empty,
            &empty,
            &captures,
            None,
        )
        .expect("two real captured bindings"),
    );
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(function = closure(&body, EgclVal::from_fixnum(7), &mut env));
    let frame = closure_captured_env(*function).unwrap();
    bind_boxed_param(&frame, &missing_name, EgclVal::from_fixnum(99));
    let code = install_automatic(*function);
    let NativeCodeStorage::Mapped(owner) = &code._storage else {
        panic!("native owner")
    };
    assert!(owner.ordinary_entry.is_some() && owner.native_callable_slot.is_some());
    // Remove ONLY the second binding after native compilation. The same
    // activation must commit its first capture increment, then signal.
    {
        let mut bindings = frame.borrow_mut();
        bindings
            .vars
            .entries
            .retain(|(name, _)| name != &missing_name);
        bindings
            .symbol_vars
            .entries
            .retain(|(symbol, _)| *symbol != missing_symbol.as_symbol_index());
    }
    lexical::HITS.with(|count| count.set(0));
    lexical::PUBLISHED.with(|count| count.set(0));
    egcl_rt::rooted!(
        error = owner
            .run_callable(*function, &[NIL], &mut env)
            .expect_err("second capture must be missing after the committed prefix")
    );
    // This low-level native API returns the exact runtime error. Lisp
    // condition construction belongs to its enclosing evaluator boundary.
    match error.without_backtrace() {
        EgclError::UnboundVariable(symbol) => assert_eq!(*symbol, missing_symbol),
        other => panic!("expected exact missing UnboundVariable, got {other:?}"),
    }
    assert_eq!(
        Env::lookup_frame(&frame, &existing_name),
        Some(EgclVal::from_fixnum(8)),
        "the failed activation must increment its first capture exactly once"
    );
    assert!(
        lexical::HITS.with(Cell::get) > 0,
        "completed prefix uses native lexical leaves"
    );
    assert_eq!(
        lexical::PUBLISHED.with(Cell::get),
        1,
        "only the missing read publishes"
    );
}

#[test]
#[ignore = "requires a platform-supported native compiler"]
fn ordinary_lexical_pending_poll_requires_published_store() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        "((setq ordinary-native-cell next) ordinary-native-cell)",
        &env,
    );
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(function = closure(&body, NIL, &mut env));
    let code = install_automatic(*function);
    let NativeCodeStorage::Mapped(owner) = &code._storage else {
        panic!("native owner")
    };
    assert!(owner.ordinary_entry.is_some() && owner.native_callable_slot.is_some());
    assert_eq!(
        owner.run_callable(*function, &[NIL], &mut env).unwrap(),
        NIL
    );
    lexical::PUBLISHED.with(|count| count.set(0));
    lexical::HITS.with(|count| count.set(0));
    lexical::BEFORE_LEAF.with(|hook| hook.set(Some(poll_at_store)));
    let _reset = Reset;
    assert_eq!(
        owner
            .run_callable(*function, &[EgclVal::from_fixnum(17)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(17)
    );
    assert_eq!(
        lexical::PUBLISHED.with(Cell::get),
        1,
        "only the armed store requires publication"
    );
    assert!(
        lexical::HITS.with(Cell::get) >= 1,
        "subsequent lexical reads use the leaf"
    );
    assert!(!egcl_rt::thread::current_execution_poll_pending());
}

#[test]
#[ignore = "requires bootstrapped native compilation"]
fn ordinary_lexical_bootstrapped_funcall_cold_after_major_uses_leaf() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // Match CLI startup's mandatory prelude, including real INCF/DOTIMES
    // expansion. Minimal Env fixtures alone missed the cold adapter ingress.
    crate::cli::read_eval_all_env(crate::cli::EMBEDDED_BOOT_LISP, &mut env).unwrap();
    struct BootRestore(bool);
    impl Drop for BootRestore {
        fn drop(&mut self) {
            crate::cli::BOOT_COMPLETE.with(|ready| ready.set(self.0));
        }
    }
    let _boot = BootRestore(crate::cli::BOOT_COMPLETE.with(|ready| ready.replace(true)));
    crate::cli::read_eval_all_env(
        "(declaim (notinline lexical-boot-factory lexical-boot-loop lexical-boot-batch))
         (defun lexical-boot-factory ()
           (let ((seed 7)) (lambda (k) (incf seed) (+ k seed))))
         (defparameter *lexical-boot-held* (lexical-boot-factory))
         (defun lexical-boot-loop (f n)
           (let ((sum 0)) (dotimes (i n sum) (incf sum (funcall f i)))))
         (defun lexical-boot-batch (n) (lexical-boot-loop *lexical-boot-held* n))
         (dotimes (i 12) (lexical-boot-batch 8))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(
        held = crate::cli::read_eval_all_env("*lexical-boot-held*", &mut env).unwrap()
    );
    egcl_rt::rooted!(
        caller = crate::cli::read_eval_all_env("#'lexical-boot-loop", &mut env).unwrap()
    );
    egcl_rt::rooted!(
        batch = crate::cli::read_eval_all_env("#'lexical-boot-batch", &mut env).unwrap()
    );
    // The cold, once-called factory returns an original tree wrapper. Its
    // warmed compiled object is the one that owns bytecode/native metadata;
    // leave the original wrapper in the Lisp global to exercise real dispatch.
    for function in [&mut *held, &mut *caller, &mut *batch] {
        *function = native_transfer_entry::funcall_fast::closure_carrier(*function)
            .expect("warmed source callable must have a settled compiled carrier");
        assert!(egcl_rt::function::is_interpreted_function(*function));
        assert!(
            registry_get(egcl_rt::function::definition_index(*function)).is_some(),
            "install the actual compiled carrier, never wrapper payload bits"
        );
    }
    // Deterministic owner-side installation avoids dependence on process-wide
    // invoke thresholds while compiling the real, already-profiled source.
    let child_code = install_automatic(*held);
    // A public DEFUN has a separate private FUNCTION-DEFINITION identity.
    // Named machine calls dispatch through the public symbol, so install the
    // actual public owners rather than merely tiering their private aliases.
    let install_named = |name: &str, env: &mut Env| {
        // Read in the same package/environment as the real DEFUN. A bare
        // registry spelling outside the reader hook can intern a lookalike.
        let symbol = crate::cli::read_eval_all_env(&format!("'{name}"), env)
            .unwrap()
            .as_symbol_index();
        assert!(
            registry_get(symbol).is_some(),
            "public bytecode owner {name}"
        );
        let input = snapshot_t2_input(symbol, 0).expect("profiled public T2 snapshot");
        let generation = input.generation;
        let input = egcl_rt::CrossThreadRoot::new(input);
        let artifact = input
            .with_gc_stable_mutator(compile_t2_artifact)
            .expect("public T2 artifact");
        let code = install_t2_completion(T2Completion {
            panicked: false,
            _permit: None,
            dependencies: Vec::new(),
            sym: symbol,
            generation,
            artifact: Some(artifact),
            input,
        })
        .expect("public T2 completion");
        assert!(NATIVE_REGISTRY.with(|registry| {
            registry
                .borrow()
                .get(&symbol)
                .is_some_and(|installed| Rc::ptr_eq(installed, &code))
        }));
        (symbol, code)
    };
    let (caller_symbol, caller_code) = install_named("LEXICAL-BOOT-LOOP", &mut env);
    let (batch_symbol, batch_code) = install_named("LEXICAL-BOOT-BATCH", &mut env);
    assert_eq!(
        egcl_rt::symbols::symbol_function(caller_symbol),
        Some(*caller)
    );
    assert_eq!(
        egcl_rt::symbols::symbol_function(batch_symbol),
        Some(*batch)
    );
    for code in [&child_code, &caller_code, &batch_code] {
        let NativeCodeStorage::Mapped(owner) = &code._storage else {
            panic!("native owner")
        };
        assert!(code.is_t2 && owner.ordinary_entry.is_some());
    }
    assert_eq!(
        crate::cli::read_eval_all_env(
            "(list (egcl-ext:function-tier *lexical-boot-held*)
               (egcl-ext:function-tier #'lexical-boot-loop)
               (egcl-ext:function-tier #'lexical-boot-batch))",
            &mut env,
        )
        .map(crate::cli::list_to_vec)
        .unwrap(),
        vec![EgclVal::from_fixnum(2); 3]
    );
    for _ in 0..4 {
        run_native_callable(
            &batch_code,
            batch_symbol,
            *batch,
            &[EgclVal::from_fixnum(8)],
            &mut env,
        )
        .unwrap();
    }
    egcl_rt::gc::full_gc().unwrap();
    for (symbol, expected) in [(caller_symbol, &caller_code), (batch_symbol, &batch_code)] {
        assert!(NATIVE_REGISTRY.with(|registry| {
            registry
                .borrow()
                .get(&symbol)
                .is_some_and(|installed| Rc::ptr_eq(installed, expected))
        }));
        let NativeCodeStorage::Mapped(owner) = &expected._storage else {
            panic!("public native owner")
        };
        assert!(owner.ordinary_entry.is_some());
    }
    let frame = closure_captured_env(*held).unwrap();
    let seed_name = child_code
        .body
        .as_ref()
        .unwrap()
        .names
        .iter()
        .find(|name| name.ends_with("SEED"))
        .unwrap();
    let seed = Env::lookup_frame(&frame, seed_name).unwrap().as_fixnum();
    lexical::HITS.with(|count| count.set(0));
    lexical::PUBLISHED.with(|count| count.set(0));
    let _ = native_transfer_entry::funcall_fast::take_fast_calls_for_test();
    let result = run_native_callable(
        &batch_code,
        batch_symbol,
        *batch,
        &[EgclVal::from_fixnum(8)],
        &mut env,
    )
    .unwrap();
    assert_eq!(result, EgclVal::from_fixnum(8 * seed + 64));
    assert_eq!(
        Env::lookup_frame(&frame, seed_name),
        Some(EgclVal::from_fixnum(seed + 8))
    );
    assert!(
        lexical::HITS.with(Cell::get) > 0,
        "bootstrapped generated caller must hit the lexical leaf"
    );
    assert_eq!(
        lexical::PUBLISHED.with(Cell::get),
        0,
        "post-major cold lexical adapters must not publish on successful accesses"
    );
    // The first FUNCALL after GC may refill its probe through Rust. The
    // remaining calls must be generated children of the real compiled loop.
    assert!(native_transfer_entry::funcall_fast::take_fast_calls_for_test().0 <= 1);
}

#[test]
fn ordinary_encoded_capture_keys_preserve_identity_and_move_published_store() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    for escaped in [false, true] {
        egcl_rt::rooted!(
            target = if escaped {
                EgclVal::from_symbol_index(egcl_rt::symbols::intern("\0egcl-lexical:symbol:999"))
            } else {
                egcl_rt::symbols::make_uninterned("ENCODED-CAPTURE")
            }
        );
        egcl_rt::rooted!(
            other = egcl_rt::symbols::make_uninterned(
                &egcl_rt::symbols::symbol_name(target.as_symbol_index()).unwrap()
            )
        );
        assert_ne!(*target, *other);
        let key = lexical_identity::key(*target);
        let other_key = lexical_identity::key(*other);
        assert!(lexical_identity::encoded(&key));
        egcl_rt::rooted!(params = reader::read_from_string("(next)").unwrap().0);
        egcl_rt::rooted!(
            assignment = crate::cli::vec_to_list(&[
                resolve_sym("SETQ").unwrap(),
                *target,
                resolve_sym("NEXT").unwrap()
            ])
        );
        egcl_rt::rooted!(forms = crate::cli::vec_to_list(&[*assignment, *target]));
        let empty = std::collections::HashSet::new();
        let captures = std::collections::HashSet::from([key.clone()]);
        let body = Arc::new(
            compile_function_in(
                "ORDINARY-ENCODED-CAPTURE",
                *params,
                *forms,
                &env,
                &HashMap::new(),
                false,
                false,
                &empty,
                &empty,
                &empty,
                &captures,
                None,
            )
            .unwrap(),
        );
        egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
        let mut construction = env.child();
        construction.frame = Arc::new(SharedCell::new(EnvFrame {
            vars: crate::cli::VecMap::default(),
            symbol_vars: crate::cli::VecMap::default(),
            parent: None,
        }));
        egcl_rt::rooted_ref!(_construction = &mut construction);
        bind_boxed_param(&construction.frame, &key, NIL);
        bind_boxed_param(&construction.frame, &other_key, EgclVal::from_fixnum(99));
        egcl_rt::rooted!(
            function = make_bytecode_closure(&body, Some(Arc::clone(&construction.frame)))
        );
        let code = install_automatic(*function);
        let NativeCodeStorage::Mapped(owner) = &code._storage else {
            panic!("native owner");
        };
        assert!(owner.ordinary_entry.is_some() && owner.native_callable_slot.is_some());
        lexical::HITS.with(|count| count.set(0));
        lexical::PUBLISHED.with(|count| count.set(0));
        assert_eq!(
            owner.run_callable(*function, &[NIL], &mut env).unwrap(),
            NIL
        );
        assert!(
            lexical::HITS.with(Cell::get) > 0,
            "encoded keys must execute actual native lexical leaves"
        );
        assert_eq!(lexical::PUBLISHED.with(Cell::get), 0);
        egcl_rt::gc::collect_t0_minor().unwrap();
        egcl_rt::rooted!(value = crate::cli::arena_cons(EgclVal::from_fixnum(89), NIL));
        MOVED.with(|slot| slot.set(None));
        lexical::BEFORE_LEAF.with(|hook| hook.set(Some(poll_at_store)));
        lexical::STORE_OBSERVER.with(|hook| hook.set(Some(collect_outgoing)));
        let _reset = Reset;
        egcl_rt::rooted!(result = owner.run_callable(*function, &[*value], &mut env).unwrap());
        let (before, after, ok) = MOVED
            .with(Cell::get)
            .expect("published encoded store observed");
        assert!(ok);
        assert_ne!(before, after, "outgoing value must actually relocate");
        assert_eq!(result.to_raw(), after);
        assert_eq!(*result, *value);
        assert_eq!(
            Env::lookup_lexical_frame(&construction.frame, &key),
            Some(*value)
        );
        assert_eq!(
            Env::lookup_lexical_frame(&construction.frame, &other_key),
            Some(EgclVal::from_fixnum(99))
        );
        assert!(lexical::PUBLISHED.with(Cell::get) >= 1);
        assert_eq!(owner.deopt_count(), 0);
    }
}
