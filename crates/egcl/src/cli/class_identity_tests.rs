// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
fn anonymous_class_metaobjects_keep_instance_and_class_membership() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = resolve_sym("ANONYMOUS-CLASS-IDENTITY-UNIT").unwrap();
    egcl_rt::rooted!(class = egcl_stdlib::clos::new_class_metaobject(NIL).unwrap());
    egcl_stdlib::clos::define_class(name, *class, &[], &[]).unwrap();
    egcl_rt::rooted!(object = egcl_stdlib::clos::allocate_instance(*class).unwrap());
    // The internal registration API can set the recorded name independently
    // of aliases. This exercises a truly unnamed class without depending on
    // the separate public (SETF CLASS-NAME) implementation.
    egcl_stdlib::clos::set_find_class(NIL, *class).unwrap();
    egcl_stdlib::clos::bind_class_name(name, NIL).unwrap();
    egcl_stdlib::clos::bind_class_name(NIL, NIL).unwrap();
    assert!(egcl_stdlib::clos::class_name(*class).is_nil());
    assert!(egcl_stdlib::clos::is_class_object(*class));
    assert_eq!(egcl_stdlib::class_of(*object), *class);
    assert!(typep_matches(&mut env, *object, *class).unwrap());
    assert_eq!(class_metaobject_of(*class), Some(*class));
}
