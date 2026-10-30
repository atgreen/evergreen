// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::{macroexpand::*, reader};
use egcl_rt::{
    EgclVal,
    object::{ConsCell, type_id},
    value::NIL,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn cons(car: EgclVal, cdr: EgclVal) -> EgclVal {
    egcl_rt::rooted!(car = car);
    egcl_rt::rooted!(cdr = cdr);
    let body = egcl_rt::gc::alloc_typed(16, type_id::CONS).unwrap();
    unsafe {
        body.cast::<ConsCell>().write(ConsCell {
            car: *car,
            cdr: *cdr,
        });
    }
    EgclVal::from_cons_ptr(body)
}

fn nth(mut form: EgclVal, index: usize) -> EgclVal {
    for _ in 0..index {
        assert!(form.is_cons());
        form = unsafe { (*form.as_ptr().cast::<ConsCell>()).cdr };
    }
    assert!(form.is_cons());
    unsafe { (*form.as_ptr().cast::<ConsCell>()).car }
}

struct Registration(EgclVal);
impl Drop for Registration {
    fn drop(&mut self) {
        unregister_macro_function(self.0);
    }
}

#[test]
fn body_retains_a_rewrite_when_the_original_macro_call_moves() {
    egcl_rt::gc::ensure_heap_initialized();
    let rewrite = EgclVal::from_symbol_index(reader::intern_symbol("BODY-GC-REWRITE"));
    let block = EgclVal::from_symbol_index(reader::intern_symbol("BLOCK"));
    let quote = EgclVal::from_symbol_index(reader::intern_symbol("QUOTE"));
    let key = EgclVal::from_macro_handle(0x7272697465);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    register_macro_function(
        key,
        Arc::new(move |original, _| {
            egcl_rt::rooted!(original = original);
            let before = original.to_raw();
            egcl_rt::gc::collect_t0_minor()?;
            assert_ne!(
                original.to_raw(),
                before,
                "the callback must relocate its input"
            );
            observed_calls.fetch_add(1, Ordering::SeqCst);
            // Allocate the replacement head first, giving the reclaimed input
            // address a chance to be reused before building its argument tail.
            egcl_rt::rooted!(replacement = cons(quote, NIL));
            egcl_rt::rooted!(tail = cons(EgclVal::from_fixnum(42), NIL));
            unsafe {
                let slot = &raw mut (*replacement.as_ptr().cast::<ConsCell>()).cdr;
                egcl_rt::gc::write_barrier(slot, NIL, *tail);
                slot.write(*tail);
            }
            Ok(*replacement)
        }),
    );
    let _registration = Registration(key);
    egcl_rt::rooted!(
        environment = Environment::null().augment_function(rewrite, FunctionInfo::Macro(key))
    );
    // Drain symbol initialization before constructing fresh movable source.
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(original = cons(rewrite, NIL));
    let before = original.to_raw();
    egcl_rt::rooted!(body = cons(*original, NIL));
    egcl_rt::rooted!(arguments = cons(NIL, *body));
    egcl_rt::rooted!(source = cons(block, *arguments));
    egcl_rt::rooted!(expanded = macroexpand_all(*source, &environment).unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_ne!(original.to_raw(), before, "source must actually move");
    assert_eq!(nth(*expanded, 0), block);
    assert_eq!(nth(*expanded, 1), NIL);
    let result = nth(*expanded, 2);
    assert_eq!(
        nth(result, 0),
        quote,
        "the rewritten body must survive rebuilding"
    );
    assert_eq!(nth(result, 1), EgclVal::from_fixnum(42));
    assert_eq!(
        nth(*original, 0),
        rewrite,
        "expansion must not mutate source"
    );
    assert_ne!(*expanded, *source, "changed body requires a rebuilt BLOCK");
}
