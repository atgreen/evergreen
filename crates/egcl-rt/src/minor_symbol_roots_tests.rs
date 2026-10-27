// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::object::SymbolData;
use crate::value::{NIL, UNBOUND};

fn config() -> GcConfig {
    GcConfig {
        heap_size: 8 * 1024 * 1024,
        heap_max: 16 * 1024 * 1024,
        nursery_size: 1024 * 1024,
        tlab_size: 256,
        region_size: 4096,
        promotion_threshold: 15,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 64,
        old_occupancy_trigger: 0.5,
    }
}
fn cons(marker: i64) -> EgclVal {
    let body = alloc_typed(16, crate::object::type_id::CONS).unwrap();
    unsafe {
        body.cast::<EgclVal>().write(EgclVal::from_fixnum(marker));
        body.cast::<EgclVal>().add(1).write(NIL);
        EgclVal::from_cons_ptr(body)
    }
}
fn cells(address: usize) -> [EgclVal; 5] {
    let symbol = unsafe { &*(address as *const SymbolData) };
    [
        symbol.name,
        EgclVal(symbol.value.load(Ordering::Acquire)),
        EgclVal(symbol.function.load(Ordering::Acquire)),
        symbol.plist,
        symbol.package,
    ]
}
fn set_cells(address: usize, values: [EgclVal; 5]) {
    let symbol = unsafe { &mut *(address as *mut SymbolData) };
    symbol.name = values[0];
    symbol.value.store(values[1].to_raw(), Ordering::Release);
    symbol.function.store(values[2].to_raw(), Ordering::Release);
    symbol.plist = values[3];
    symbol.package = values[4];
}

#[test]
fn minor_symbol_cells_follow_authoritative_old_scan() {
    const CHILD: &str = "EGCL_TEST_MINOR_SYMBOL_ELISION_CHILD";
    const TEST: &str =
        "gc::minor_symbol_roots_tests::minor_symbol_cells_follow_authoritative_old_scan";
    if std::env::var_os(CHILD).is_none() {
        for soft_dirty in ["0", "1"] {
            for disabled in [false, true] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                    .env(CHILD, "1")
                    .env("EGCL_GC_SOFT_DIRTY", soft_dirty)
                    .env("EGCL_GC_POISON", "1")
                    .env("EGCL_MINOR_SYMBOL_ROOT_LOG", "1")
                    .env_remove("EGCL_DISABLE_MINOR_SYMBOL_ROOT_ELISION")
                    .env_remove("EGCL_GC_STRESS")
                    .env_remove("EGCL_GC_STRESS_AT")
                    .env_remove("EGCL_GC_VERIFY");
                if disabled {
                    command.env("EGCL_DISABLE_MINOR_SYMBOL_ROOT_ELISION", "1");
                }
                let output = command.output().unwrap();
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    output.status.success(),
                    "soft_dirty={soft_dirty} disabled={disabled}:\n{}\n{stderr}",
                    String::from_utf8_lossy(&output.stdout)
                );
                assert!(
                    stderr.contains(if disabled { "path=FULL" } else { "path=ELIDED" }),
                    "actual selected path missing: {stderr}"
                );
                if disabled {
                    assert!(!stderr.contains("path=ELIDED"));
                } else {
                    assert!(
                        stderr.matches("path=FULL").count() >= 2,
                        "both exceptional inventories must use fallback: {stderr}"
                    );
                }
                assert!(String::from_utf8_lossy(&output.stdout).contains("MINOR-SYMBOL-CELLS-OK"));
            }
        }
        return;
    }
    init_heap(&config()).unwrap();
    let index = crate::symbols::intern("MINOR-SYMBOL-CELLS");
    let address = crate::symbols::symbol_object_ptr(index).unwrap();
    crate::rooted!(original_name = cells(address)[0]);
    // Establish the cached exact-boundary proof before bypassing cell setters,
    // as ordinary native global stores do. Only the five field values change.
    collect_t0_minor().unwrap();
    let before = {
        crate::rooted!(values = Vec::<EgclVal>::new());
        values.push(alloc_character_string("YOUNG-SYMBOL-NAME"));
        for marker in 1..5 {
            values.push(cons(marker));
        }
        let before = std::array::from_fn::<_, 5, _>(|i| values[i].to_raw());
        // The low-level cells accept tagged values. Non-name slots contain
        // cons markers solely to exercise each tracing position, not Lisp APIs.
        set_cells(
            address,
            [values[0], values[1], values[2], values[3], values[4]],
        );
        before
    };
    collect_t0_minor().unwrap();
    {
        let after = cells(address);
        for i in 0..5 {
            assert_ne!(
                before[i],
                after[i].to_raw(),
                "field {i} must actually relocate"
            );
        }
        assert_eq!(after[0].as_string(), "YOUNG-SYMBOL-NAME");
        for (i, value) in after.iter().enumerate().skip(1) {
            assert_eq!(
                unsafe { value.as_ptr().cast::<EgclVal>().read() },
                EgclVal::from_fixnum(i as i64)
            );
        }
    }
    collect_t0_minor().unwrap();
    assert_eq!(cells(address)[0].as_string(), "YOUNG-SYMBOL-NAME");
    set_cells(address, [*original_name, UNBOUND, UNBOUND, NIL, NIL]);
    let renamed = crate::symbols::intern("MINOR-OLD::RENAMED");
    assert_eq!(
        crate::symbols::rename_package_prefix("MINOR-OLD", "MINOR-NEW").len(),
        1
    );
    collect_t0_minor().unwrap();
    assert_eq!(
        crate::symbols::registry_key(renamed).as_deref(),
        Some("MINOR-NEW::RENAMED")
    );

    // Major reclamation turns an unowned pinned symbol into a reusable filler.
    let dead = crate::symbols::make_uninterned("DEAD-SYMBOL-SLOT").as_symbol_index();
    let dead_address = crate::symbols::symbol_object_ptr(dead).unwrap();
    full_gc().unwrap();
    assert!(crate::symbols::symbol_object_ptr(dead).is_none());
    crate::rooted!(reused = crate::symbols::make_uninterned("REUSED-SYMBOL-SLOT"));
    let reused_address = crate::symbols::symbol_object_ptr(reused.as_symbol_index()).unwrap();
    assert_eq!(
        reused_address, dead_address,
        "exercise an actual reclaimed pinned slot"
    );
    let before = {
        crate::rooted!(young = cons(81));
        crate::symbols::set_symbol_value(reused.as_symbol_index(), *young);
        young.to_raw()
    };
    collect_t0_minor().unwrap();
    assert_ne!(
        crate::symbols::symbol_value(reused.as_symbol_index())
            .unwrap()
            .to_raw(),
        before
    );
    assert_eq!(
        crate::symbols::symbol_name(reused.as_symbol_index()).as_deref(),
        Some("REUSED-SYMBOL-SLOT")
    );

    // Never mistake an interior pointer or a Nursery object for an exact
    // allocated older SymbolData, even if an interior header looks plausible.
    crate::rooted!(
        nursery = {
            let body = alloc_typed(
                std::mem::size_of::<SymbolData>() - OBJECT_HEADER_SIZE,
                crate::object::type_id::SYMBOL,
            )
            .unwrap();
            unsafe {
                let header = body.sub(OBJECT_HEADER_SIZE).cast::<SymbolData>();
                let allocated_header = (&raw const (*header).header).read();
                header.write(SymbolData {
                    header: allocated_header,
                    name: NIL,
                    value: AtomicU64::new(UNBOUND.to_raw()),
                    function: AtomicU64::new(UNBOUND.to_raw()),
                    plist: NIL,
                    package: NIL,
                    flags: 0,
                    binding_guard: AtomicU32::new(0),
                });
                EgclVal::from_heap_ptr(header.cast())
            }
        }
    );
    set_cells(
        (nursery.to_raw() & !crate::value::TAG_MASK) as usize,
        [NIL, UNBOUND, UNBOUND, NIL, NIL],
    );
    let holder = alloc_pinned_typed(128, crate::object::type_id::SIMPLE_BASE_STRING).unwrap();
    let interior_header = unsafe { holder.add(24) };
    unsafe {
        holder.cast::<u64>().write(0); // valid empty string, remaining bytes opaque
        let mut forged = ObjectHeader::new(crate::object::type_id::SYMBOL, 8);
        forged.set_hash(48);
        forged.set_pinned();
        interior_header.cast::<ObjectHeader>().write(forged);
    }
    let forged = unsafe { EgclVal::from_heap_ptr(interior_header) };
    {
        let guard = heap_state().lock().unwrap();
        let state = guard.as_ref().unwrap();
        assert!(
            !older_symbol_objects_covered(state, &[forged]),
            "a plausible pinned SYMBOL header inside a leaf payload is not an allocation start"
        );
        assert!(!older_symbol_objects_covered(state, &[*nursery]));
        let interior = unsafe { EgclVal::from_heap_ptr((address + 16) as *mut u8) };
        assert!(!older_symbol_objects_covered(state, &[interior]));
        assert!(!older_symbol_objects_covered(state, &[NIL]));
    }
    // Pinning a valid Nursery symbol keeps its address stable, allowing a real
    // fallback collection. It becomes retained OldGen during that collection;
    // mark and relocation must still use the SAME original Full decision.
    pin(*nursery);
    let fixture_index = crate::symbols::register_root_test_object(*nursery);
    let pinned_region = ((nursery.to_raw() & !crate::value::TAG_MASK) as usize
        - heap_base_address() as usize)
        / config().region_size;
    let before = {
        crate::rooted!(young = NIL);
        loop {
            *young = cons(91);
            let region = ((young.to_raw() & !crate::value::TAG_MASK) as usize
                - heap_base_address() as usize)
                / config().region_size;
            if region != pinned_region {
                break;
            }
        }
        crate::symbols::set_symbol_value(fixture_index, *young);
        young.to_raw()
    };
    collect_t0_minor().unwrap();
    assert_ne!(
        crate::symbols::symbol_value(fixture_index)
            .unwrap()
            .to_raw(),
        before
    );
    crate::symbols::remove_root_test_object(fixture_index);

    // A legitimate off-heap object can be traced by legacy cell enumeration,
    // but is never eligible for omission based on a plausible header alone.
    let mut offheap = Box::new(SymbolData {
        header: ObjectHeader::new(
            crate::object::type_id::SYMBOL,
            (std::mem::size_of::<SymbolData>() / 8) as u16,
        ),
        name: NIL,
        value: AtomicU64::new(UNBOUND.to_raw()),
        function: AtomicU64::new(UNBOUND.to_raw()),
        plist: NIL,
        package: NIL,
        flags: 0,
        binding_guard: AtomicU32::new(0),
    });
    let object = unsafe { EgclVal::from_heap_ptr((&mut *offheap as *mut SymbolData).cast()) };
    let fixture_index = crate::symbols::register_root_test_object(object);
    let before = {
        crate::rooted!(young = cons(101));
        crate::symbols::set_symbol_value(fixture_index, *young);
        young.to_raw()
    };
    collect_t0_minor().unwrap();
    assert_ne!(
        crate::symbols::symbol_value(fixture_index)
            .unwrap()
            .to_raw(),
        before
    );
    crate::symbols::remove_root_test_object(fixture_index);
    drop(offheap);
    // Full-image replacement must discard the old eligibility cache. Restored
    // registry objects need a new exact-start proof before raw cell stores.
    set_entry_continuation(EgclVal::from_symbol_index(index));
    let path =
        std::env::temp_dir().join(format!("egcl-symbol-elision-{}.core", std::process::id()));
    crate::image::save_image(
        path.to_str().unwrap(),
        &crate::image::SaveImageOptions {
            executable: false,
            compression: crate::image::ImageCompression::None,
            purify: false,
        },
    )
    .unwrap();
    drop(original_name);
    drop(reused);
    drop(nursery);
    let restored = crate::image::load_image(path.to_str().unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(restored, EgclVal::from_symbol_index(index));
    let before = {
        crate::rooted!(young = cons(121));
        let previous = crate::symbols::symbol_value(index).unwrap();
        assert_eq!(
            crate::symbols::compare_exchange_symbol_value(index, previous, *young),
            Some(previous)
        );
        young.to_raw()
    };
    collect_t0_minor().unwrap();
    let value = crate::symbols::symbol_value(index).unwrap();
    assert_ne!(
        value.to_raw(),
        before,
        "restored global cell must retain a fresh young value"
    );
    assert_eq!(
        unsafe { value.as_ptr().cast::<EgclVal>().read() },
        EgclVal::from_fixnum(121)
    );
    if !minor_symbol_elision_disabled() {
        let guard = heap_state().lock().unwrap();
        let proof = guard.as_ref().unwrap().minor_symbol_coverage.unwrap();
        assert_eq!(proof.heap_epoch, heap_identity_epoch());
        assert!(
            proof.eligible,
            "restored canonical registry must obtain a fresh coverage proof"
        );
    }
    eprintln!(
        "MINOR-SYMBOL-SOFT-DIRTY-SCANS {}",
        old_scan_tests::DIRTY_SCANS.with(std::cell::Cell::get)
    );
    println!("MINOR-SYMBOL-CELLS-OK");
}
