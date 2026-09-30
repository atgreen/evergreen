use std::cell::RefCell;
use egcl_compiler::t2::deopt::{LoweredScope, Rebox, SlotDescriptor};
use egcl_compiler::t2::ir::Inst;
use egcl_compiler::t2::mach::{Location, StackSlot};
use egcl_compiler::t2::transfer_capture::TransferSnapshot;
use egcl_compiler::t2::transfer_map::TransferCaptureMap;
use egcl_rt::value::EgclVal;
use egcl_rt::{Collector, HeapCollector};

fn double(value: f64) -> EgclVal {
    let body = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe {
        body.cast::<f64>().write(value);
        EgclVal::from_heap_ptr(body.sub(8))
    }
}

#[test]
fn captured_inputs_and_completed_frames_relocate_during_reconstruction() {
    let locations: Vec<_> = (0..4).map(|i| Location::Stack(StackSlot(i))).collect();
    let map = TransferCaptureMap {
        call: Inst(0),
        machine_inst: 0,
        origin_bcp: 17,
        control_scopes: vec![],
        frames: vec![
            LoweredScope {
                resume_pc: 17,
                function: 0,
                num_locals: 3,
                slots: vec![
                    SlotDescriptor::InLocation(locations[0], Rebox::None),
                    SlotDescriptor::InLocation(locations[1], Rebox::ReboxF64),
                    SlotDescriptor::InLocation(locations[3], Rebox::ReboxFixnum),
                ],
                live_ref_bitmap: vec![true, false, false],
            },
            LoweredScope {
                resume_pc: 29,
                function: 0,
                num_locals: 1,
                slots: vec![
                    SlotDescriptor::InLocation(locations[2], Rebox::ReboxF64),
                    SlotDescriptor::InLocation(locations[0], Rebox::None),
                ],
                live_ref_bitmap: vec![false, true],
            },
        ],
        roots: vec![locations[0]],
    };
    // Reserve the snapshot before entering a native segment. Capture itself
    // must not allocate, collect, or retain a borrow of the physical frame.
    let mut snapshot = TransferSnapshot::new(&map).unwrap();
    egcl_rt::rooted!(expected = double(101.0));
    let unrooted_original = expected.to_raw();
    let mut physical_words = [
        unrooted_original,
        3.25f64.to_bits(),
        4.5f64.to_bits(),
        unrooted_original,
    ];
    unsafe {
        snapshot.capture(|location| {
            let Location::Stack(StackSlot(index)) = location else {
                unreachable!()
            };
            physical_words[index as usize]
        });
    }
    physical_words.fill(0); // the snapshot must be independent of these old homes
    let originals = RefCell::new(Vec::new());
    egcl_rt::rooted!(
        frames = snapshot
            .reconstruct(|value| {
                HeapCollector::new()
                    .minor_gc()
                    .expect("forced relocation during boxing");
                let boxed = double(value);
                originals.borrow_mut().push(boxed.to_raw()); // raw observations, not roots
                boxed
            })
            .unwrap()
    );
    assert_ne!(
        expected.to_raw(),
        unrooted_original,
        "A/B proof: the source really moved"
    );
    assert_eq!(
        frames[0].locals[0], *expected,
        "already rebuilt local must be updated"
    );
    assert_eq!(
        frames[1].stack[0], *expected,
        "later read must use relocated saved input"
    );
    assert_eq!(
        frames[0].locals[2].as_fixnum() as u64,
        unrooted_original,
        "raw bits resembling a heap pointer must not be relocated"
    );
    assert_ne!(
        frames[0].locals[1].to_raw(),
        originals.borrow()[0],
        "the first boxed result must survive moving GC while rebuilding the second frame"
    );
    assert_eq!(frames[0].locals[1].as_double_float(), 3.25);
    assert_eq!(frames[1].locals[0].as_double_float(), 4.5);
    HeapCollector::new().minor_gc().unwrap();
    assert_eq!(frames[0].locals[0].as_double_float(), 101.0);
    assert_eq!(frames[1].locals[0].as_double_float(), 4.5);
}

#[test]
fn snapshot_refuses_incomplete_roots_and_uncaptured_state() {
    use egcl_compiler::t2::transfer_capture::CaptureError;
    let location = Location::Stack(StackSlot(0));
    let mut map = TransferCaptureMap {
        call: Inst(0),
        machine_inst: 0,
        origin_bcp: 0,
        control_scopes: vec![],
        frames: vec![LoweredScope {
            resume_pc: 0,
            function: 0,
            num_locals: 1,
            slots: vec![SlotDescriptor::InLocation(location, Rebox::None)],
            live_ref_bitmap: vec![true],
        }],
        roots: vec![],
    };
    assert!(matches!(
        TransferSnapshot::new(&map),
        Err(CaptureError::MissingRoot(_))
    ));
    map.roots.push(location);
    let mut snapshot = TransferSnapshot::new(&map).unwrap();
    assert_eq!(
        snapshot.reconstruct(|_| unreachable!()),
        Err(CaptureError::NotCaptured)
    );
    map.frames[0]
        .slots
        .push(SlotDescriptor::InLocation(location, Rebox::ReboxFixnum));
    assert!(matches!(
        TransferSnapshot::new(&map),
        Err(CaptureError::ConflictingLocation(_))
    ));
    map.frames[0].slots = vec![SlotDescriptor::MaterializeConst(EgclVal(0x1001))];
    map.roots.clear();
    assert!(
        TransferSnapshot::new(&map).is_err(),
        "a movable literal cannot hide outside the root map"
    );
}
