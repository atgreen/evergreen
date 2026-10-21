// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Readtable enumeration for `EGCL-EXT:MAP-READTABLE-MACRO-CHARACTERS`
//! (GitHub #473). Libraries that copy syntax between readtables
//! (named-readtables' `merge-readtables-into`) otherwise have to probe every
//! one of the 1,114,112 character codes with GET-MACRO-CHARACTER.

use egcl_compiler::reader;
use egcl_rt::object::{ConsCell, type_id};
use egcl_rt::value::{EgclVal, NIL};

fn alloc_cons(car: EgclVal, cdr: EgclVal) -> EgclVal {
    egcl_rt::rooted!(car = car);
    egcl_rt::rooted!(cdr = cdr);
    let body = match egcl_rt::gc::alloc_typed(16, type_id::CONS) {
        Some(b) => b,
        None => std::alloc::handle_alloc_error(std::alloc::Layout::new::<ConsCell>()),
    };
    unsafe {
        let cell = body as *mut ConsCell;
        (*cell).car = *car;
        (*cell).cdr = *cdr;
        EgclVal::from_cons_ptr(body)
    }
}

/// The macro-character candidates of `readtable` as a Lisp list, sorted by
/// character code. A character that is not a dispatching macro character is
/// listed as itself; a dispatching one as `(char sub-char ...)`.
///
/// The list is a superset (see
/// [`reader::readtable_macro_character_candidates`]): the Lisp caller keeps
/// only the entries GET-MACRO-CHARACTER and GET-DISPATCH-MACRO-CHARACTER
/// report a function for, so the enumeration always agrees with them.
pub fn macro_character_candidates(readtable: EgclVal) -> EgclVal {
    // Collect everything (no heap allocation, readtable locks released) before
    // consing: characters are immediates, so only the list under construction
    // needs a root.
    let candidates = reader::readtable_macro_character_candidates(readtable);
    egcl_rt::rooted!(result = NIL);
    egcl_rt::rooted!(entry = NIL);
    for candidate in candidates.iter().rev() {
        let ch = EgclVal::from_char(candidate.ch);
        *entry = match &candidate.sub_chars {
            None => ch,
            Some(subs) => {
                *entry = NIL;
                for &sub in subs.iter().rev() {
                    *entry = alloc_cons(EgclVal::from_char(sub), *entry);
                }
                alloc_cons(ch, *entry)
            }
        };
        *result = alloc_cons(*entry, *result);
    }
    *result
}
