//! Evaluator bridge for Lisp-defined class protocol methods.
use super::*;

pub(super) fn capture_slot_initfunctions(
    slots: &mut Vec<SlotDef>,
    env: &mut Env,
) -> Result<(), EgclError> {
    egcl_rt::rooted_ref!(_slots = slots);
    egcl_rt::rooted!(lambda = resolve_sym("LAMBDA").unwrap_or(NIL));
    egcl_rt::rooted!(funcall = resolve_sym("FUNCALL").unwrap_or(NIL));
    let mut i = 0;
    while i < slots.len() {
        let supplied = slots[i].mop_options.chunks_exact(2).find_map(|pair| {
            (sym_bare_name_rc(pair[0]).as_ref() == "INITFUNCTION").then_some(pair[1])
        });
        if let Some(function) = supplied {
            if !function.is_nil() && !is_function_value(function) {
                return Err(EgclError::TypeError {
                    datum: function,
                    expected: "FUNCTION".into(),
                });
            }
            slots[i].initfunction = (!function.is_nil()).then_some(function);
        } else if let Some(form) = slots[i].source_initform {
            egcl_rt::rooted!(source = vec_to_list(&[*lambda, NIL, form]));
            slots[i].initfunction = Some(eval_form(*source, env)?);
        }
        if let Some(function) = slots[i].initfunction {
            slots[i].initform = Some(vec_to_list(&[*funcall, function]));
        }
        i += 1;
    }
    Ok(())
}

fn field(slot: EgclVal, name: &str) -> Result<EgclVal, EgclError> {
    let index = reader::find_symbol_index(&format!("EGCL-MOP::{name}"))
        .ok_or_else(|| EgclError::ProgramError("slot protocol is not initialized".into()))?;
    egcl_stdlib::clos::slot_value_if_present(slot, EgclVal::from_symbol_index(index)).ok_or_else(
        || {
            EgclError::ProgramError(
                "slot definition has no current standard metadata layout".into(),
            )
        },
    )?
}

fn checked_list(mut list: EgclVal) -> Result<Vec<EgclVal>, EgclError> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    while list.is_cons() {
        if !seen.insert(list) {
            return Err(EgclError::ProgramError("cyclic class protocol list".into()));
        }
        let (value, tail) = cp(list);
        result.push(value);
        list = tail;
    }
    if !list.is_nil() {
        return Err(EgclError::ProgramError(
            "class protocol requires a proper list".into(),
        ));
    }
    Ok(result)
}

fn effective_slot(slot: EgclVal) -> Result<SlotDef, EgclError> {
    let name = field(slot, "NAME")?;
    if !name.is_symbol() {
        return Err(EgclError::ProgramError(
            "computed slot name must be a symbol".into(),
        ));
    }
    let allocation = field(slot, "ALLOCATION")?;
    let allocation = match sym_bare_name_rc(allocation).as_ref() {
        "INSTANCE" => SlotAllocation::Instance,
        "CLASS" => SlotAllocation::Class,
        _ => {
            return Err(EgclError::ProgramError(
                "computed slot allocation must be :INSTANCE or :CLASS".into(),
            ));
        }
    };
    let function = field(slot, "INITFUNCTION")?;
    if !function.is_nil() && !is_function_value(function) {
        return Err(EgclError::TypeError {
            datum: function,
            expected: "FUNCTION".into(),
        });
    }
    let initargs = checked_list(field(slot, "INITARGS")?)?;
    if initargs.iter().any(|arg| !arg.is_symbol()) {
        return Err(EgclError::ProgramError(
            "computed slot initargs must be symbols".into(),
        ));
    }
    Ok(SlotDef {
        name: sym_name(name),
        initargs: initargs
            .into_iter()
            .map(|arg| symbol_bare_name(&sym_name(arg)))
            .collect(),
        accessor: None,
        readers: Vec::new(),
        writers: Vec::new(),
        initform: (!function.is_nil())
            .then(|| field(slot, "INITFORM"))
            .transpose()?,
        source_initform: (!function.is_nil())
            .then(|| field(slot, "INITFORM"))
            .transpose()?,
        initfunction: (!function.is_nil()).then_some(function),
        mop_options: Vec::new(),
        allocation,
    })
}

pub(super) fn effective_slots(class_name: &str) -> Option<Vec<SlotDef>> {
    let name = reader::find_symbol_index(class_name)?;
    let class = egcl_stdlib::find_class(EgclVal::from_symbol_index(name))?;
    let snapshot = egcl_stdlib::clos::class_protocol_runtime_slots(class)?;
    Some(
        list_to_vec(snapshot)
            .into_iter()
            .map(|entry| {
                let fields = list_to_vec(entry);
                SlotDef {
                    name: sym_name(fields[0]),
                    initargs: list_to_vec(fields[1])
                        .into_iter()
                        .map(|arg| symbol_bare_name(&sym_name(arg)))
                        .collect(),
                    initform: (!fields[3].is_nil()).then_some(fields[2]),
                    source_initform: (!fields[3].is_nil()).then_some(fields[4]),
                    initfunction: (!fields[3].is_nil()).then_some(fields[3]),
                    allocation: if fields[5].is_nil() {
                        SlotAllocation::Instance
                    } else {
                        SlotAllocation::Class
                    },
                    accessor: None,
                    readers: Vec::new(),
                    writers: Vec::new(),
                    mop_options: Vec::new(),
                }
            })
            .collect(),
    )
}

pub(super) fn call(_name: &str, args: &[EgclVal], env: &mut Env) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(args = args.to_vec());
    if args.len() < 2 || !args[0].is_symbol() {
        return Err(EgclError::ProgramError(
            "class protocol bridge requires operation and class".into(),
        ));
    }
    if sym_bare_name_rc(args[0]).as_ref() == "DEFINE" && args.len() == 5 {
        return eval_defclass_with_defaults(args[2], env, Some(args[3]), Some(args[4]));
    }
    let mut class = resolve_class_metaobject(env, args[1])?;
    egcl_rt::rooted_ref!(_class = &mut class);
    match sym_bare_name_rc(args[0]).as_ref() {
        "DIRECT" | "EFFECTIVE" => {
            let direct = sym_bare_name_rc(args[0]).as_ref() == "DIRECT";
            let (value, ready) = egcl_stdlib::clos::class_slot_metaobjects(class, direct);
            // A one-element wrapper distinguishes a cached empty slot list.
            Ok(if ready { arena_cons(value, NIL) } else { NIL })
        }
        "SET-DIRECT" if args.len() == 3 => {
            egcl_stdlib::clos::set_class_direct_slot_metaobjects(class, args[2])?;
            Ok(args[2])
        }
        "SET-RAW-EFFECTIVE" if args.len() == 3 => {
            if supports_protocol(class) {
                return Err(EgclError::ProgramError(
                    "standard classes require protocol slot computation".into(),
                ));
            }
            egcl_rt::rooted!(slots = args[2]);
            egcl_rt::rooted!(wrapper = arena_cons(*slots, NIL));
            egcl_stdlib::clos::set_class_raw_effective_slot_metaobjects(class, *wrapper)?;
            Ok(*slots)
        }
        "BEGIN" => {
            egcl_stdlib::clos::begin_class_finalization(class)?;
            Ok(class)
        }
        "FINALIZED" => Ok(if egcl_stdlib::clos::class_protocol_finalized(class) {
            T
        } else {
            NIL
        }),
        "CPL" => {
            let cpl = egcl_stdlib::compute_class_precedence_list(class)?;
            Ok(vec_to_list(&cpl))
        }
        "DEFAULT-CPL" => {
            let cpl = egcl_stdlib::clos::compute_class_precedence_list_default(class)?;
            Ok(vec_to_list(&cpl))
        }
        "PUBLISH" if args.len() == 6 => {
            egcl_rt::rooted!(cpl = checked_list(args[2])?);
            let defaults = checked_list(args[4])?;
            let mut default_names = HashMap::new();
            for entry in defaults {
                let fields = checked_list(entry)?;
                if fields.len() != 3
                    || !fields[0].is_symbol()
                    || !is_function_value(fields[2])
                    || default_names.insert(fields[0], fields[2]).is_some()
                {
                    return Err(EgclError::ProgramError(
                        "invalid computed default initargs".into(),
                    ));
                }
            }
            let runtime_defaults = checked_list(args[5])?;
            if runtime_defaults.len() != default_names.len() {
                return Err(EgclError::ProgramError(
                    "invalid runtime default initargs".into(),
                ));
            }
            let mut runtime_names = HashSet::new();
            for entry in runtime_defaults {
                let fields = checked_list(entry)?;
                if fields.len() != 2 || !runtime_names.insert(fields[0]) {
                    return Err(EgclError::ProgramError(
                        "invalid runtime default initargs".into(),
                    ));
                }
                let call = checked_list(fields[1])?;
                if call.len() != 2
                    || !call[0].is_symbol()
                    || sym_bare_name_rc(call[0]).as_ref() != "FUNCALL"
                    || default_names.get(&fields[0]) != Some(&call[1])
                {
                    return Err(EgclError::ProgramError(
                        "runtime defaults must call the computed initfunctions".into(),
                    ));
                }
            }
            egcl_rt::rooted!(objects = checked_list(args[3])?);
            egcl_rt::rooted!(funcall = resolve_sym("FUNCALL").unwrap_or(NIL));
            egcl_rt::rooted!(snapshot = Vec::<EgclVal>::new());
            egcl_rt::rooted!(names = Vec::new());
            let mut seen = HashSet::new();
            let mut shared_names = Vec::new();
            for i in 0..objects.len() {
                egcl_rt::rooted!(slot = effective_slot(objects[i])?);
                if let Some(function) = slot.initfunction {
                    slot.initform = Some(vec_to_list(&[*funcall, function]));
                }
                if !seen.insert(slot.name.clone()) {
                    return Err(EgclError::ProgramError(
                        "duplicate computed effective slot".into(),
                    ));
                }
                let name = field(objects[i], "NAME")?;
                if slot.allocation == SlotAllocation::Instance {
                    names.push(name);
                } else {
                    shared_names.push(symbol_bare_name(&slot.name));
                }
                egcl_rt::rooted!(
                    fields = vec![
                        name,
                        field(objects[i], "INITARGS")?,
                        slot.initform.unwrap_or(NIL),
                        slot.initfunction.unwrap_or(NIL),
                        slot.source_initform.unwrap_or(NIL),
                        if slot.allocation == SlotAllocation::Class {
                            T
                        } else {
                            NIL
                        }
                    ]
                );
                snapshot.push(vec_to_list(&fields));
            }
            egcl_rt::rooted!(snapshot = vec_to_list(&snapshot));
            // Validate before publishing. Preserve existing shared owners/cells;
            // newly computed shared slots acquire a cell in this class.
            let class_name = class_name_for_instance_class(class);
            let missing_shared: Vec<_> = shared_names
                .into_iter()
                .filter(|name| class_slot_owner(env, &class_name, name).is_none())
                .collect();
            egcl_stdlib::clos::publish_class_protocol(
                class, &cpl, &names, args[3], args[4], args[5], *snapshot,
            )?;
            let shared_cells = env
                .classes
                .borrow()
                .get(&class_name)
                .map(|definition| Arc::clone(&definition.class_slot_values));
            if let Some(shared_cells) = shared_cells {
                let mut cells = shared_cells.lock().unwrap();
                for name in missing_shared {
                    cells.entry(name).or_insert(None);
                }
            }
            invalidate_gf_dispatch_cache();
            bump_accessor_slot_gen();
            Ok(class)
        }
        "DIRECT-DEFAULTS" => {
            let class_name = class_name_for_instance_class(class);
            Ok(env
                .classes
                .borrow()
                .get(&class_name)
                .map_or(NIL, |c| c.direct_default_metadata))
        }
        "DEFAULTS" => Ok(egcl_stdlib::clos::class_protocol_default_initargs(class).unwrap_or(NIL)),
        _ => Err(EgclError::ProgramError(
            "invalid class protocol operation".into(),
        )),
    }
}

pub(super) fn capture_default_initargs(
    names: &[String],
    forms: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(forms = forms.to_vec());
    egcl_rt::rooted!(out = Vec::<EgclVal>::new());
    egcl_rt::rooted!(lambda = resolve_sym("LAMBDA").unwrap_or(NIL));
    for i in 0..names.len() {
        egcl_rt::rooted!(source = vec_to_list(&[*lambda, NIL, forms[i]]));
        egcl_rt::rooted!(function = eval_form(*source, env)?);
        let keyword = resolve_sym(&format!(":{}", names[i])).unwrap_or(NIL);
        out.push(vec_to_list(&[keyword, forms[i], *function]));
    }
    Ok(vec_to_list(&out))
}

pub(super) fn effective_defaults(class_name: &str) -> Option<Vec<(String, EgclVal)>> {
    let symbol = EgclVal::from_symbol_index(reader::find_symbol_index(class_name)?);
    let class = egcl_stdlib::find_class(symbol)?;
    let defaults = egcl_stdlib::clos::class_protocol_runtime_defaults(class)?;
    Some(
        list_to_vec(defaults)
            .into_iter()
            .map(|entry| {
                let (name, tail) = cp(entry);
                (symbol_bare_name(&sym_name(name)), cp(tail).0)
            })
            .collect(),
    )
}

/// Automatic AMOP finalization is defined for STANDARD-CLASS and its
/// subclasses, not for the independent STRUCTURE-CLASS/BUILT-IN-CLASS kinds.
pub(super) fn supports_protocol(class: EgclVal) -> bool {
    let Some(name) = reader::find_symbol_index("STANDARD-CLASS") else {
        return false;
    };
    let Some(standard) = egcl_stdlib::find_class(EgclVal::from_symbol_index(name)) else {
        return false;
    };
    egcl_stdlib::compute_class_precedence_list(egcl_stdlib::class_of(class))
        .is_ok_and(|cpl| cpl.contains(&standard))
}

pub(super) fn ensure_finalized(mut class: EgclVal, env: &mut Env) -> Result<(), EgclError> {
    egcl_rt::rooted_ref!(_class_root = &mut class);
    if egcl_stdlib::clos::class_protocol_finalized(class) || !supports_protocol(class) {
        return Ok(());
    }
    if let Some(active) = reader::find_symbol_index("EGCL-MOP::*FINALIZING-CLASS*") {
        if env.lookup_var_symbol(EgclVal::from_symbol_index(active)) == Some(class) {
            return Err(EgclError::ProgramError(
                "cannot access instances during their class finalization".into(),
            ));
        }
    }
    if !egcl_stdlib::clos::forward_referenced_superclass(class).is_nil() {
        return Err(EgclError::ProgramError(
            "cannot finalize a class with a forward-referenced superclass".into(),
        ));
    }
    // Use the library's checked raw hierarchy, not a stale computed CPL. This
    // also protects custom child finalizers reached through the runtime path.
    egcl_rt::rooted!(ancestors = egcl_stdlib::clos::compute_class_precedence_list_default(class)?);
    for index in (1..ancestors.len()).rev() {
        ensure_finalized(ancestors[index], env)?;
    }
    let Some(index) = reader::find_symbol_index("EGCL-MOP:FINALIZE-INHERITANCE")
        .or_else(|| reader::find_symbol_index("EGCL-MOP::FINALIZE-INHERITANCE"))
    else {
        return if BOOT_COMPLETE.with(|complete| complete.get()) {
            Err(EgclError::ProgramError(
                "class finalization protocol is unavailable".into(),
            ))
        } else {
            Ok(())
        };
    };
    egcl_rt::rooted!(helper = EgclVal::from_symbol_index(index));
    egcl_rt::rooted!(
        function = egcl_rt::symbols::symbol_function(index).unwrap_or(egcl_rt::value::UNBOUND)
    );
    let name = sym_name(*helper);
    if *function == *helper && generic_binding::current(&name) {
        // A native generic's physical function cell is its self-symbol marker.
        // Reify only that verified current binding, never stale method tables.
        *function = generic_fn_wrapper(env, *helper, &name);
    } else if !is_function_value(*function) {
        return if BOOT_COMPLETE.with(|complete| complete.get()) {
            Err(EgclError::UndefinedFunction(*helper))
        } else {
            Ok(())
        };
    }
    egcl_rt::rooted!(arguments = vec![class]);
    apply_function(*function, &arguments, env)?;
    if !egcl_stdlib::clos::class_protocol_finalized(class) {
        return Err(EgclError::ProgramError(
            "finalizer did not finalize the class".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBLICATION_CHILD: &str = "EGCL_MOP_PUBLICATION_ROOT_CHILD";
    const PUBLICATION_FILTER: &str =
        "cli::class_protocol::tests::publication_updates_detached_argument_copy";

    #[test]
    fn bridge_arguments_survive_moving_publication() {
        for moving in [false, true] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args([
                "--exact",
                PUBLICATION_FILTER,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ]);
            for key in [
                "EGCL_GC_STRESS",
                "EGCL_GC_STRESS_SKIP",
                "EGCL_GC_STRESS_AT",
                "EGCL_GC_POISON",
                "EGCL_FORCE_TIER",
                "EGCL_GC_DISABLE",
            ] {
                command.env_remove(key);
            }
            command
                .env(PUBLICATION_CHILD, if moving { "moving" } else { "normal" })
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
            if moving {
                command.env("EGCL_GC_STRESS", "1");
            }
            let output = command.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "moving={moving}: {}\n{stdout}\n{stderr}",
                output.status
            );
            assert!(
                stdout.contains("MOP-PUBLICATION-METADATA-OK"),
                "{stdout}\n{stderr}"
            );
            if moving {
                assert!(
                    stdout.contains("MOP-PUBLICATION-RELOCATED"),
                    "{stdout}\n{stderr}"
                );
            }
        }
    }

    #[test]
    #[ignore = "spawned fresh because allocation stress configuration is cached"]
    fn publication_updates_detached_argument_copy() {
        let moving = std::env::var(PUBLICATION_CHILD).unwrap() == "moving";
        let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        BOOT_COMPLETE.with(|flag| flag.set(false));
        read_eval_all_env(EMBEDDED_BOOT_LISP, &mut env).unwrap();
        BOOT_COMPLETE.with(|flag| flag.set(true));
        read_eval_all_env(
            "(defclass bridge-publication () ((value :initform 1)))",
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(
            class = read_eval_all_env("(find-class 'bridge-publication)", &mut env).unwrap()
        );
        egcl_rt::rooted!(function = read_eval_all_env("#'(lambda () 73)", &mut env).unwrap());
        let publish = resolve_sym(":PUBLISH").unwrap();
        let keyword = resolve_sym(":ANSWER").unwrap();
        let funcall = resolve_sym("FUNCALL").unwrap();
        egcl_rt::gc::collect_t0_minor().unwrap();
        // Only ordinary cons allocation follows setup, so these fresh outer
        // metadata lists can actually evacuate at the bridge's first snapshot.
        egcl_rt::rooted!(entry = vec_to_list(&[keyword, NIL, *function]));
        egcl_rt::rooted!(defaults = vec_to_list(&[*entry]));
        egcl_rt::rooted!(invocation = vec_to_list(&[funcall, *function]));
        egcl_rt::rooted!(runtime_entry = vec_to_list(&[keyword, *invocation]));
        egcl_rt::rooted!(runtime_defaults = vec_to_list(&[*runtime_entry]));
        egcl_rt::rooted!(
            cpl = vec_to_list(
                &egcl_stdlib::clos::compute_class_precedence_list_default(*class).unwrap()
            )
        );
        let effective = egcl_stdlib::clos::class_slot_metaobjects(*class, false).0;
        egcl_rt::rooted!(
            original = vec![
                publish,
                *class,
                *cpl,
                effective,
                *defaults,
                *runtime_defaults
            ]
        );
        let detached_arguments = original.to_vec();
        let before = original[4].to_raw();
        egcl_rt::gc::stress_arm_after_init();
        assert_eq!(
            call("%MOP-CLASS-PROTOCOL", &detached_arguments, &mut env).unwrap(),
            *class
        );
        if moving {
            assert_ne!(
                before,
                original[4].to_raw(),
                "young metadata must move inside PUBLISH"
            );
            println!("MOP-PUBLICATION-RELOCATED");
        }
        assert_eq!(
            egcl_stdlib::clos::class_protocol_default_initargs(*class),
            Some(original[4])
        );
        assert_eq!(
            egcl_stdlib::clos::class_protocol_runtime_defaults(*class),
            Some(original[5])
        );
        egcl_rt::rooted!(
            saved = egcl_stdlib::clos::class_protocol_default_initargs(*class).unwrap()
        );
        egcl_rt::rooted!(saved_function = cp(cp(cp(*saved).0).1).1);
        assert_eq!(cp(*saved_function).0, *function);
        assert_eq!(
            apply_function(*function, &[], &mut env).unwrap(),
            EgclVal::from_fixnum(73)
        );
        println!("MOP-PUBLICATION-METADATA-OK");
    }

    #[test]
    fn failed_superclass_finalization_keeps_descendant_wrappers_and_fast_paths_retired() {
        let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        BOOT_COMPLETE.with(|flag| flag.set(false));
        read_eval_all_env(EMBEDDED_BOOT_LISP, &mut env).unwrap();
        BOOT_COMPLETE.with(|flag| flag.set(true));
        read_eval_all_env(
            r#"
            (defvar *ancestor-fail* nil)
            (defvar *ancestor-attempts* 0)
            (defclass ancestor-meta (standard-class) ())
            (defmethod egcl-mop:validate-superclass ((class ancestor-meta) (super standard-class)) t)
            (defmethod egcl-mop:compute-slots ((class ancestor-meta))
              (when (and *ancestor-fail* (eq (class-name class) 'ancestor-parent))
                (incf *ancestor-attempts*) (error "parent finalization rejected"))
              (call-next-method))
            (defmethod egcl-mop:validate-superclass ((class standard-class) (super ancestor-meta)) t)
            (defclass ancestor-parent () ((kept :initform 1 :accessor ancestor-kept)) (:metaclass ancestor-meta))
            (defclass ancestor-child (ancestor-parent) ((own :initform 2)) )
            (defparameter *ancestor-object* (make-instance 'ancestor-child))
            (setf (slot-value *ancestor-object* 'kept) 777)
            (dotimes (i 20) (assert (= (ancestor-kept *ancestor-object*) 777)))
            "#,
            &mut env,
        ).unwrap();
        egcl_rt::rooted!(object = read_eval_all_env("*ancestor-object*", &mut env).unwrap());
        egcl_rt::rooted!(
            parent = egcl_stdlib::find_class(resolve_sym("ANCESTOR-PARENT").unwrap()).unwrap()
        );
        egcl_rt::rooted!(child = egcl_stdlib::class_of(*object));
        let old_layout = egcl_stdlib::clos::instance_layout_key(*object).unwrap();
        assert!(slot_access::pristine(
            *object,
            slot_access::Operation::Read,
            &env
        ));
        read_eval_all_env(
            "(setq *ancestor-fail* t) (handler-case (defclass ancestor-parent () ((temporary :initform 3)) (:metaclass ancestor-meta)) (error () nil))",
            &mut env,
        ).unwrap();
        for source in [
            "(slot-value *ancestor-object* 'kept)",
            "(ancestor-kept *ancestor-object*)",
            "(make-instance 'ancestor-child)",
            "(egcl-mop:finalize-inheritance (find-class 'ancestor-child))",
        ] {
            let before = read_eval_all_env("*ancestor-attempts*", &mut env)
                .unwrap()
                .as_fixnum();
            assert!(read_eval_all_env(source, &mut env).is_err(), "{source}");
            assert_eq!(
                read_eval_all_env("*ancestor-attempts*", &mut env)
                    .unwrap()
                    .as_fixnum(),
                before + 1,
                "the parent hook must cause the rejection"
            );
            assert!(!egcl_stdlib::clos::class_protocol_finalized(*parent));
            assert!(!egcl_stdlib::clos::class_protocol_finalized(*child));
            assert_eq!(
                egcl_stdlib::clos::class_layout_key(*child),
                Some(old_layout)
            );
            assert_eq!(
                egcl_stdlib::clos::instance_layout_key(*object),
                Some(old_layout)
            );
            assert!(!slot_access::pristine(
                *object,
                slot_access::Operation::Read,
                &env
            ));
            assert_eq!(
                read_eval_all_env(
                    "(egcl-internal::%raw-slot-value *ancestor-object* 'kept)",
                    &mut env
                )
                .unwrap(),
                EgclVal::from_fixnum(777)
            );
        }
        read_eval_all_env(
            "(setq *ancestor-fail* nil) (defclass ancestor-parent () ((added :initform 3) (kept :initform 4)) (:metaclass ancestor-meta)) (assert (= (slot-value *ancestor-object* 'kept) 777)) (assert (= (slot-value *ancestor-object* 'added) 3))",
            &mut env,
        ).unwrap();
        assert!(egcl_stdlib::clos::class_protocol_finalized(*child));
        assert_ne!(
            egcl_stdlib::clos::class_layout_key(*child),
            Some(old_layout)
        );
    }
}
