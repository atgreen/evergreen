// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Real emitted unwind maps over a bounded synthetic physical stack. This
//! exercises owner changes without pretending the ordinary call emitter exists.
use super::*;
use crate::cli::{Env, reader};
use crate::cli::bytecode::{compile_function, call_table::OrdinaryCodeDirectory};
use egcl_compiler::t2::x64_calls::{NativeCallOrigin, NativeStackBase};
use std::{rc::Rc, sync::Arc};

fn compiled(name: &str, form: &str, env: &Env) -> Rc<TransferCode> {
    egcl_rt::rooted!(params = reader::read_from_string("(x y)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(form).unwrap().0);
    let body = Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap());
    Rc::new(TransferCode::compile(body).expect("compiled frame-map fixture"))
}

#[test]
fn heterogeneous_owners_preserve_composed_register_home_addresses() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let inner = compiled("OWNER-INNER", "((let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 3) (go again)))) (cons x y))", &env);
    let outer = compiled("OWNER-OUTER", "((let ((n 0)) (tagbody again (setq n (+ n 2)) (if (< n 6) (go again)))) (cons y x))", &env);
    let directory = Rc::new(OrdinaryCodeDirectory::for_walk_test(&[(Rc::clone(&outer), outer.code.as_ptr() as usize, outer.code_len)]));
    // The initial owner deliberately is not registered: the boundary retains it.
    assert!(directory.code_for_pc(inner.code.as_ptr() as usize).is_none());
    let codes = [&*inner, &*outer, &*inner];
    let sites: Vec<_> = codes.iter().map(|code| {
        *code._native_calls.iter().find(|site| matches!(site.origin, NativeCallOrigin::LoopPoll(_))).expect("loop poll")
    }).collect();
    assert!(sites.iter().all(|site| site.stack_base == NativeStackBase::Body));
    for code in codes {
        assert_eq!(code._native_calls.unwind_recipe().unwrap().saved_offset(5), None,
            "legacy fixture deliberately leaves RBP unsaved");
    }
    let mut words = vec![0usize; 4096];
    let low = words.as_mut_ptr() as usize;
    let high = low + words.len() * 8;
    let initial_homes = std::array::from_fn(|i| Some(low + i * 8));
    let mut sp = low + 8 * 8;
    let first_sp = sp;
    let mut body_sps = Vec::new();
    for (index, code) in codes.iter().enumerate() {
        let body = sp + sites[index].stack_adjust as usize;
        body_sps.push(body);
        let entry = body + code._native_calls.unwind_recipe().unwrap().frame_bytes() as usize;
        assert!(entry + 8 < high);
        let return_pc = if index + 1 < codes.len() {
            codes[index + 1].code.as_ptr() as usize + sites[index + 1].return_offset
        } else { 0x12345678 };
        words[(entry - low) / 8] = return_pc;
        sp = entry + 8;
    }
    let saved_rbx = body_sps[0] + inner._native_calls.unwind_recipe().unwrap()
        .saved_offset(3).expect("inner frame saves the outer RBX") as usize;
    assert_ne!(Some(saved_rbx), initial_homes[0]);
    let outer_saved_rbx = body_sps[1] + outer._native_calls.unwind_recipe().unwrap()
        .saved_offset(3).expect("outer frame saves its caller's RBX") as usize;
    words[(outer_saved_rbx - low) / 8] = 0xaaa0;
    words[(saved_rbx - low) / 8] = 0xabc0;
    let boundary = PublishedBoundary {
        directory: Rc::clone(&directory),
        owner: Rc::as_ptr(&inner), context: std::ptr::null_mut(),
        cursor: NativeFrameCursor {
            pc: inner.code.as_ptr() as usize + sites[0].return_offset,
            call_sp: first_sp, registers: initial_homes,
        },
        bounds: low..high, segment: std::ptr::null_mut(),
        stack: egcl_rt::current_stack(), previous: std::ptr::null_mut(), visited: Cell::new(0),
    };
    let mut owners = Vec::new();
    unsafe { walk_boundary_frames_to(&boundary, (sp, 0x12345678), false,
        &mut |code, _, _, cursor, body, _| {
            owners.push(code as *const TransferCode);
            let index = owners.len() - 1;
            assert_eq!(body, Some(body_sps[index]));
            if index == 2 {
                assert_eq!(cursor.registers[0], Some(outer_saved_rbx));
                assert_eq!(*(outer_saved_rbx as *const usize), 0xaaa0);
            }
            assert_eq!(cursor.registers[1], initial_homes[1],
                "unsaved RBP inherits its original home across every owner");
            if index == 1 {
                assert_eq!(cursor.registers[0], Some(saved_rbx), "outer RBX lives in inner save slot");
                assert_eq!(*(saved_rbx as *const usize), 0xabc0);
                // A visitor must update the inherited physical home, not a copy.
                *(cursor.registers[0].unwrap() as *mut usize) = 0xdef0;
            }
            1
        }); }
    assert_eq!(owners, vec![Rc::as_ptr(&inner), Rc::as_ptr(&outer), Rc::as_ptr(&inner)]);
    assert_eq!(words[(saved_rbx - low) / 8], 0xdef0);
    assert_eq!(words[0], 0, "the initial RBX home belongs to another frame");
}
