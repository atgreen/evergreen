// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAllocator;
thread_local! {
    static COUNT: Cell<bool> = const { Cell::new(false) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}
fn record(size: usize) {
    if COUNT.try_with(Cell::get).unwrap_or(false) {
        BYTES.with(|bytes| bytes.set(bytes.get() + size));
    }
}
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct RestoreFrame(Option<Arc<SharedCell<EnvFrame>>>);
impl Drop for RestoreFrame {
    fn drop(&mut self) {
        NATIVE_ENV_FRAME.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

#[test]
fn native_captured_read_and_assignment_do_not_allocate() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let symbol = resolve_sym("NATIVE-NO-ALLOC-CAPTURE").unwrap();
    env.define_local_symbol(symbol, EgclVal::from_fixnum(17));
    egcl_rt::rooted!(form = reader::read_from_string("(nil)").unwrap().0);
    let mut body = compile_function("NATIVE-NO-ALLOC", NIL, *form, &env, false, false).unwrap();
    body.names = vec!["NATIVE-NO-ALLOC-CAPTURE".into()];
    let names = NativeEnvNames::new(&body);
    let _restore =
        RestoreFrame(NATIVE_ENV_FRAME.with(|slot| slot.replace(Some(Arc::clone(&env.frame)))));
    assert_eq!(c2i_load_env(&*names, 0), EgclVal::from_fixnum(17).0);
    c2i_store_env(&*names, 0, EgclVal::from_fixnum(19).0);
    BYTES.with(|bytes| bytes.set(0));
    COUNT.with(|count| count.set(true));
    let mut answer = 0;
    for _ in 0..100 {
        answer = std::hint::black_box(c2i_load_env(&*names, 0));
        c2i_store_env(&*names, 0, answer);
    }
    COUNT.with(|count| count.set(false));
    assert_eq!(answer, EgclVal::from_fixnum(19).0);
    assert_eq!(
        BYTES.with(Cell::get),
        0,
        "repeated native lexical accesses allocated"
    );
    assert!(!native_error_pending());
}

#[test]
fn native_names_preserve_shadowing_late_interning_and_renames() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let key = "NATIVE-SNAPSHOT-OLD::VALUE";
    let symbol = egcl_rt::symbols::intern(key);
    env.define_local_symbol(EgclVal::from_symbol_index(symbol), EgclVal::from_fixnum(11));
    let outer = Arc::clone(&env.frame);
    env.frame = Arc::new(SharedCell::new(EnvFrame {
        vars: super::super::VecMap::default(),
        symbol_vars: super::super::VecMap::default(),
        parent: Some(Arc::clone(&outer)),
    }));
    env.frame
        .borrow_mut()
        .vars
        .insert(key.into(), EgclVal::from_fixnum(29));
    egcl_rt::rooted!(form = reader::read_from_string("(nil)").unwrap().0);
    let mut body =
        compile_function("NATIVE-NAME-SEMANTICS", NIL, *form, &env, false, false).unwrap();
    let late_key = "NATIVE-SNAPSHOT-LATE";
    assert_eq!(egcl_rt::symbols::find_index(late_key), None);
    body.names = vec![key.into(), late_key.into()];
    let names = NativeEnvNames::new(&body);
    assert_eq!(
        egcl_rt::symbols::find_index(late_key),
        None,
        "compilation must not intern a name"
    );
    let _restore =
        RestoreFrame(NATIVE_ENV_FRAME.with(|slot| slot.replace(Some(Arc::clone(&env.frame)))));
    assert_eq!(
        c2i_load_env(&*names, 0),
        EgclVal::from_fixnum(29).0,
        "inner name-only binding must shadow outer symbol binding"
    );
    c2i_store_env(&*names, 0, EgclVal::from_fixnum(31).0);
    assert_eq!(
        Env::lookup_frame(&outer, key),
        Some(EgclVal::from_fixnum(11))
    );
    assert_eq!(c2i_load_env(&*names, 0), EgclVal::from_fixnum(31).0);

    // The same table must see a symbol registered after native compilation.
    let late_symbol = egcl_rt::symbols::intern(late_key);
    env.frame
        .borrow_mut()
        .symbol_vars
        .insert(late_symbol, EgclVal::from_fixnum(43));
    assert_eq!(c2i_load_env(&*names, 1), EgclVal::from_fixnum(43).0);

    // Renaming changes the name lookup; reusing the old name creates another
    // identity. A stale cached index would incorrectly return 31 here.
    assert_eq!(
        egcl_rt::symbols::rename_package_prefix("NATIVE-SNAPSHOT-OLD", "NATIVE-SNAPSHOT-NEW").len(),
        1
    );
    let replacement = egcl_rt::symbols::intern(key);
    assert_ne!(symbol, replacement);
    env.frame
        .borrow_mut()
        .symbol_vars
        .insert(replacement, EgclVal::from_fixnum(47));
    assert_eq!(c2i_load_env(&*names, 0), EgclVal::from_fixnum(47).0);
    c2i_store_env(&*names, 0, EgclVal::from_fixnum(53).0);
    assert_eq!(
        env.frame.borrow().symbol_vars.get(&replacement),
        Some(&EgclVal::from_fixnum(53))
    );
    c2i_define_env(&*names, 1, EgclVal::from_fixnum(59).0);
    assert_eq!(
        env.frame.borrow().vars.get(late_key),
        Some(&EgclVal::from_fixnum(59))
    );
    assert_eq!(c2i_load_env(&*names, 1), EgclVal::from_fixnum(59).0);
    assert!(!native_error_pending());
}
