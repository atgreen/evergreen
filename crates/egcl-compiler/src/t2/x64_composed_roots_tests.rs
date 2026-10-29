// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execute real save/reuse/restore instructions against production unwind and
//! value maps. The CLI retained-owner directory has separate integration tests;
//! this fixture deliberately does not widen ordinary body admission for testing.

use super::*;
use crate::t2::ir::{Inst, Value, ValueRepresentation};
use crate::t2::x64_frame::{FrameValue, FrameValueLocation, ValueHome};
use crate::t2::x64_unwind::SYSV_PRESERVED_REGISTERS;
use crate::t2::x64_value_maps::{NativeFrameLayout, NativeValueArea, NativeValueLocation};
use egcl_rt::EgclVal;
use egcl_rt::gc::TraceHostRoots;
use egcl_rt::jit::JitBuffer;
use std::cell::Cell;

struct Owner {
    code: JitBuffer,
    sites: NativeCallSites,
}

// Each body has its own executable allocation and exact CALL return PC. A
// keeps X in RBX; B saves X and reuses RBX for Y; C saves Y and reuses RBX for
// a scalar. After collection the actual POPs must restore both moved pointers.
fn owner(next: *const u8, level: usize) -> Owner {
    let mut code = vec![0x55, 0x48, 0x89, 0xe5, 0x53]; // push rbp; mov rbp,rsp; push rbx
    let saved = if level == 0 {
        code.extend_from_slice(&[0x41, 0x54]); // push r12 (output address, not a root)
        code.extend_from_slice(&[0x49, 0x89, 0xd4, 0x48, 0x89, 0xfb]); // r12=rdx; rbx=rdi
        vec![5, 3, 12]
    } else {
        code.extend_from_slice(&[0x48, 0x83, 0xec, 8]);
        if level == 1 {
            code.extend_from_slice(&[0x48, 0x89, 0xf3]); // rbx=rsi
        } else {
            code.extend_from_slice(&[0x48, 0xbb]);
            code.extend_from_slice(&EgclVal::from_fixnum(991).to_raw().to_le_bytes());
        }
        vec![5, 3]
    };
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11,next
    code.extend_from_slice(&(next as u64).to_le_bytes());
    code.extend_from_slice(&[0x41, 0xff, 0xd3]); // call r11
    let return_offset = code.len();
    if level == 0 {
        code.extend_from_slice(&[0x49, 0x89, 0x04, 0x24]); // output[0]=returned Y
    }
    code.extend_from_slice(&[0x48, 0x89, 0xd8]); // rax=restored RBX
    if level == 0 {
        code.extend_from_slice(&[0x41, 0x5c]); // pop r12
    } else {
        code.extend_from_slice(&[0x48, 0x83, 0xc4, 8]);
    }
    code.extend_from_slice(&[0x5b, 0x5d, 0xf8, 0xc3]); // pop rbx; pop rbp; clc; ret
    let values: Vec<_> = (level < 2)
        .then_some(FrameValue {
            value: Value(0),
            repr: ValueRepresentation::Tagged,
            location: FrameValueLocation::Home(ValueHome::Reg(3)),
        })
        .into_iter()
        .collect();
    let map = NativeFrameValues::for_call_in_area(
        &values,
        &[],
        NativeValueArea::NativeStack(0),
        &[],
        None,
        NativeFrameLayout {
            native_slots: 3,
            activation_base_slot: None,
            activation_slots: 0,
        },
        None,
        |_| false,
    )
    .unwrap();
    let sites = NativeCallSites::new(
        code.len(),
        vec![NativeCallSite {
            return_offset,
            stack_adjust: 0,
            stack_base: NativeStackBase::Body,
            origin: NativeCallOrigin::Instruction(Inst(0)),
        }],
    )
    .unwrap()
    .with_unwind(NativeUnwindRecipe::from_prologue(&saved, level != 0, 0).unwrap())
    .with_value_maps(HashMap::from([(
        return_offset,
        NativeCallValues::Frame(Arc::new(map)),
    )]))
    .unwrap();
    assert_eq!(sites.unwind_recipe().unwrap().frame_bytes(), 24);
    Owner {
        code: JitBuffer::new(&code).unwrap(),
        sites,
    }
}

struct Check {
    owners: Vec<Owner>, // A, B, C; each is independently retained
    slots: [usize; 2],  // X's B-save slot and Y's C-save slot
    before: [u64; 2],
    after: [u64; 2],
    observed: bool,
    omit_outer: bool,
}

thread_local! {
    static CHECK: Cell<*mut Check> = const { Cell::new(std::ptr::null_mut()) };
}

struct MappedRoots(Vec<*mut EgclVal>);
impl TraceHostRoots for MappedRoots {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for &slot in &self.0 {
            visit(slot);
        }
    }
}

extern "C" fn collect(call_sp: usize, return_pc: usize, captured_rbx: *mut usize) {
    let check = unsafe { &mut *CHECK.with(Cell::get) };
    let registers = std::array::from_fn(|i| Some(unsafe { captured_rbx.sub(i) } as usize));
    let mut cursor = NativeFrameCursor {
        pc: return_pc,
        call_sp,
        registers,
    };
    // The three described frames each occupy24bytes plus their return word.
    let bounds = unsafe { captured_rbx.sub(6) } as usize..call_sp + 3 * 32;
    let read = |address: usize| {
        (address % 8 == 0 && bounds.contains(&address))
            .then(|| unsafe { *(address as *const usize) })
    };
    assert_eq!(
        unsafe { captured_rbx.read() },
        EgclVal::from_fixnum(991).to_raw() as usize,
        "C really reused hardware RBX before publication"
    );
    let mut roots = MappedRoots(Vec::new());
    for level in (0..3).rev() {
        let owner = &check.owners[level];
        let base = owner.code.as_ptr() as usize;
        let NativeCallValues::Frame(map) = owner.sites.value_map(cursor.pc - base).unwrap() else {
            panic!("exact frame map")
        };
        for location in map.gc_locations() {
            let NativeValueLocation::Register(register) = *location else {
                panic!("fixture requires preserved-register roots")
            };
            let index = SYSV_PRESERVED_REGISTERS
                .iter()
                .position(|&r| r == register)
                .unwrap();
            let address = cursor.registers[index].unwrap();
            assert!(bounds.contains(&address));
            assert_ne!(
                address, captured_rbx as usize,
                "outer root cannot use innermost hardware image"
            );
            check.slots[level] = address;
            check.before[level] = unsafe { (address as *const u64).read() };
            if !(level == 0 && check.omit_outer) {
                roots.0.push(address as *mut EgclVal);
            }
        }
        // This is the production composition algorithm: never reconstruct
        // saved-register homes locally or reset them when the owner changes.
        cursor = owner
            .sites
            .unwind(base, &cursor, bounds.clone(), read)
            .unwrap()
            .caller;
    }
    assert_eq!(cursor.call_sp, bounds.end);
    assert_ne!(check.slots[0], check.slots[1]);
    assert_ne!(
        check.before[0], check.before[1],
        "A and B have distinct heap values"
    );
    egcl_rt::rooted_ref!(_roots = &mut roots);
    // Allocation stress may have promoted a fixture root into Survivor space.
    // Collect it too so the measured boundary must relocate both saved values.
    egcl_rt::gc::full_gc().unwrap();
    check.after = check
        .slots
        .map(|address| unsafe { (address as *const u64).read() });
    check.observed = true;
}

fn trampoline() -> JitBuffer {
    let mut code = vec![0x53, 0x55, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57];
    code.extend_from_slice(&[0x48, 0x83, 0xec, 8]); // six saves plus pad: align Rust CALL
    code.extend_from_slice(&[0x48, 0x8d, 0x7c, 0x24, 64]); // rdi=caller's body RSP
    code.extend_from_slice(&[0x48, 0x8b, 0x74, 0x24, 56]); // rsi=exact return PC
    code.extend_from_slice(&[0x48, 0x8d, 0x54, 0x24, 48]); // rdx=writable RBX capture
    code.extend_from_slice(&[0x48, 0xb8]);
    code.extend_from_slice(&(collect as *const () as u64).to_le_bytes());
    code.extend_from_slice(&[0xff, 0xd0, 0x48, 0x83, 0xc4, 8]);
    code.extend_from_slice(&[
        0x41, 0x5f, 0x41, 0x5e, 0x41, 0x5d, 0x41, 0x5c, 0x5d, 0x5b, 0xc3,
    ]);
    JitBuffer::new(&code).unwrap()
}

fn exercise(omit_outer: bool, expect_stale: bool) {
    let trampoline = trampoline();
    let inner = owner(trampoline.as_ptr(), 2);
    let middle = owner(inner.code.as_ptr(), 1);
    let outer = owner(middle.code.as_ptr(), 0);
    let mut check = Check {
        owners: vec![outer, middle, inner],
        slots: [0; 2],
        before: [0; 2],
        after: [0; 2],
        observed: false,
        omit_outer,
    };
    // Keep independent expected roots for a safe negative control. They cannot
    // repair stale native save slots; returned machine words must still match.
    egcl_rt::rooted!(expected = Vec::new());
    for payload in [123.5_f64, 456.25_f64] {
        let body = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
        unsafe {
            body.cast::<f64>().write(payload);
        }
        expected.push(unsafe { EgclVal::from_heap_ptr(body.sub(8)) });
    }
    let original = [expected[0].to_raw(), expected[1].to_raw()];
    let run: extern "C" fn(u64, u64, *mut u64) -> u64 =
        unsafe { std::mem::transmute(check.owners[0].code.as_ptr()) };
    let mut returned_y = 0;
    CHECK.with(|slot| slot.set(&mut check));
    let returned_x = run(original[0], original[1], &mut returned_y);
    CHECK.with(|slot| slot.set(std::ptr::null_mut()));
    assert!(check.observed);
    assert_eq!(check.before, original);
    let relocated = [expected[0].to_raw(), expected[1].to_raw()];
    assert!(
        original
            .into_iter()
            .zip(relocated)
            .all(|(before, after)| before != after),
        "both real heap objects must move"
    );
    assert_eq!(
        returned_y, relocated[1],
        "B must use Y restored by C's actual POP"
    );
    assert_eq!(EgclVal::from_raw(returned_y).as_double_float(), 456.25);
    if expect_stale {
        assert_eq!(
            returned_x, original[0],
            "negative control must expose stale A register restoration"
        );
        assert_ne!(returned_x, relocated[0]);
    } else {
        assert_eq!(
            check.after, relocated,
            "both authoritative callee save words must relocate"
        );
        assert_eq!(
            returned_x, relocated[0],
            "A must use X restored by B's actual POP"
        );
        assert_eq!(EgclVal::from_raw(returned_x).as_double_float(), 123.5);
    }
}

#[test]
fn moving_roots_survive_composed_callee_saves_and_machine_restores() {
    // The opt-in omission supplies a reproducible failing acceptance run.
    exercise(
        std::env::var_os("EGCL_TEST_OMIT_COMPOSED_OUTER_ROOT").is_some(),
        false,
    );
}

#[test]
fn omitted_composed_home_exposes_stale_machine_restore() {
    exercise(true, true);
}
