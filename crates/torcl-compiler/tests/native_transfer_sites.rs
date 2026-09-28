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

fn cleanup_site() -> SysvTransferSite {
    use torcl_compiler::control_scope::{ControlScope, Ownership, ScopeKind};
    let mut emitted = site(12, 0);
    for cleanup_bcp in [80, 60] {
        emitted.map.control_scopes.push(ControlScope {
            push_bcp: cleanup_bcp - 10,
            ownership: Ownership::Local,
            sp_restore: 0,
            kind: ScopeKind::Unwind { cleanup_bcp },
        });
    }
    emitted
}

#[test]
fn cleanup_landing_is_bound_to_exact_site_scope_code_and_body_stack() {
    use torcl_compiler::t2::transfer_sites::SysvCleanupLanding;
    let mut code = [0x90; 64];
    code[40..44].copy_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
    let target = SysvCleanupLanding {
        return_offset: 12,
        entry_offset: 40,
        cleanup_bcp: 60,
    };
    let table = SysvTransferTable::new(code.len(), vec![cleanup_site(), site(24, 0)])
        .unwrap()
        .with_cleanup_landings(&code, &[target])
        .unwrap();
    let mut capture = SysvTransferCapture {
        request: std::ptr::null_mut(),
        value: NIL,
        exit: NativeExit::Transfer,
        preserved: [0; 6],
        caller_sp: 0x2000 as *const u64,
        return_pc: 0x100c as *const u8,
    };
    let selected = table.lookup(0x1000, 0x100c).unwrap();
    let packet = selected
        .native_cleanup_landing(0x1000, &capture)
        .unwrap()
        .unwrap();
    assert_eq!(packet.entry as usize, 0x1028);
    assert_eq!(packet.stack_pointer as usize, 0x2010);
    capture.return_pc = 0x1018 as *const u8;
    assert!(selected.native_cleanup_landing(0x1000, &capture).is_err());
    assert!(
        table
            .lookup(0x1000, 0x1018)
            .unwrap()
            .native_cleanup_landing(0x1000, &capture)
            .unwrap()
            .is_none()
    );
    capture.return_pc = 0x100c as *const u8;
    for address in [0, 0x2008, usize::MAX - 15] {
        capture.caller_sp = address as *const u64;
        assert!(selected.native_cleanup_landing(0x1000, &capture).is_err());
    }
    capture.caller_sp = 0x2000 as *const u64;
    for exit in [NativeExit::Returned, NativeExit::Deopt] {
        capture.exit = exit;
        assert!(selected.native_cleanup_landing(0x1000, &capture).is_err());
    }
    capture.exit = NativeExit::Transfer;
    capture.return_pc = (usize::MAX - 16 + 12) as *const u8;
    assert!(
        selected
            .native_cleanup_landing(usize::MAX - 16, &capture)
            .is_err()
    );
}

#[test]
fn cleanup_landing_rejects_unverified_targets_and_inherited_scopes() {
    use torcl_compiler::control_scope::Ownership;
    use torcl_compiler::t2::transfer_sites::SysvCleanupLanding;
    let mut code = [0x90; 64];
    code[40..44].copy_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
    let target = SysvCleanupLanding {
        return_offset: 12,
        entry_offset: 40,
        cleanup_bcp: 60,
    };
    for bad in [
        SysvCleanupLanding {
            return_offset: 13,
            ..target
        },
        SysvCleanupLanding {
            entry_offset: 41,
            ..target
        },
        SysvCleanupLanding {
            entry_offset: 62,
            ..target
        },
        SysvCleanupLanding {
            entry_offset: u32::MAX,
            ..target
        },
        SysvCleanupLanding {
            cleanup_bcp: 80,
            ..target
        },
    ] {
        assert!(
            SysvTransferTable::new(64, vec![cleanup_site()])
                .unwrap()
                .with_cleanup_landings(&code, &[bad])
                .is_err()
        );
    }
    assert!(
        SysvTransferTable::new(64, vec![cleanup_site()])
            .unwrap()
            .with_cleanup_landings(&code, &[target, target])
            .is_err()
    );
    assert!(
        SysvTransferTable::new(64, vec![cleanup_site()])
            .unwrap()
            .with_cleanup_landings(&code[..63], &[target])
            .is_err()
    );
    assert!(
        SysvTransferTable::new(64, vec![site(12, 0)])
            .unwrap()
            .with_cleanup_landings(&code, &[target])
            .is_err()
    );
    let mut inherited = cleanup_site();
    inherited.map.control_scopes.last_mut().unwrap().ownership = Ownership::Inherited;
    assert!(
        SysvTransferTable::new(64, vec![inherited])
            .unwrap()
            .with_cleanup_landings(&code, &[target])
            .is_err()
    );
    let mut unaligned = cleanup_site();
    unaligned.call_stack_adjust = 8;
    assert!(
        SysvTransferTable::new(64, vec![unaligned])
            .unwrap()
            .with_cleanup_landings(&code, &[target])
            .is_err()
    );
    let bound = SysvTransferTable::new(64, vec![cleanup_site()])
        .unwrap()
        .with_cleanup_landings(&code, &[target])
        .unwrap();
    assert!(bound.with_cleanup_landings(&code, &[target]).is_err());
}

#[test]
fn catch_landings_bind_each_identity_to_its_exact_source_site() {
    use torcl_compiler::control_scope::{ControlScope, Ownership, ScopeKind};
    use torcl_compiler::t2::transfer_sites::SysvCatchLanding;
    let mut code = [0x90; 64];
    for start in [40, 48] {
        code[start..start + 4].copy_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
    }
    let catch_site = || {
        let mut emitted = site(12, 0);
        for push_bcp in [1, 2] {
            emitted.map.control_scopes.push(ControlScope {
                push_bcp,
                ownership: Ownership::Local,
                sp_restore: 0,
                kind: ScopeKind::Catch { resume_bcp: 80 },
            });
        }
        emitted
    };
    let outer = SysvCatchLanding {
        return_offset: 12,
        entry_offset: 40,
        push_bcp: 1,
        resume_bcp: 80,
    };
    let inner = SysvCatchLanding {
        entry_offset: 48,
        push_bcp: 2,
        ..outer
    };
    let table = SysvTransferTable::new(64, vec![catch_site(), site(24, 0)])
        .unwrap()
        .with_catch_landings(&code, &[outer, inner])
        .unwrap();
    let selected = table.lookup(0x1000, 0x100c).unwrap();
    let mut capture = SysvTransferCapture {
        request: std::ptr::null_mut(),
        value: NIL,
        exit: NativeExit::Transfer,
        preserved: [0; 6],
        caller_sp: 0x2000 as *const u64,
        return_pc: 0x100c as *const u8,
    };
    for (push, address) in [(1, 0x1028), (2, 0x1030)] {
        let landing = selected
            .native_catch_landing(0x1000, &capture, push)
            .unwrap()
            .unwrap();
        assert_eq!(landing.entry as usize, address);
        assert_eq!(landing.stack_pointer as usize, 0x2010);
    }
    assert!(
        selected
            .native_catch_landing(0x1000, &capture, 3)
            .unwrap()
            .is_none()
    );
    capture.return_pc = 0x1018 as *const u8;
    assert!(selected.native_catch_landing(0x1000, &capture, 1).is_err());
    assert!(
        table
            .lookup(0x1000, 0x1018)
            .unwrap()
            .native_catch_landing(0x1000, &capture, 1)
            .unwrap()
            .is_none()
    );
    capture.return_pc = 0x100c as *const u8;
    for stack in [0, 0x2008, usize::MAX - 15] {
        capture.caller_sp = stack as *const u64;
        assert!(selected.native_catch_landing(0x1000, &capture, 1).is_err());
    }
    capture.caller_sp = 0x2000 as *const u64;
    for exit in [NativeExit::Returned, NativeExit::Deopt] {
        capture.exit = exit;
        assert!(selected.native_catch_landing(0x1000, &capture, 1).is_err());
    }
    for bad in [
        SysvCatchLanding {
            return_offset: 13,
            ..outer
        },
        SysvCatchLanding {
            push_bcp: 3,
            ..outer
        },
        SysvCatchLanding {
            resume_bcp: 81,
            ..outer
        },
        SysvCatchLanding {
            entry_offset: 41,
            ..outer
        },
        SysvCatchLanding {
            entry_offset: 62,
            ..outer
        },
        SysvCatchLanding {
            entry_offset: u32::MAX,
            ..outer
        },
    ] {
        assert!(
            SysvTransferTable::new(64, vec![catch_site()])
                .unwrap()
                .with_catch_landings(&code, &[bad])
                .is_err()
        );
    }
    assert!(
        SysvTransferTable::new(64, vec![catch_site()])
            .unwrap()
            .with_catch_landings(&code, &[outer, outer])
            .is_err()
    );
    assert!(
        SysvTransferTable::new(64, vec![catch_site()])
            .unwrap()
            .with_catch_landings(&code[..63], &[outer])
            .is_err()
    );
    for index in [0, 1] {
        let mut inherited = catch_site();
        inherited.map.control_scopes[index].ownership = Ownership::Inherited;
        assert!(
            SysvTransferTable::new(64, vec![inherited])
                .unwrap()
                .with_catch_landings(&code, &[outer])
                .is_err()
        );
    }
    let mut cleanup = catch_site();
    cleanup.map.control_scopes[1].kind = ScopeKind::Unwind { cleanup_bcp: 70 };
    assert!(
        SysvTransferTable::new(64, vec![cleanup])
            .unwrap()
            .with_catch_landings(&code, &[outer])
            .is_err()
    );
    let mut emitted = catch_site();
    emitted.map.control_scopes[1].push_bcp = 1;
    assert!(
        SysvTransferTable::new(64, vec![emitted])
            .unwrap()
            .with_catch_landings(&code, &[outer])
            .is_err()
    );
}
