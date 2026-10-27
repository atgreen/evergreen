// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Recover a raw double and a native-local young root at a real ordinary guard.
//! Each child owns its cached compiler flags and its GC stress configuration.

use super::*;
use egcl_compiler::t2::x64_calls::NativeCallValues;

thread_local! {
    static ENTRY_COUNT: Cell<usize> = const { Cell::new(0) };
    static PAYLOAD_ENTRIES: Cell<usize> = const { Cell::new(0) };
    static OBSERVED_MARKER: Cell<u64> = const { Cell::new(0) };
    static STRESS: Cell<bool> = const { Cell::new(false) };
    static ENTRY_ARGUMENTS: Cell<*const EgclVal> = const { Cell::new(std::ptr::null()) };
    static ENTRY_MARKER: Cell<u64> = const { Cell::new(0) };
}

fn ordinary_entry() {
    ENTRY_COUNT.with(|count| count.set(count.get() + 1));
    let arguments = ENTRY_ARGUMENTS.with(Cell::get);
    assert!(!arguments.is_null());
    // Address only, not a root: host arguments and independent serialized
    // native payload must both update when the raw-double reboxer collects.
    ENTRY_MARKER.with(|slot| slot.set(unsafe { (*arguments.add(2)).to_raw() }));
    if STRESS.with(Cell::get) {
        egcl_rt::gc::stress_arm_after_init();
    }
}

fn inspect_payload(pointer: *const u64, count: usize, had_f64: bool) {
    assert!(had_f64, "failure must recover an actual raw double payload");
    PAYLOAD_ENTRIES.with(|entries| entries.set(entries.get() + 1));
    // Do not create a slice or copy a tagged payload before collecting. The
    // callback is inside the NativeDeoptRoots lifetime, after F64 reboxing.
    if !STRESS.with(Cell::get) {
        egcl_rt::collect_t0_minor().unwrap();
    }
    let words = unsafe { std::slice::from_raw_parts(pointer, count) };
    assert!(words.len() >= 4);
    assert_eq!(words.len(), 4 + words[2] as usize + words[3] as usize);
    let marker = words[4..]
        .iter()
        .copied()
        .find(|bits| {
            let value = EgclVal(*bits);
            value.is_cons() && crate::cli::cp(value) == (EgclVal::from_fixnum(71), NIL)
        })
        .expect("rooted guard payload must retain the held marker");
    let allocated = ENTRY_MARKER.with(Cell::get);
    assert_ne!(allocated, 0);
    assert_ne!(marker, allocated, "held marker must actually relocate");
    OBSERVED_MARKER.with(|slot| slot.set(marker));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_shared_deopt_reboxes_raw_double_and_moves_payload_root_once() {
    const CHILD: &str = "EGCL_TEST_ORDINARY_SHARED_DEOPT_CHILD";
    const TEST: &str = "cli::bytecode::native_transfer_tests::ordinary_t2::shared_deopt::ordinary_shared_deopt_reboxes_raw_double_and_moves_payload_root_once";
    if std::env::var_os(CHILD).is_none() {
        for mode in ["on-normal", "off-normal", "on-stress", "off-stress"] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    TEST,
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, mode)
                .env("EGCL_SHARED_DEOPT_LOG", "1")
                .env("EGCL_T2_NO_QUEUE", "1")
                .env_remove("EGCL_PROFILING_DISABLED")
                .env_remove("EGCL_DISABLE_SHARED_DEOPT_RECOVERY")
                .env_remove("EGCL_GC_STRESS")
                .env_remove("EGCL_GC_STRESS_AT")
                .env_remove("EGCL_GC_STRESS_SKIP")
                .env_remove("EGCL_GC_STRESS_AFTER_INIT")
                .env_remove("EGCL_GC_POISON")
                .env_remove("EGCL_FORCE_TIER")
                .env_remove("EGCL_GC_VERIFY")
                .env_remove("EGCL_DISABLE_T2")
                .env_remove("EGCL_DISABLE_ARRAY_SPECIALIZATION");
            if mode.starts_with("off") {
                child.env("EGCL_DISABLE_SHARED_DEOPT_RECOVERY", "1");
            }
            if mode.ends_with("stress") {
                child
                    .env("EGCL_GC_STRESS", "1")
                    .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                    .env("EGCL_GC_POISON", "1");
            }
            let output = child.output().expect("run isolated shared recovery child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "{mode}: {}\n{stdout}\n{stderr}",
                output.status
            );
            assert!(
                stdout.contains("SHARED-DEOPT-MOVED-ONCE"),
                "{mode}: child did not execute\n{stdout}\n{stderr}"
            );
            let witness = stdout
                .lines()
                .find(|line| line.contains("SHARED-DEOPT-INSTALLED "))
                .expect("child must fingerprint the installed mapping");
            let field = |line: &str, key: &str| -> Option<String> {
                line.split_whitespace()
                    .find_map(|word| word.strip_prefix(key).map(str::to_owned))
            };
            let code_len = field(witness, "code_len=").unwrap();
            let fingerprint = field(witness, "fingerprint=").unwrap();
            let installed_len = code_len.parse::<usize>().unwrap();
            let chosen: Vec<_> = stderr
                .lines()
                .filter(|line| {
                    line.starts_with("[shared-deopt] function=ORDINARY-SHARED-DEOPT chosen sites=")
                })
                .collect();
            if mode.starts_with("on") {
                assert!(
                    chosen.iter().any(|line| {
                        let number = |key| field(line, key).and_then(|v| v.parse::<usize>().ok());
                        number("sites=").is_some_and(|n| n > 0)
                            && field(line, "code_len=").as_deref() == Some(code_len.as_str())
                            && field(line, "fingerprint=").as_deref() == Some(fingerprint.as_str())
                            && number("helper_start=")
                                .zip(number("helper_end="))
                                .is_some_and(|(start, end)| start < end && end <= installed_len)
                            && number("data_offset=")
                                .zip(number("data_bytes="))
                                .is_some_and(|(offset, bytes)| {
                                    bytes > 0
                                        && offset
                                            .checked_add(bytes)
                                            .is_some_and(|end| end <= installed_len)
                                })
                    }),
                    "{mode}: chosen recipe must match the actual installed bytes and bounds\n{stdout}\n{stderr}"
                );
            } else {
                assert!(
                    chosen.is_empty(),
                    "{mode}: ablation must retain legacy recovery\n{stderr}"
                );
            }
        }
        return;
    }

    let mode = std::env::var(CHILD).unwrap();
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // The minimal unit-test Env skips the production boxing-veneer startup.
    native_transfer_entry::install_box_double_veneer();
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    const NAME: &str = "ORDINARY-SHARED-DEOPT";
    const PARAMS: &str = "(src scale held record)";
    const FORM: &str = "(progn
        (setf (svref record 0) (+ (svref record 0) 1))
        (let* ((x0 (aref src 0))
               (x1 (aref src 1))
               (x2 (aref src 2))
               (x3 (aref src 3))
               (y0 (+ (* x0 scale) x0))
               (y1 (+ (* x1 scale) x1))
               (y2 (+ (* x2 scale) x2))
               (y3 (+ (* x3 scale) x3)))
          (cons held (+ (+ y0 y1) (+ y2 y3)))))";
    // Warm this exact body in T0 with real double arithmetic and a fixnum
    // counter. No opaque helper call can force a tagged fallback compilation.
    crate::cli::read_eval_all_env(&format!("(defun {NAME} {PARAMS} {FORM})"), &mut env).unwrap();
    let symbol = egcl_rt::symbols::intern(NAME);
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    egcl_rt::rooted!(params = reader::read_from_string(PARAMS).unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(&format!("({FORM})")).unwrap().0);
    let body = Arc::new(compile_function(NAME, *params, *forms, &env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    egcl_rt::rooted!(warm_element = egcl_rt::gc::alloc_double_float(1.25));
    egcl_rt::rooted!(
        warm_source = egcl_stdlib::sequences::build_simple_vector(&[*warm_element; 4])
    );
    egcl_rt::rooted!(warm_scale = egcl_rt::gc::alloc_double_float(2.0));
    egcl_rt::rooted!(warm_held = crate::cli::arena_cons(EgclVal::from_fixnum(71), NIL));
    egcl_rt::rooted!(
        warm_record = egcl_stdlib::sequences::build_simple_vector(&[EgclVal::from_fixnum(0)])
    );
    egcl_rt::rooted!(warm_arguments = vec![*warm_source, *warm_scale, *warm_held, *warm_record]);
    for _ in 0..8 {
        egcl_rt::rooted!(
            warm_result = run(Arc::clone(&body), &warm_arguments, *function, &mut env).unwrap()
        );
        assert_eq!(crate::cli::cp(*warm_result).1.as_double_float(), 15.0);
    }
    let input = snapshot_t2_input(symbol, 0).expect("real double-profiled snapshot");
    let arithmetic: Vec<_> = body
        .code
        .iter()
        .enumerate()
        .filter_map(|(bcp, instruction)| {
            if let Instr::CallNamed { sym, .. } = instruction {
                let name = egcl_rt::symbols::symbol_name(*sym).unwrap();
                if matches!(name.as_str(), "+" | "*") {
                    return Some(bcp as u32);
                }
            }
            None
        })
        .collect();
    assert!(!arithmetic.is_empty());
    let mut double_sites = 0;
    let mut counter_sites = 0;
    for bcp in arithmetic {
        let profile = input
            .type_profiles
            .get(&bcp)
            .expect("executed arithmetic profile");
        if profile.double_float >= 8 {
            assert_eq!(profile.fixnum, 0);
            double_sites += 1;
        } else {
            assert!(profile.fixnum >= 8);
            assert_eq!(profile.double_float, 0);
            counter_sites += 1;
        }
    }
    assert_eq!(double_sites, 11);
    assert_eq!(counter_sites, 1);
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("profiled T2 artifact");
    let code = install_t2_completion(T2Completion {
        panicked: false,
        _permit: None,
        dependencies: Vec::new(),
        sym: symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("profiled ordinary installation");
    assert_mapped_t2(&code);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    assert!(
        mapped.ordinary_entry.is_some(),
        "fixture must enter the ordinary T2 ABI"
    );
    // The final-emission diagnostic is evidence only when it fingerprints
    // the bytes of this successfully installed immutable executable mapping.
    let (start, code_len) = mapped
        .ordinary_code_bounds()
        .expect("installed ordinary code bounds");
    let installed = unsafe { std::slice::from_raw_parts(start as *const u8, code_len) };
    let fingerprint = installed.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    println!(
        "SHARED-DEOPT-INSTALLED code_len={} fingerprint={fingerprint:016x}",
        code_len
    );
    let maps = mapped.value_maps_for_test();
    assert!(
        maps.iter()
            .any(|site| matches!(maps.value_map(site.return_offset),
        Some(NativeCallValues::Deoptimizing { f64_slots, .. }) if !f64_slots.is_empty())),
        "installed code must contain a precise raw-F64 deopt map"
    );

    // All input construction except the held cons precedes a real nursery
    // evacuation. The new held cons stays young until actual ordinary entry.
    egcl_rt::rooted!(element = egcl_rt::gc::alloc_double_float(1.25));
    egcl_rt::rooted!(src = egcl_stdlib::sequences::build_simple_vector(&[*element; 4]));
    egcl_rt::rooted!(
        record = egcl_stdlib::sequences::build_simple_vector(&[EgclVal::from_fixnum(0)])
    );
    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(held_argument = crate::cli::arena_cons(EgclVal::from_fixnum(71), NIL));
    egcl_rt::rooted!(
        args = vec![
            *src,
            EgclVal::from_single_float(2.0),
            *held_argument,
            *record
        ]
    );
    // A fixnum is accepted by native double contagion; a single float forces
    // the valid generic numeric continuation after four raw double loads.
    STRESS.with(|slot| slot.set(mode.ends_with("stress")));
    ENTRY_ARGUMENTS.with(|slot| slot.set(args.as_ptr()));
    egcl_rt::rooted!(
        result = native_transfer_entry::with_ordinary_guard_payload_observer_for_test(
            inspect_payload,
            || native_transfer_entry::with_ordinary_entry_hook_for_test(ordinary_entry, || {
                run_native(&code, symbol, &args, &mut env)
            }),
        )
        .unwrap()
    );
    ENTRY_ARGUMENTS.with(|slot| slot.set(std::ptr::null()));
    assert_eq!(
        ENTRY_COUNT.with(Cell::get),
        1,
        "actual ordinary admission must execute once"
    );
    assert_eq!(mapped.deopt_count(), 1);
    assert_eq!(
        egcl_stdlib::sequences::svref(*record, 0).unwrap(),
        EgclVal::from_fixnum(1),
        "committed counter effect must not replay"
    );
    assert_eq!(PAYLOAD_ENTRIES.with(Cell::get), 1);
    egcl_rt::rooted!(held = crate::cli::cp(*result).0);
    egcl_rt::rooted!(sum = crate::cli::cp(*result).1);
    assert_eq!(crate::cli::cp(*held), (EgclVal::from_fixnum(71), NIL));
    assert!(sum.is_double_float());
    assert_eq!(sum.as_double_float(), 15.0);
    // The observer proved nursery evacuation. Subsequent minor collections
    // during T0 arithmetic must retain that already-evacuated marker.
    assert_eq!(held.to_raw(), OBSERVED_MARKER.with(Cell::get));
    assert_ne!(held.to_raw(), ENTRY_MARKER.with(Cell::get));
    assert_eq!(
        *held, args[2],
        "host argument and independent native payload must both relocate"
    );
    assert_eq!(*held, *held_argument);
    for index in 0..4 {
        let value = egcl_stdlib::sequences::svref(*src, index).unwrap();
        assert!(value.is_double_float());
        assert_eq!(value.as_double_float(), 1.25);
    }
    println!("SHARED-DEOPT-MOVED-ONCE {mode}");
}
