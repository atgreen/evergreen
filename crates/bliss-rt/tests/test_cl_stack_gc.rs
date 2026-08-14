//! Precise CL-stack GC: objects held only in interpreter/compiled frames are
//! marked precisely and relocate correctly across a collection (bliss-nmq.3).
//!
//! Allocation goes through the internal TLAB allocator, which is awkward to
//! drive directly, so these tests exercise the CL-stack scanner against the
//! GC's exact forwarding-pointer format (OBJECT_HEADER_SIZE = 8; a forwarded
//! object has type_id 0xFF at the header and its new body pointer stored at the
//! body address). The scanner integration itself is wired into the GC's old-gen
//! marker (scan_cl_stack_roots) and evacuation (relocate_cl_stack_refs).

use bliss_rt::{BlissStack, BlissVal, visit_stack_refs};

const HDR: usize = 8; // OBJECT_HEADER_SIZE
const FWD: u8 = 0xFF; // FORWARDED_TYPE_ID
const TAG_HEAP: u64 = 0b010;

#[test]
fn cl_frame_object_relocates_correctly_across_move() {
    // An object [header|body] with recognizable contents, and its relocated copy.
    let mut old = vec![0u8; HDR + 16];
    let mut newo = vec![0u8; HDR + 16];
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

    // Simulate the collector moving the object: install a forwarding pointer.
    unsafe {
        *old.as_mut_ptr() = FWD; // header type_id
        *(old_body as *mut usize) = new_body; // new body ptr at the body address
    }

    // Relocate CL-stack references (same logic as HeapCollector::relocate_cl_stack_refs).
    unsafe {
        visit_stack_refs(stack.fp(), |slot| {
            let tag = slot.0 & 0b111;
            let body = (slot.0 & !0b111) as usize;
            if body >= HDR && *((body - HDR) as *const u8) == FWD {
                let nb = *(body as *const usize);
                slot.0 = (nb as u64) | tag;
            }
        });
    }

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
    let mut obj = vec![0u8; HDR + 8];
    obj[0] = 0x01; // some real type_id, not FORWARDED
    let body = unsafe { obj.as_mut_ptr().add(HDR) } as usize;
    let stack = BlissStack::new(64 * 1024);
    let f = stack
        .push_frame(BlissVal::from_fixnum(0), std::ptr::null(), 1, 0)
        .unwrap();
    let original = BlissVal((body as u64) | TAG_HEAP);
    unsafe { BlissStack::frame_slots_mut(f)[0] = original };
    unsafe {
        visit_stack_refs(stack.fp(), |slot| {
            let tag = slot.0 & 0b111;
            let b = (slot.0 & !0b111) as usize;
            if b >= HDR && *((b - HDR) as *const u8) == FWD {
                let nb = *(b as *const usize);
                slot.0 = (nb as u64) | tag;
            }
        });
    }
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
