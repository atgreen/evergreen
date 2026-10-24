// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Inline nursery allocation for calls to CONS, LIST and LIST* (bliss-8yo5p).
//!
//! A call to one of these standard functions keeps its IR identity: it is
//! still a call to the runtime, with the same frame state, safepoint map and
//! multiple-value treatment. In front of that call the emitter places a fast
//! path that bump-allocates the conses from the thread's T0 TLAB through the
//! runtime's window (`egcl_rt::gc::native_alloc_layout`), stores every header,
//! car and cdr, and continues after the call with the tagged list in RAX.
//!
//! When the window is too small, disabled, or was published before the last
//! collection, the fast path falls through into the unchanged call. That call
//! is the only path that can collect, so it alone needs (and keeps) the exact
//! map of the frame's roots; the fast path has no safepoint, cannot move
//! anything, and finishes initializing every object before any later
//! safepoint can observe it.
//!
//! The fast path stands in for a single-valued builtin, which empties the
//! multiple-value state. It leaves that state untouched, so the caller uses it
//! only where the difference cannot be seen: the state is known to be empty
//! on entry, or (in mapped code, which empties the buffer only at explicit
//! resets) nothing reads it before the next reset. Ordinary-ABI code needs the
//! first, because a CF-clear return there promises an empty buffer.
//!
//! The same window boxes raw doubles (BoxFloat) in front of the box veneer.

use super::{Asm, Cc, FramedHome, RAX, SCRATCH, load_home, mov_imm64, store_mem64_disp};
use crate::t2::ir::{AuxData, InstData, Value};
use std::collections::HashMap;

/// Longest list built inline; longer ones keep the call.
const MAX_INLINE_CONSES: usize = 8;

/// The conses a call site would build: `(cars, tail)` where `cars` are the
/// argument indexes of the cars in list order and `tail` is the final cdr
/// (an argument index, or `None` for NIL).
pub(super) struct ListShape {
    cars: std::ops::Range<usize>,
    tail: Option<usize>,
}

impl ListShape {
    fn conses(&self) -> usize {
        self.cars.len()
    }
}

/// Whether `symbol` is the canonical CL symbol `name`. Standard CL functions
/// cannot be redefined, so their semantics are fixed.
fn is_standard(symbol: u32, name: &str) -> bool {
    egcl_rt::symbols::registry_key(symbol).is_some_and(|key| key == name)
        && egcl_rt::symbols::find_index(name) == Some(symbol)
}

/// The list a call to CONS, LIST or LIST* builds, when it can be built inline.
pub(super) fn list_shape(data: &InstData) -> Option<ListShape> {
    let AuxData::CallTarget(symbol) = data.aux else {
        return None;
    };
    if data.results.len() != 1 {
        return None;
    }
    let n = data.args.len();
    let shape = if n == 2 && is_standard(symbol, "CONS") {
        ListShape {
            cars: 0..1,
            tail: Some(1),
        }
    } else if (1..=MAX_INLINE_CONSES).contains(&n) && is_standard(symbol, "LIST") {
        ListShape {
            cars: 0..n,
            tail: None,
        }
    } else if (2..=MAX_INLINE_CONSES + 1).contains(&n) && is_standard(symbol, "LIST*") {
        ListShape {
            cars: 0..n - 1,
            tail: Some(n - 1),
        }
    } else {
        return None;
    };
    Some(shape)
}

/// `op r64, qword ptr fs:[disp32]` (or the store form), absolute within the
/// thread's TLS block.
fn fs_mem(a: &mut Asm, opcode: u8, reg: u8, disp: i32) {
    a.push(0x64);
    a.push(0x48 | if reg >= 8 { 4 } else { 0 });
    a.push(opcode);
    a.push(((reg & 7) << 3) | 0b100);
    a.push(0x25);
    a.extend_from_slice(&disp.to_le_bytes());
}

/// `lea dst, [rax + disp32]`.
fn lea_rax(a: &mut Asm, dst: u8, disp: i32) {
    a.push(0x48 | if dst >= 8 { 4 } else { 0 });
    a.push(0x8d);
    a.push(0x80 | ((dst & 7) << 3));
    a.extend_from_slice(&disp.to_le_bytes());
}

fn load_value(
    a: &mut Asm,
    dst: u8,
    value: Value,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
) -> Option<()> {
    if let Some(&bits) = constants.get(&value) {
        mov_imm64(a, dst, bits as i64);
    } else {
        load_home(a, dst, *homes.get(&value)?, 0);
    }
    Some(())
}

/// Emit the fast path for an ordinary-ABI call to CONS, LIST or LIST*.
///
/// On success it leaves the tagged list in RAX with CF clear (a single-valued
/// return in the ordinary ABI) and jumps to the returned label, which the
/// caller binds where the call's own result arrives. On failure it falls
/// through to whatever the caller emits next: the call.
///
/// `mv_empty` says the multiple-value buffer is known to be empty on entry.
/// The ordinary ABI requires an empty buffer behind every CF-clear return, so
/// without it nothing is emitted.
pub(super) fn emit_ordinary_fast_path(
    a: &mut Asm,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
    mv_empty: bool,
) -> Option<egcl_rt::asm::Label> {
    if !mv_empty {
        return None;
    }
    let slow = a.label();
    let done = a.label();
    emit_bump(a, data, homes, constants, slow)?;
    a.push(0xf8); // clc: one value, MV buffer untouched (and empty)
    a.jmp(done);
    a.bind(slow);
    Some(done)
}

/// Emit the fast path for a mapped (helper-veneer or c2i) call to CONS, LIST
/// or LIST*. On success it stores the tagged list in the result's home and
/// jumps to the returned label, which the caller binds after the whole call
/// sequence, its root restoration included; nothing moved, so the homes the
/// call would have reloaded are already current. On failure it falls through
/// to the call.
///
/// `mv_unobservable` says leaving the MV state untouched is equivalent to the
/// call emptying it: it was empty already, or nothing reads it before the
/// next reset.
pub(super) fn emit_framed_fast_path(
    a: &mut Asm,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
    mv_unobservable: bool,
) -> Option<egcl_rt::asm::Label> {
    if !mv_unobservable {
        return None;
    }
    let result = *homes.get(data.results.first()?)?;
    let slow = a.label();
    let done = a.label();
    emit_bump(a, data, homes, constants, slow)?;
    super::store_home(a, result, RAX, 0);
    a.jmp(done);
    a.bind(slow);
    Some(done)
}

/// Bump-allocate and initialize the conses of `data`'s list, leaving it in
/// RAX, or branch to `slow` having changed nothing but RAX, RDX and R11 (none
/// of which is a value home). `None`, with nothing emitted, when the site is
/// not such a call or inline allocation is unavailable.
fn emit_bump(
    a: &mut Asm,
    data: &InstData,
    homes: &HashMap<Value, FramedHome>,
    constants: &HashMap<Value, u64>,
    slow: egcl_rt::asm::Label,
) -> Option<()> {
    let shape = list_shape(data)?;
    let layout = egcl_rt::gc::native_alloc_layout()?;
    // Every operand must be addressable before anything is emitted.
    if !data
        .args
        .iter()
        .all(|value| constants.contains_key(value) || homes.contains_key(value))
    {
        return None;
    }
    let footprint = i32::try_from(layout.cons_footprint).ok()?;
    let total = footprint.checked_mul(i32::try_from(shape.conses()).ok()?)?;
    // The header precedes each body; a cons value is its body address
    // tagged CONS.
    let tagged = 8 + egcl_rt::value::TAG_CONS as i32;
    window_fields(&layout)?;
    emit_window_bump(a, &layout, total, slow)?;
    const R11: u8 = 11;
    mov_imm64(a, R11, layout.cons_header as i64);
    let cars: Vec<usize> = shape.cars.clone().collect();
    for (index, &car) in cars.iter().enumerate() {
        let base = footprint * index as i32;
        store_mem64_disp(a, RAX, base, R11);
        load_value(a, SCRATCH, data.args[car], homes, constants)?;
        store_mem64_disp(a, RAX, base + 8, SCRATCH);
        if index + 1 < cars.len() {
            lea_rax(a, SCRATCH, base + footprint + tagged);
        } else if let Some(tail) = shape.tail {
            load_value(a, SCRATCH, data.args[tail], homes, constants)?;
        } else {
            mov_imm64(a, SCRATCH, egcl_rt::value::NIL.0 as i64);
        }
        store_mem64_disp(a, RAX, base + 16, SCRATCH);
    }
    lea_rax(a, RAX, tagged);
    Some(())
}

/// Emit the fast path for boxing the raw double in `input` (BoxFloat): on
/// success the tagged DOUBLE-FLOAT is in `result` and control continues at
/// the returned label, which the caller binds after its box-veneer call; on
/// failure it falls through to that call. Only RAX, RDX and R11 are written.
pub(super) fn emit_box_double_fast_path(
    a: &mut Asm,
    input: FramedHome,
    result: FramedHome,
) -> Option<egcl_rt::asm::Label> {
    let layout = egcl_rt::gc::native_alloc_layout()?;
    let footprint = i32::try_from(layout.double_footprint).ok()?;
    window_fields(&layout)?;
    let slow = a.label();
    let done = a.label();
    emit_window_bump(a, &layout, footprint, slow)?;
    const R11: u8 = 11;
    mov_imm64(a, R11, layout.double_header as i64);
    store_mem64_disp(a, RAX, 0, R11);
    load_home(a, SCRATCH, input, 0);
    store_mem64_disp(a, RAX, 8, SCRATCH);
    // A heap object's value is its header address tagged HEAP-OBJECT.
    lea_rax(a, RAX, egcl_rt::value::TAG_HEAP_OBJECT as i32);
    super::store_home(a, result, RAX, 0);
    a.jmp(done);
    a.bind(slow);
    Some(done)
}

/// The window's `(cursor, limit, epoch)` displacements from the thread
/// pointer.
fn window_fields(layout: &egcl_rt::gc::NativeAllocLayout) -> Option<(i32, i32, i32)> {
    use egcl_rt::gc::native_tlab_offsets as offsets;
    let window = |field: i32| layout.tls_offset.checked_add(field);
    Some((
        window(offsets::CURSOR)?,
        window(offsets::LIMIT)?,
        window(offsets::EPOCH)?,
    ))
}

/// Reserve `bytes` from the thread's TLAB window, leaving their start in RAX,
/// or branch to `slow` having written only RAX, RDX and R11. The caller has
/// checked [`window_fields`], so nothing here can fail half-way.
fn emit_window_bump(
    a: &mut Asm,
    layout: &egcl_rt::gc::NativeAllocLayout,
    bytes: i32,
    slow: egcl_rt::asm::Label,
) -> Option<()> {
    let (cursor, limit, epoch) = window_fields(layout)?;
    const R11: u8 = 11;
    fs_mem(a, 0x8b, RAX, cursor); // rax = cursor
    lea_rax(a, R11, bytes); // r11 = cursor + size
    fs_mem(a, 0x3b, R11, limit); // cmp r11, limit
    // Heap addresses are canonical user-space pointers, so a signed compare
    // orders them; a disabled (zero) limit always fails.
    a.jcc(Cc::G, slow);
    mov_imm64(a, SCRATCH, layout.epoch_address as i64);
    load_mem64(a, SCRATCH, SCRATCH);
    fs_mem(a, 0x3b, SCRATCH, epoch); // cmp rdx, published epoch
    a.jcc(Cc::Ne, slow);
    fs_mem(a, 0x89, R11, cursor); // cursor = r11
    Some(())
}

/// `mov dst, qword ptr [base]`.
fn load_mem64(a: &mut Asm, dst: u8, base: u8) {
    super::load_mem64_disp(a, dst, base, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::ir::{InstFlags, Opcode};

    fn call(name: &str, nargs: u32) -> InstData {
        InstData {
            opcode: Opcode::Call,
            args: (0..nargs).map(Value).collect(),
            results: vec![Value(nargs)],
            aux: AuxData::CallTarget(egcl_rt::symbols::intern(name)),
            flags: InstFlags {
                call: true,
                effectful: true,
                ..InstFlags::default()
            },
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        }
    }

    #[test]
    fn standard_list_constructors_have_inline_shapes() {
        let shape = |name, nargs| list_shape(&call(name, nargs)).map(|s| (s.cars, s.tail));
        assert_eq!(shape("CONS", 2), Some((0..1, Some(1))));
        assert_eq!(shape("LIST", 3), Some((0..3, None)));
        assert_eq!(shape("LIST*", 3), Some((0..2, Some(2))));
        assert_eq!(shape("CONS", 3), None);
        assert_eq!(shape("LIST", 9), None);
        assert_eq!(shape("LIST*", 1), None);
        assert_eq!(shape("VECTOR", 2), None);
    }

    #[test]
    fn the_fast_path_bumps_the_thread_window_before_the_call() {
        let data = call("CONS", 2);
        let homes = HashMap::from([
            (Value(0), FramedHome::Reg(3)),
            (Value(1), FramedHome::Stack(2)),
            (Value(2), FramedHome::Stack(3)),
        ]);
        let mut a = Asm::new();
        assert!(emit_ordinary_fast_path(&mut a, &data, &homes, &HashMap::new(), false).is_none());
        assert!(emit_framed_fast_path(&mut a, &data, &homes, &HashMap::new(), false).is_none());
        let done = emit_ordinary_fast_path(&mut a, &data, &homes, &HashMap::new(), true)
            .expect("x86-64 Linux allocates inline");
        a.bind(done);
        let code = a.finish().unwrap();
        // mov rax, fs:[window cursor]; nothing was emitted for the declined sites.
        assert_eq!(code[..5], [0x64, 0x48, 0x8b, 0x04, 0x25]);
        let offset = egcl_rt::gc::native_alloc_layout().unwrap().tls_offset;
        assert_eq!(code[5..9], offset.to_le_bytes());
    }
}
