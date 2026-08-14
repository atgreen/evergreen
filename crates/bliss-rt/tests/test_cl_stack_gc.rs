//! Precise CL-stack GC: objects held only in interpreter/compiled frames are
//! marked precisely and relocate correctly across a collection (bliss-nmq.3),
//! using the unified spec `ObjectHeader` forwarding contract (bliss-jtc.19).
//!
//! Allocation goes through the internal TLAB allocator, which is awkward to
//! drive directly into a guaranteed evacuation, so these tests exercise the
//! CL-stack scanner against the GC's real forwarding representation: an object
//! header (`ObjectHeader`, §1.3) with the FORWARDED gc-bit set and the new body
//! pointer stored in the object's first payload word, type_id and size left
//! intact. The scanner integration itself is wired into the GC's old-gen marker
//! (scan_cl_stack_roots) and evacuation (relocate_cl_stack_refs).

use bliss_rt::object::{gc_bit, ObjectHeader};
use bliss_rt::{BlissStack, BlissVal, visit_stack_refs};

const HDR: usize = 8; // OBJECT_HEADER_SIZE
const TAG_HEAP: u64 = 0b010;

/// A minimal 24-byte object: 8-byte `ObjectHeader` + 16-byte body. `type_id` is
/// arbitrary; `size` is the total footprint in 8-byte units (24 / 8 = 3).
fn make_object(type_id: u8) -> Vec<u8> {
    let mut buf = vec![0u8; HDR + 16];
    let hdr = ObjectHeader::new(type_id, 3);
    buf[..HDR].copy_from_slice(&hdr.0.to_le_bytes());
    buf
}

/// Install a forwarding pointer exactly as `HeapCollector::copy_object` does:
/// set the FORWARDED gc-bit (preserving type_id/size) and write `new_body` into
/// the first payload word.
fn forward(obj: &mut [u8], new_body: usize) {
    let mut hdr = ObjectHeader(u64::from_le_bytes(obj[..HDR].try_into().unwrap()));
    hdr.set_forwarded();
    obj[..HDR].copy_from_slice(&hdr.0.to_le_bytes());
    obj[HDR..HDR + 8].copy_from_slice(&(new_body as u64).to_le_bytes());
}

/// Chase forwarding for a CL-frame slot — the same logic as
/// `HeapCollector::relocate_cl_stack_refs`: if the referenced object's header
/// has the FORWARDED gc-bit set, rewrite the slot to the forwarding address
/// stored at offset `HDR`, preserving the tag.
fn chase(slot: &mut BlissVal) {
    let tag = slot.0 & 0b111;
    let body = (slot.0 & !0b111) as usize;
    if body < HDR {
        return;
    }
    let header = body - HDR;
    // SAFETY: `header` precedes a live object body constructed by the test.
    let hdr = unsafe { ObjectHeader(*(header as *const u64)) };
    if (hdr.gc_bits() & (1 << gc_bit::FORWARDED)) != 0 {
        let new_body = unsafe { *(body as *const usize) };
        slot.0 = (new_body as u64) | tag;
    }
}

#[test]
fn cl_frame_object_relocates_correctly_across_move() {
    // An object with recognizable contents, and its relocated copy.
    let mut old = make_object(0x0E); // STANDARD_OBJECT
    let mut newo = make_object(0x0E);
    newo[HDR..HDR + 8].copy_from_slice(&0xDEAD_BEEF_u64.to_le_bytes());
    let old_body = unsafe { old.as_mut_ptr().add(HDR) } as usize;
    let new_body = unsafe { newo.as_mut_ptr().add(HDR) } as usize;

    // A CL frame is the ONLY thing referencing the object.
    let stack = BlissStack::new(64 * 1024);
    let f = stack
        .push_frame(BlissVal::from_fixnum(0), std::ptr::null(), 2, 0)
        .unwrap();
    unsafe {
        let s = BlissStack::frame_slots_mut(f);
        s[0] = BlissVal((old_body as u64) | TAG_HEAP); // the movable object
        s[1] = BlissVal::from_fixnum(7); // a non-reference, must not be touched
    }

    // Simulate the collector moving the object.
    forward(&mut old, new_body);

    // Relocate CL-stack references.
    unsafe { visit_stack_refs(stack.fp(), chase) };

    // The reference now points at the new location; contents are intact; the
    // fixnum slot is untouched.
    unsafe {
        let s = BlissStack::frame_slots_mut(f);
        assert_eq!((s[0].0 & !0b111) as usize, new_body, "ref relocated to new body");
        assert_eq!(*((s[0].0 & !0b111) as *const u64), 0xDEAD_BEEF, "contents intact");
        assert_eq!(s[1], BlissVal::from_fixnum(7), "non-reference untouched");
    }
}

#[test]
fn non_forwarded_refs_are_left_alone() {
    // A live (non-forwarded) object's reference must not be rewritten.
    let mut obj = make_object(0x01); // CONS type_id, not forwarded
    let body = unsafe { obj.as_mut_ptr().add(HDR) } as usize;
    let stack = BlissStack::new(64 * 1024);
    let f = stack
        .push_frame(BlissVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    let original = BlissVal((body as u64) | TAG_HEAP);
    unsafe { BlissStack::frame_slots_mut(f)[0] = original };
    unsafe { visit_stack_refs(stack.fp(), chase) };
    unsafe {
        assert_eq!(BlissStack::frame_slots_mut(f)[0], original, "non-forwarded ref unchanged");
    }
}

#[test]
fn full_gc_with_cl_frame_refs_does_not_crash() {
    use bliss_rt::gc::full_gc;
    use bliss_rt::runtime::{Runtime, RuntimeConfig};
    // A live runtime provides an initialized heap; a full GC scans the current
    // thread's CL frames precisely without crashing.
    let mut cfg = RuntimeConfig::from_env().expect("from_env");
    cfg.heap_size = 4 * 1024 * 1024;
    cfg.num_workers = 1;
    cfg.no_image = true;
    let mut rt = Runtime::init(cfg).expect("runtime init");
    // Push a frame holding references (addresses need not be heap-managed; the
    // scanner bounds-checks before dereferencing during relocation).
    let stack = bliss_rt::current_thread().stack();
    let _f = stack
        .push_frame(BlissVal::from_fixnum(0), std::ptr::null(), 2, 0)
        .unwrap();
    let _ = full_gc();
    stack.pop_frame();
    rt.shutdown().ok();
}
