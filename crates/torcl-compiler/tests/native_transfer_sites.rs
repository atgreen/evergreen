#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use torcl_compiler::t2::deopt::{LoweredScope, Rebox, SlotDescriptor};
use torcl_compiler::t2::ir::Inst;
use torcl_compiler::t2::mach::{Location, StackSlot};
use torcl_compiler::t2::native_transfer::SysvTransferCapture;
use torcl_compiler::t2::transfer_map::TransferCaptureMap;
use torcl_compiler::t2::transfer_sites::{SysvTransferSite, SysvTransferTable};
use torcl_rt::native_transfer::NativeExit;
use torcl_rt::value::NIL;

fn site(offset: u32, slot: u32) -> SysvTransferSite {
    let location = Location::Stack(StackSlot(slot));
    SysvTransferSite {
        return_offset: offset,
        stack_slots: 2,
        call_stack_adjust: 16,
        activation_slots: 0,
        shadow_roots: vec![],
        map: TransferCaptureMap {
            call: Inst(offset),
            machine_inst: offset as usize,
            origin_bcp: offset,
            control_scopes: vec![],
            roots: vec![location],
            frames: vec![LoweredScope {
                function: 0,
                resume_pc: offset,
                num_locals: 1,
                live_ref_bitmap: vec![true],
                slots: vec![SlotDescriptor::InLocation(location, Rebox::None)],
            }],
        },
    }
}

#[test]
fn return_pc_lookup_is_exact_sorted_and_bounded() {
    let table = SysvTransferTable::new(64, vec![site(40, 1), site(12, 0)]).unwrap();
    let base = 0x1000;
    for (pc, expected) in [(base + 12, 12), (base + 40, 40)] {
        assert_eq!(table.lookup(base, pc).unwrap().map().origin_bcp, expected);
    }
    for pc in [
        0,
        base - 1,
        base,
        base + 11,
        base + 13,
        base + 39,
        base + 41,
        base + 64,
    ] {
        assert!(table.lookup(base, pc).is_none());
    }
    assert!(table.lookup(usize::MAX - 8, usize::MAX).is_none());
    assert!(SysvTransferTable::new(64, vec![site(12, 0), site(12, 1)]).is_err());
    assert!(SysvTransferTable::new(64, vec![site(0, 0)]).is_err());
    assert!(SysvTransferTable::new(64, vec![site(64, 0)]).is_err());
}

#[test]
fn physical_recipes_cover_the_entire_map_before_capture() {
    assert!(SysvTransferTable::new(64, vec![site(12, 2)]).is_err());
    let mut broken = site(12, 0);
    broken.map.roots.clear();
    assert!(SysvTransferTable::new(64, vec![broken]).is_err());
    let mut broken = site(12, 0);
    broken.map.frames[0].num_locals = 2;
    assert!(SysvTransferTable::new(64, vec![broken]).is_err());
    let mut broken = site(12, 0);
    broken.map.frames[0].resume_pc = 99;
    assert!(SysvTransferTable::new(64, vec![broken]).is_err());
    let mut broken = site(12, 0);
    broken.call_stack_adjust = 3;
    assert!(SysvTransferTable::new(64, vec![broken]).is_err());
}

#[test]
fn snapshot_cannot_capture_another_sites_frame() {
    let table = SysvTransferTable::new(64, vec![site(12, 0), site(40, 1)]).unwrap();
    let site = table.lookup(0x1000, 0x100c).unwrap();
    let mut snapshot = site.reserve_snapshot().unwrap();
    let words = [0, 0, NIL.to_raw(), NIL.to_raw()];
    let mut capture = SysvTransferCapture {
        request: std::ptr::null_mut(),
        value: NIL,
        exit: NativeExit::Transfer,
        preserved: [0; 6],
        caller_sp: words.as_ptr(),
        return_pc: 0x1028 as *const u8,
    };
    assert!(unsafe { snapshot.capture(0x1000, &capture) }.is_err());
    capture.return_pc = 0x100c as *const u8;
    unsafe {
        snapshot.capture(0x1000, &capture).unwrap();
    }
    let frames = snapshot.reconstruct(|_| panic!("no floats")).unwrap();
    assert_eq!(frames[0].locals, vec![NIL]);
    capture.return_pc = 0x1028 as *const u8;
    assert!(unsafe { snapshot.write_back(0x1000, &mut capture) }.is_err());
    assert!(unsafe { snapshot.capture(0x1000, &capture) }.is_err());
    assert!(snapshot.reconstruct(|_| panic!("no floats")).is_err());
}

#[test]
fn canonical_shadow_wins_over_stale_native_home_and_requires_owning_activation() {
    use torcl_rt::value::TorclVal;
    let mut emitted = site(12, 0);
    emitted.activation_slots = 2;
    emitted.shadow_roots = vec![(Location::Stack(StackSlot(0)), 1)];
    let table = SysvTransferTable::new(64, vec![emitted]).unwrap();
    let mut snapshot = table
        .lookup(0x1000, 0x100c)
        .unwrap()
        .reserve_snapshot()
        .unwrap();
    let mut words = [0, 0, TorclVal::from_fixnum(1).to_raw(), 0];
    let mut capture = SysvTransferCapture {
        request: std::ptr::null_mut(),
        value: NIL,
        exit: NativeExit::Transfer,
        preserved: [0; 6],
        caller_sp: words.as_mut_ptr(),
        return_pc: 0x100c as *const u8,
    };
    let activation = [NIL, TorclVal::from_fixnum(2)];
    assert!(unsafe { snapshot.capture(0x1000, &capture) }.is_err());
    assert!(
        unsafe { snapshot.capture_from_activation(0x1000, &capture, &activation[..1]) }.is_err()
    );
    unsafe {
        snapshot
            .capture_from_activation(0x1000, &capture, &activation)
            .unwrap();
    }
    let frames = snapshot.reconstruct(|_| panic!("no floats")).unwrap();
    assert_eq!(
        frames[0].locals,
        vec![activation[1]],
        "native home is stale"
    );
    unsafe {
        snapshot.write_back(0x1000, &mut capture).unwrap();
    }
    assert_eq!(
        words[2],
        activation[1].to_raw(),
        "repair native home after capture"
    );
}

#[test]
fn shadow_maps_reject_raw_words_out_of_bounds_and_ambiguous_sources() {
    let root = Location::Stack(StackSlot(0));
    for shadows in [
        vec![(root, 1)],
        vec![(root, 0), (root, 0)],
        vec![(Location::Stack(StackSlot(1)), 0)],
    ] {
        let mut emitted = site(12, 0);
        emitted.activation_slots = 1;
        emitted.shadow_roots = shadows;
        assert!(SysvTransferTable::new(64, vec![emitted]).is_err());
    }
    let mut emitted = site(12, 0);
    emitted.activation_slots = 1;
    emitted.shadow_roots = vec![(root, 0)];
    emitted.map.roots.clear();
    emitted.map.frames[0].slots[0] = SlotDescriptor::InLocation(root, Rebox::ReboxFixnum);
    emitted.map.frames[0].live_ref_bitmap[0] = false;
    assert!(SysvTransferTable::new(64, vec![emitted]).is_err());
}
