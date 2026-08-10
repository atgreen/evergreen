use bliss_rt::stack::*;
use bliss_rt::value::{BlissVal, NIL};

#[test]
fn stack_capacity_matches_requested() {
    let stack = BlissStack::new(4096);
    assert_eq!(stack.capacity(), 4096);
    let stack2 = BlissStack::new(1024 * 512);
    assert_eq!(stack2.capacity(), 1024 * 512);
}

#[test]
fn stack_base_non_null() {
    let stack = BlissStack::new(4096);
    assert!(!stack.base().is_null());
}

#[test]
fn stack_sp_within_bounds() {
    let stack = BlissStack::new(4096);
    let base = stack.base() as usize;
    let sp = stack.sp() as usize;
    assert!(sp >= base && sp <= base + stack.capacity());
}

#[test]
fn stack_used_le_capacity() {
    let stack = BlissStack::new(65536);
    assert!(stack.used() <= stack.capacity());
}

#[test]
fn frame_type_repr_values() {
    assert_eq!(FrameType::Call as u8, 0b00);
    assert_eq!(FrameType::Catch as u8, 0b01);
    assert_eq!(FrameType::Unwind as u8, 0b10);
    assert_eq!(FrameType::Special as u8, 0b11);
}

#[test]
fn frame_type_extraction() {
    use std::ptr;
    for (flag_bits, expected) in [
        (0b00u32, FrameType::Call),
        (0b01, FrameType::Catch),
        (0b10, FrameType::Unwind),
        (0b11, FrameType::Special),
        (0xFF01, FrameType::Catch), // upper bits ignored
    ] {
        let frame = Frame {
            prev_fp: ptr::null_mut(),
            return_pc: ptr::null(),
            function: NIL,
            code_info: ptr::null(),
            flags: flag_bits,
            num_locals: 0,
            _pad: 0,
        };
        assert_eq!(frame.frame_type(), expected, "flags={:#06x}", flag_bits);
    }
}

#[test]
fn stack_fp_within_bounds() {
    let stack = BlissStack::new(4096);
    let fp = stack.fp() as usize;
    let base = stack.base() as usize;
    // fp should be null (empty stack) or within the stack region
    if fp != 0 {
        assert!(fp >= base && fp <= base + stack.capacity());
    }
}

#[test]
fn frame_locals_returns_correct_slice() {
    use std::ptr;
    // Build a frame with 3 locals backed by a contiguous array.
    // Frame layout: [Frame header][local0][local1][local2]
    let locals_data: [BlissVal; 3] = [
        BlissVal::from_raw(10 << 3),
        BlissVal::from_raw(20 << 3),
        BlissVal::from_raw(30 << 3),
    ];
    // Allocate frame + locals contiguously
    let mut buf = vec![0u8; std::mem::size_of::<Frame>() + 3 * std::mem::size_of::<BlissVal>()];
    let frame_ptr = buf.as_mut_ptr() as *mut Frame;
    unsafe {
        (*frame_ptr).prev_fp = ptr::null_mut();
        (*frame_ptr).return_pc = ptr::null();
        (*frame_ptr).function = NIL;
        (*frame_ptr).code_info = ptr::null();
        (*frame_ptr).flags = 0;
        (*frame_ptr).num_locals = 3;
        (*frame_ptr)._pad = 0;
        // Copy locals right after the frame header
        let locals_dst = frame_ptr.add(1) as *mut BlissVal;
        std::ptr::copy_nonoverlapping(locals_data.as_ptr(), locals_dst, 3);
        let locals = (*frame_ptr).locals();
        assert_eq!(locals.len(), 3);
        assert_eq!(locals[0], locals_data[0]);
        assert_eq!(locals[1], locals_data[1]);
        assert_eq!(locals[2], locals_data[2]);
    }
}

#[test]
fn frame_walker_null_yields_empty() {
    unsafe {
        let items: Vec<_> = FrameWalker::new(std::ptr::null()).collect();
        assert!(items.is_empty());
    }
}

#[test]
fn frame_walker_chain() {
    use std::ptr;
    let mut a = Frame {
        prev_fp: ptr::null_mut(), return_pc: ptr::null(),
        function: NIL, code_info: ptr::null(),
        flags: 0, num_locals: 0, _pad: 0,
    };
    let mut b = Frame {
        prev_fp: &mut a as *mut Frame, return_pc: ptr::null(),
        function: NIL, code_info: ptr::null(),
        flags: 0, num_locals: 0, _pad: 0,
    };
    unsafe {
        let items: Vec<_> = FrameWalker::new(&b as *const Frame).collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], &b as *const Frame);
        assert_eq!(items[1], &a as *const Frame);
    }
}
