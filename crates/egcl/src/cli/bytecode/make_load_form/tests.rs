// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

fn action(kind: u8, target: u32, form: u32) -> BbuLoadAction {
    BbuLoadAction {
        kind,
        flags: 0,
        arg0: target,
        arg1: form,
        arg2: BBU_NO_INDEX,
    }
}

#[test]
fn malformed_deferred_plans_are_rejected_before_execution() {
    let constants = vec![
        BbuConstant::LoadFormObject,
        BbuConstant::Nil,
        BbuConstant::Cons(0, 1),
    ];
    let create = action(CREATE, 0, 1);
    let initialize = action(INITIALIZE, 0, 1);
    let consume = action(9, 2, BBU_NO_INDEX);
    assert!(validate(&constants, &[], &[create, initialize, consume], 0x0206).is_ok());
    for actions in [
        vec![],
        vec![initialize],
        vec![create],
        vec![create, create, initialize],
        vec![create, initialize, initialize],
        vec![create, consume, initialize],
        vec![action(CREATE, 1, 1), initialize],
        vec![action(CREATE, 0, 99), initialize],
        vec![action(CREATE, 0, BBU_NO_INDEX), initialize],
    ] {
        assert!(validate(&constants, &[], &actions, 0x0206).is_err());
    }
    assert!(validate(&constants, &[], &[create, initialize], 0x0205).is_err());
    let mut bad_flags = create;
    bad_flags.flags = 1;
    assert!(validate(&constants, &[], &[bad_flags, initialize], 0x0206).is_err());
}

#[test]
fn mutual_initialization_references_are_allowed_but_creation_cycles_are_not() {
    let constants = vec![
        BbuConstant::LoadFormObject,
        BbuConstant::LoadFormObject,
        BbuConstant::Nil,
        BbuConstant::Cons(0, 2),
        BbuConstant::Vector(vec![1]),
    ];
    let valid = [
        action(CREATE, 0, 2),
        action(CREATE, 1, 2),
        action(INITIALIZE, 0, 4),
        action(INITIALIZE, 1, 3),
    ];
    assert!(validate(&constants, &[], &valid, 0x0206).is_ok());
    let invalid = [
        action(CREATE, 0, 4),
        action(CREATE, 1, 3),
        action(INITIALIZE, 0, 2),
        action(INITIALIZE, 1, 2),
    ];
    assert!(validate(&constants, &[], &invalid, 0x0206).is_err());
}

#[test]
fn deferred_hashes_keep_backward_references_and_initialization_readiness() {
    for test in [0, 1, 2, 3] {
        let constants = vec![
            BbuConstant::LoadFormObject,
            BbuConstant::Nil,
            BbuConstant::HashTable {
                test,
                entry_refs: vec![0, 1],
            },
        ];
        let actions = [action(CREATE, 0, 1), action(INITIALIZE, 0, 2)];
        assert_eq!(
            validate(&constants, &[], &actions, 0x0206).is_ok(),
            test < 2
        );
    }
    let constants = vec![
        BbuConstant::LoadFormObject,
        BbuConstant::HashTable {
            test: 0,
            entry_refs: vec![0, 2],
        },
        BbuConstant::Nil,
    ];
    assert!(validate(
        &constants,
        &[],
        &[action(CREATE, 0, 2), action(INITIALIZE, 0, 2)],
        0x0206
    )
    .is_err());
}

#[test]
fn function_metadata_and_nested_record_literals_require_initialized_objects() {
    fn function(literals: Vec<u32>, lambda_list: u32) -> EncodedBbuFunction {
        EncodedBbuFunction {
            self_call_sites: vec![],
            notinline_sites: vec![],
            name_ref: BBU_NO_INDEX,
            lambda_list_ref: lambda_list,
            flags: BBU_FUNC_LOAD_TIME_THUNK,
            arity_min: 0,
            arity_max: 0,
            n_locals: 0,
            max_stack: 1,
            code: vec![],
            literal_refs: literals,
            metadata: None,
        }
    }
    let constants = vec![
        BbuConstant::LoadFormObject,
        BbuConstant::Nil,
        BbuConstant::Cons(0, 1),
    ];
    for functions in [
        vec![function(vec![0], BBU_NO_INDEX)],
        vec![function(vec![], 2)],
        vec![
            function(vec![0], BBU_NO_INDEX),
            function(vec![], BBU_NO_INDEX),
        ],
    ] {
        let consume = action(7, (functions.len() - 1) as u32, BBU_NO_INDEX);
        assert!(validate(&constants, &functions, &[consume], 0x0206).is_err());
        assert!(validate(
            &constants,
            &functions,
            &[action(CREATE, 0, 1), action(INITIALIZE, 0, 1), consume],
            0x0206
        )
        .is_ok());
    }
}

#[test]
fn identity_registry_and_fixup_owners_follow_actual_relocation() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    egcl_rt::rooted!(pool = BbuConstPool::default());
    // Real instance allocation supplies the same representation passed to MLF.
    super::super::super::read_eval_all_env("(defstruct mlf-root-probe value)", &mut env).unwrap();
    let original =
        super::super::super::read_eval_all_env("(make-mlf-root-probe :value 42)", &mut env)
            .unwrap();
    let reference = pool.reserve_load_form(original);
    let second =
        super::super::super::read_eval_all_env("(make-mlf-root-probe :value 42)", &mut env)
            .unwrap();
    assert_ne!(
        pool.reserve_load_form(second),
        reference,
        "equal instances must not coalesce"
    );
    pool.load_forms.records[0].creation = arena_cons(EgclVal::from_fixnum(17), NIL);
    pool.load_forms.records[0].initialization = arena_cons(EgclVal::from_fixnum(18), NIL);
    let original_creation = pool.load_forms.records[0].creation;
    let original_initialization = pool.load_forms.records[0].initialization;
    let encoded = vec![
        BbuConstant::LoadFormObject,
        BbuConstant::Nil,
        BbuConstant::Cons(0, 1),
        BbuConstant::Vector(vec![0, 2]),
    ];
    egcl_rt::rooted!(values = materialize_bbu_constants(&encoded).unwrap());
    let original_owner = values[2];
    super::super::super::store_cons_field(values[2], EgclVal::from_fixnum(99), false).unwrap();
    egcl_rt::collect_t0_minor().unwrap();
    let relocated = pool.load_forms.records[0].object;
    assert_ne!(
        original, relocated,
        "the registered instance must really move"
    );
    assert_ne!(
        original_creation, pool.load_forms.records[0].creation,
        "creation form must move"
    );
    assert_ne!(
        original_initialization, pool.load_forms.records[0].initialization,
        "initialization form must move"
    );
    assert_ne!(original_owner, values[2], "the fixup owner must move");
    assert_eq!(
        pool.reserve_load_form(relocated),
        reference,
        "memo lookup must follow relocation"
    );
    values[0] = relocated;
    let runtime = Runtime::new(&encoded).unwrap();
    runtime.apply_fixups(0, &values).unwrap();
    assert_eq!(
        cp(values[2]),
        (relocated, EgclVal::from_fixnum(99)),
        "unrelated mutation must survive fixup"
    );
    assert_eq!(egcl_stdlib::elt(values[3], 0).unwrap(), relocated);
    eprintln!(
        "MLF-ROOTS-RELOCATED {:#x}->{:#x}",
        original.to_raw(),
        relocated.to_raw()
    );
}

#[test]
fn independent_initialization_closure_precedes_dependent_creation() {
    let mut pool = BbuConstPool {
        entries: vec![vec![30], vec![30], vec![30], vec![0]],
        ..Default::default()
    };
    for (reference, creation_deps, initialization_deps) in [
        (0, vec![1], vec![0]),
        (1, vec![], vec![1, 2]),
        (2, vec![], vec![2]),
    ] {
        pool.load_forms.records.push(Record {
            object: NIL,
            reference,
            creation: NIL,
            initialization: NIL,
            prepared: Some(Prepared {
                creation: 3,
                initialization: 3,
                creation_deps,
                initialization_deps,
            }),
            created: false,
            initialized: false,
        });
    }
    let actions = schedule(&mut pool, &[0, 1, 2]).unwrap();
    let where_is = |kind, object| {
        actions
            .iter()
            .position(|a| a.0 == kind && a.2 == object)
            .unwrap()
    };
    assert!(where_is(CREATE, 2) < where_is(INITIALIZE, 2));
    assert!(where_is(INITIALIZE, 2) < where_is(INITIALIZE, 1));
    assert!(where_is(INITIALIZE, 1) < where_is(CREATE, 0));
}

#[test]
fn printed_constants_cannot_observe_uninitialized_placeholders() {
    let constants = vec![BbuConstant::LoadFormObject, BbuConstant::Nil];
    let actions = [action(CREATE, 0, 1), action(INITIALIZE, 0, 1)];
    let early = [BbuPrintGroup {
        end: 0,
        result_kind: 2,
        result: 0,
    }];
    let ready = [BbuPrintGroup {
        end: 2,
        result_kind: 2,
        result: 0,
    }];
    assert!(validate_print_groups(&constants, &actions, Some(&early)).is_err());
    assert!(validate_print_groups(&constants, &actions, Some(&ready)).is_ok());
}
