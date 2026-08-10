use bliss_rt::stack::*;
use bliss_rt::value::NIL;

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
