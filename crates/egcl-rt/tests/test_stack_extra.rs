// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Extra tests for the egcl-rt stack module, covering interfaces
//! not exercised by test_stack.rs.
//!
//! Focuses on: SourceLocation construction and field access, CodeInfo
//! methods returning None, EgclStack zero-size edge case, initial
//! stack state invariants, frame with zero locals, frame flags with
//! high bits, and FrameWalker with longer chains.

use egcl_rt::stack::*;
use egcl_rt::value::{NIL, EgclVal};

// ═══════════════════════════════════════════════════════════════════
// SourceLocation — construction and field access
// ═══════════════════════════════════════════════════════════════════

#[test]
fn source_location_with_all_fields() {
    let loc = SourceLocation {
        file: Some("test.lisp".to_string()),
        line: 42,
        column: 10,
    };
    assert_eq!(loc.file.as_deref(), Some("test.lisp"));
    assert_eq!(loc.line, 42);
    assert_eq!(loc.column, 10);
}

#[test]
fn source_location_with_no_file() {
    let loc = SourceLocation {
        file: None,
        line: 1,
        column: 0,
    };
    assert!(loc.file.is_none());
    assert_eq!(loc.line, 1);
    assert_eq!(loc.column, 0);
}

#[test]
fn source_location_zero_line_and_column() {
    let loc = SourceLocation {
        file: Some("repl".to_string()),
        line: 0,
        column: 0,
    };
    assert_eq!(loc.line, 0);
    assert_eq!(loc.column, 0);
}

#[test]
fn source_location_clone_and_debug() {
    let loc = SourceLocation {
        file: Some("clone.lisp".to_string()),
        line: u32::MAX,
        column: 3,
    };
    let cloned = loc.clone();
    assert_eq!(cloned.file, loc.file);
    assert_eq!(cloned.line, u32::MAX);
    let debug_str = format!("{:?}", cloned);
    assert!(debug_str.contains("clone.lisp"));
}

// ═══════════════════════════════════════════════════════════════════
// CodeInfo — source_location always returns None, stack_map always None
// ═══════════════════════════════════════════════════════════════════

#[test]
fn code_info_source_location_returns_none_at_zero() {
    let ci = CodeInfo::empty();
    assert!(
        ci.source_location(0).is_none(),
        "CodeInfo::source_location should return None (stub)"
    );
}

#[test]
fn code_info_source_location_returns_none_at_nonzero() {
    let ci = CodeInfo::empty();
    assert!(ci.source_location(100).is_none());
    assert!(ci.source_location(usize::MAX).is_none());
}

#[test]
fn code_info_stack_map_returns_none_at_zero() {
    let ci = CodeInfo::empty();
    assert!(
        ci.stack_map(0).is_none(),
        "CodeInfo::stack_map should return None (stub)"
    );
}

#[test]
fn code_info_stack_map_returns_none_at_nonzero() {
    let ci = CodeInfo::empty();
    assert!(ci.stack_map(42).is_none());
    assert!(ci.stack_map(usize::MAX).is_none());
}

// ═══════════════════════════════════════════════════════════════════
// EgclStack — zero-size edge case
// ═══════════════════════════════════════════════════════════════════

#[test]
fn stack_zero_size_capacity() {
    let stack = EgclStack::new(0);
    assert_eq!(stack.capacity(), 0);
}

#[test]
fn stack_zero_size_used_is_zero() {
    let stack = EgclStack::new(0);
    assert_eq!(stack.used(), 0);
}

#[test]
fn stack_zero_size_sp_equals_base() {
    let stack = EgclStack::new(0);
    // With zero used, sp should equal base
    assert_eq!(stack.sp(), stack.base());
}

#[test]
fn stack_zero_size_fp_is_null() {
    let stack = EgclStack::new(0);
    assert!(stack.fp().is_null());
}

// ═══════════════════════════════════════════════════════════════════
// EgclStack — initial state invariants
// ═══════════════════════════════════════════════════════════════════

#[test]
fn stack_initial_state() {
    let stack = EgclStack::new(8192);
    assert_eq!(stack.used(), 0);
    assert!(stack.fp().is_null());
    assert_eq!(stack.sp(), stack.base());
    assert!(!stack.base().is_null());
}

#[test]
fn stack_various_sizes() {
    let s1 = EgclStack::new(1);
    assert_eq!(s1.capacity(), 1);
    assert_eq!(s1.used(), 0);
    let s2 = EgclStack::new(512 * 1024);
    assert_eq!(s2.capacity(), 512 * 1024);
}

// ═══════════════════════════════════════════════════════════════════
// Frame — zero locals returns empty slice
// ═══════════════════════════════════════════════════════════════════

#[test]
fn frame_zero_locals_returns_empty_slice() {
    use std::ptr;
    let frame = Frame {
        prev_fp: ptr::null_mut(),
        return_pc: ptr::null(),
        function: NIL,
        code_info: ptr::null(),
        flags: 0,
        num_locals: 0,
        _pad: 0,
    };
    unsafe {
        let locals = frame.locals();
        assert!(
            locals.is_empty(),
            "zero num_locals should yield empty slice"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// Frame — flags with high bits set, frame_type uses only low 2 bits
// ═══════════════════════════════════════════════════════════════════

#[test]
fn frame_type_ignores_high_bits_comprehensive() {
    use std::ptr;
    for &hi in &[0x8000_0000u32, 0xFFFF_FF00, 0xDEAD_BE00] {
        for low in 0u32..=3 {
            let expected = match low {
                0 => FrameType::Call,
                1 => FrameType::Catch,
                2 => FrameType::Unwind,
                3 => FrameType::Special,
                _ => unreachable!(),
            };
            let frame = Frame {
                prev_fp: ptr::null_mut(),
                return_pc: ptr::null(),
                function: NIL,
                code_info: ptr::null(),
                flags: hi | low,
                num_locals: 0,
                _pad: 0,
            };
            assert_eq!(frame.frame_type(), expected, "flags={:#010x}", hi | low);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// FrameWalker — longer chain (3 frames)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn frame_walker_three_frame_chain() {
    use std::ptr;
    let mut a = Frame {
        prev_fp: ptr::null_mut(),
        return_pc: ptr::null(),
        function: NIL,
        code_info: ptr::null(),
        flags: FrameType::Call as u32,
        num_locals: 0,
        _pad: 0,
    };
    let mut b = Frame {
        prev_fp: &mut a as *mut Frame,
        return_pc: ptr::null(),
        function: NIL,
        code_info: ptr::null(),
        flags: FrameType::Catch as u32,
        num_locals: 0,
        _pad: 0,
    };
    let c = Frame {
        prev_fp: &mut b as *mut Frame,
        return_pc: ptr::null(),
        function: NIL,
        code_info: ptr::null(),
        flags: FrameType::Unwind as u32,
        num_locals: 0,
        _pad: 0,
    };
    unsafe {
        let frames: Vec<_> = FrameWalker::new(&c as *const Frame).collect();
        assert_eq!(frames.len(), 3);
        // Verify order: c -> b -> a
        assert_eq!(frames[0], &c as *const Frame);
        assert_eq!(frames[1], &b as *const Frame);
        assert_eq!(frames[2], &a as *const Frame);
    }
}

// ═══════════════════════════════════════════════════════════════════
// FrameWalker — single frame (prev_fp is null)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn frame_walker_single_frame() {
    use std::ptr;
    let frame = Frame {
        prev_fp: ptr::null_mut(),
        return_pc: ptr::null(),
        function: NIL,
        code_info: ptr::null(),
        flags: 0,
        num_locals: 0,
        _pad: 0,
    };
    unsafe {
        let frames: Vec<_> = FrameWalker::new(&frame as *const Frame).collect();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], &frame as *const Frame);
    }
}

// ═══════════════════════════════════════════════════════════════════
// Frame — locals with 1 local variable
// ═══════════════════════════════════════════════════════════════════

#[test]
fn frame_single_local() {
    use std::ptr;
    let local_val = EgclVal::from_raw(0xCAFE_0000);
    let mut buf = vec![0u8; std::mem::size_of::<Frame>() + std::mem::size_of::<EgclVal>()];
    let frame_ptr = buf.as_mut_ptr() as *mut Frame;
    unsafe {
        (*frame_ptr).prev_fp = ptr::null_mut();
        (*frame_ptr).return_pc = ptr::null();
        (*frame_ptr).function = NIL;
        (*frame_ptr).code_info = ptr::null();
        (*frame_ptr).flags = 0;
        (*frame_ptr).num_locals = 1;
        (*frame_ptr)._pad = 0;
        let locals_dst = frame_ptr.add(1) as *mut EgclVal;
        std::ptr::copy_nonoverlapping(&local_val as *const EgclVal, locals_dst, 1);
        let locals = (*frame_ptr).locals();
        assert_eq!(locals.len(), 1);
        assert_eq!(locals[0], local_val);
    }
}

// ═══════════════════════════════════════════════════════════════════
// FrameType — traits (Copy, Eq, Debug)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn frame_type_traits() {
    assert_eq!(FrameType::Call, FrameType::Call);
    assert_ne!(FrameType::Call, FrameType::Catch);
    let ft = FrameType::Unwind;
    let ft2 = ft; // Copy
    assert_eq!(ft, ft2);
    assert!(format!("{:?}", FrameType::Special).contains("Special"));
}
