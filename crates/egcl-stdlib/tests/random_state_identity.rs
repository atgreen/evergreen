// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::value::{NIL, T};
use egcl_rt::{Collector, HeapCollector};
use egcl_stdlib::{MakeHashTableOptions, random_state};

#[test]
fn state_copies_are_independent_and_hash_identity_survives_mutation_and_gc() {
    egcl_rt::rooted!(state = random_state::make_random_state(T, NIL).unwrap());
    let fresh_seed = egcl_rt::random_state::snapshot(*state);
    egcl_rt::rooted!(copy = random_state::make_random_state(*state, NIL).unwrap());
    assert_eq!(egcl_rt::random_state::snapshot(*state), fresh_seed);
    assert_ne!(*state, *copy);
    assert!(!egcl_stdlib::equality::equal(*state, *copy));
    assert!(!egcl_stdlib::equality::equalp(*state, *copy));
    let hash = egcl_stdlib::sxhash(*state).as_fixnum();
    egcl_rt::rooted!(
        table = egcl_stdlib::make_hash_table(&MakeHashTableOptions::default()).unwrap()
    );
    egcl_stdlib::set_gethash(*state, *table, egcl_rt::EgclVal::from_fixnum(77)).unwrap();
    for _ in 0..32 {
        assert_eq!(
            random_state::draw(*state).unwrap(),
            random_state::draw(*copy).unwrap()
        );
    }
    let snapshot = egcl_rt::random_state::snapshot(*copy);
    random_state::draw(*state).unwrap();
    assert_eq!(egcl_rt::random_state::snapshot(*copy), snapshot);
    assert_eq!(egcl_stdlib::sxhash(*state).as_fixnum(), hash);
    let before = state.to_raw();
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(state.to_raw(), before, "hash key must actually move");
    assert_eq!(egcl_stdlib::sxhash(*state).as_fixnum(), hash);
    assert_eq!(
        egcl_stdlib::gethash(*state, *table, NIL).unwrap(),
        (egcl_rt::EgclVal::from_fixnum(77), true)
    );
    HeapCollector::new().major_gc().unwrap();
    assert_eq!(egcl_stdlib::sxhash(*state).as_fixnum(), hash);
    assert_eq!(
        egcl_stdlib::gethash(*state, *table, NIL).unwrap(),
        (egcl_rt::EgclVal::from_fixnum(77), true)
    );
}

#[test]
fn random_state_apis_reject_registered_strings_and_pathnames() {
    use egcl_stdlib::{pathnames, streams};
    egcl_rt::rooted!(text = streams::make_lisp_string("random-state-control"));
    pathnames::register_string(*text, "random-state-control");
    egcl_rt::rooted!(path = pathnames::parse_namestring(*text, None, None).unwrap().0);
    for value in [*text, *path] {
        assert!(!egcl_stdlib::random_state::random_state_p(value));
        assert_eq!(egcl_rt::random_state::snapshot(value), None);
        assert!(egcl_stdlib::random_state::draw(value).is_err());
        assert!(egcl_stdlib::random_state::make_random_state(value, egcl_rt::value::NIL).is_err());
    }
}
