// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Keep the actual generated cache frame alive while a real fiber changes
//! carriers. This raw helper frame contains no Lisp values or GC roots; the
//! full mapped-entry integration test independently exercises precise GC maps.
use super::*;
use egcl_rt::execution_heat::HeatId;
use egcl_rt::thread::{self, FiberId};
use egcl_rt::value::EgclVal;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Probe {
    code: usize,
    id: HeatId,
    real: bool,
    original: AtomicU64,
    resumed: AtomicU64,
    context_before: AtomicUsize,
    context_after: AtomicUsize,
    expected_cell: AtomicUsize,
    selected_before: AtomicUsize,
    selected_after: AtomicUsize,
    cache_word: AtomicUsize,
    fallback: AtomicUsize,
    remaining_before: AtomicU32,
    remaining_after: AtomicU32,
    blocked: AtomicBool,
    release: AtomicBool,
}
static ACTIVE: Mutex<Option<Arc<Probe>>> = Mutex::new(None);

extern "C" fn park(cache: *const *const AtomicU32, fallback: *const AtomicU32) -> u64 {
    let Some(probe) = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return 1;
    };
    let Some(context) = thread::current_execution_context_for_native() else {
        return 1;
    };
    // SAFETY: generated code passes two initialized, aligned words in its live
    // native frame. The selected cell is either that frame's fallback or the
    // execution's retained cell. No Lisp value is stored in this frame.
    let before = unsafe { cache.read() };
    if before.is_null() {
        return 1;
    }
    probe.cache_word.store(cache as usize, Ordering::Release);
    probe.fallback.store(fallback as usize, Ordering::Release);
    probe
        .selected_before
        .store(before as usize, Ordering::Release);
    probe.remaining_before.store(
        unsafe { &*before }.load(Ordering::Relaxed),
        Ordering::Release,
    );
    probe
        .context_before
        .store(context as usize, Ordering::Release);
    probe
        .original
        .store(thread::current_thread_id().0, Ordering::Release);
    if thread::park_current_fiber().is_err() {
        return 1;
    }
    // Re-read the generated frame's pointer word AFTER migration, rather than
    // selecting the binding again or trusting a copied Rust-local pointer.
    let after = unsafe { cache.read() };
    if after.is_null() {
        return 1;
    }
    probe
        .selected_after
        .store(after as usize, Ordering::Release);
    probe.remaining_after.store(
        unsafe { &*after }.load(Ordering::Relaxed),
        Ordering::Release,
    );
    let Some(current) = thread::current_execution_context_for_native() else {
        return 1;
    };
    probe
        .context_after
        .store(current as usize, Ordering::Release);
    probe
        .resumed
        .store(thread::current_thread_id().0, Ordering::Release);
    probe.release.store(true, Ordering::Release);
    0
}

fn fiber() -> EgclVal {
    let probe = ACTIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .unwrap()
        .clone();
    let context = thread::current_execution_context_for_native().unwrap();
    // SAFETY: this running fiber owns the context through native return.
    let execution = unsafe { &*context };
    if probe.real {
        execution.heat().install(probe.id, 4);
        probe.expected_cell.store(
            execution.heat().counter(probe.id).unwrap() as *const _ as usize,
            Ordering::Release,
        );
    }
    // SAFETY: the driver retains this executable until the fiber is joined.
    // The frame preserves RBX/R12 and aligns RSP for its C callback.
    let run: extern "C" fn(*const egcl_rt::exec_context::LispExecutionContext) -> u64 =
        unsafe { std::mem::transmute(probe.code) };
    let result = run(context);
    // Low32 = credited edges, next16 = batch requests, high16 = final counter.
    let expected = if probe.real {
        (4_u64 << 48) | (1_u64 << 32) | 4
    } else {
        (1_u64 << 48) | 4
    };
    let correct = result == expected
        && (!probe.real
            || execution
                .heat()
                .counter(probe.id)
                .unwrap()
                .load(Ordering::Relaxed)
                == 4);
    EgclVal::from_fixnum(i64::from(!correct))
}

fn emit_visit(a: &mut Asm, heat: BaselineHeat, cache: HeatCacheHomes) {
    let slow = a.label();
    let done = a.label();
    let batch = a.label();
    emit_heat_countdown(a, heat, Some(cache), slow);
    a.jmp(done);
    a.bind(slow);
    emit_heat_reload(a, heat, Some(cache));
    mov_imm64(a, 7, 0);
    emit_heat_request(a, heat, Some(cache));
    alu_r_imm(a, 7, 7, 5);
    a.jcc(Cc::E, batch);
    alu_r_imm(a, 0, 3, 1); // fallback: one real edge credited
    a.jmp(done);
    a.bind(batch);
    alu_r_imm(a, 0, 3, heat.batch as i32);
    alu_r_imm(a, 0, 12, 1); // real sampled batch request
    a.bind(done);
}

fn fragment(id: HeatId) -> egcl_rt::jit::JitBuffer {
    let mut a = Asm::new();
    let failed = a.label();
    let exit = a.label();
    let cache = HeatCacheHomes {
        pointer: 0,
        fallback: 1,
    };
    let heat = BaselineHeat {
        counter: BaselineHeatCounter::ExecutionOwned(id),
        batch: 4,
    };
    // Entry RSP%16=8. Two pushes +24-byte frame leave RSP%16=0 at CALL.
    push_reg(&mut a, 3);
    push_reg(&mut a, 12);
    alu_r_imm(&mut a, 5, 4, 24);
    emit_heat_binding(&mut a, id, 7, cache); // ONCE, before suspension
    mov_imm64(&mut a, 3, 0); // RBX: edge credits
    mov_imm64(&mut a, 12, 0); // R12: batch request count
    emit_visit(&mut a, heat, cache);
    a.extend_from_slice(&[0x48, 0x8d, 0xbc, 0x24]); // rdi=&cached pointer
    a.extend_from_slice(&0i32.to_le_bytes());
    a.extend_from_slice(&[0x48, 0x8d, 0xb4, 0x24]); // rsi=&fallback
    a.extend_from_slice(&8i32.to_le_bytes());
    mov_imm64(&mut a, RAX, park as *const () as i64);
    a.extend_from_slice(&[0xff, 0xd0]);
    alu_r_imm(&mut a, 7, RAX, 0);
    a.jcc(Cc::Ne, failed);
    for _ in 0..3 {
        emit_visit(&mut a, heat, cache);
    }
    load_heat_counter(&mut a, heat, Some(cache));
    a.extend_from_slice(&[0x8b, 0x00]); // eax=final u32 countdown
    a.extend_from_slice(&[0x48, 0xc1, 0xe0, 48]);
    mov_rr(&mut a, SCRATCH, 12);
    a.extend_from_slice(&[0x48, 0xc1, 0xe2, 32]);
    alu_rr(&mut a, 0x09, RAX, SCRATCH);
    alu_rr(&mut a, 0x09, RAX, 3);
    a.jmp(exit);
    a.bind(failed);
    mov_imm64(&mut a, RAX, -1);
    a.bind(exit);
    alu_r_imm(&mut a, 0, 4, 24);
    pop_reg(&mut a, 12);
    pop_reg(&mut a, 3);
    a.push(0xc3);
    egcl_rt::jit::JitBuffer::new(&a.finish().unwrap()).unwrap()
}

struct Cleanup<'a> {
    group: &'a egcl_rt::SchedulerGroup,
    probe: Arc<Probe>,
    fiber: Option<FiberId>,
    finished: bool,
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        self.probe.release.store(true, Ordering::Release);
        self.group.set_idle_hook(None);
        if !self.finished {
            if let Some(fiber) = self.fiber {
                let _ = thread::request_fiber_cancellation(fiber);
            }
            let _ = self.group.finish();
        }
        *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[test]
fn execution_heat_cache_and_stack_fallback_survive_forced_actual_carrier_migration() {
    egcl_rt::gc::ensure_heap_initialized();
    thread::current_thread_id();
    let id = HeatId::new().unwrap();
    let code = fragment(id); // identical emitted code for real/missing contexts
    let other = egcl_rt::exec_context::LispExecutionContext::new(
        egcl_rt::exec_context::ExecutionKind::NativeThread,
        77,
        77,
    );
    other.heat().install(id, 4);
    for real in [true, false] {
        let probe = Arc::new(Probe {
            code: code.as_ptr() as usize,
            id,
            real,
            original: AtomicU64::new(0),
            resumed: AtomicU64::new(0),
            context_before: AtomicUsize::new(0),
            context_after: AtomicUsize::new(0),
            expected_cell: AtomicUsize::new(0),
            selected_before: AtomicUsize::new(0),
            selected_after: AtomicUsize::new(0),
            cache_word: AtomicUsize::new(0),
            fallback: AtomicUsize::new(0),
            remaining_before: AtomicU32::new(0),
            remaining_after: AtomicU32::new(0),
            blocked: AtomicBool::new(false),
            release: AtomicBool::new(false),
        });
        *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&probe));
        let group =
            egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers: 2 }).unwrap();
        let mut cleanup = Cleanup {
            group: &group,
            probe: Arc::clone(&probe),
            fiber: None,
            finished: false,
        };
        let hook = Arc::clone(&probe);
        group.set_idle_hook(Some(Arc::new(move || {
            if hook.original.load(Ordering::Acquire) != thread::current_thread_id().0 {
                return;
            }
            // Existing deterministic R13 migration protocol: the hook runs
            // after scheduler locks are released. Exclude only the original
            // carrier while the second resumes the parked native stack.
            let _blocked = unsafe { egcl_rt::safepoint::NativeBlockingScope::enter() };
            hook.blocked.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !hook.release.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
        })));
        let entry = unsafe { EgclVal::from_function_ptr(fiber as *const () as *mut u8) };
        let fiber = thread::make_fiber(entry).unwrap();
        group.submit(fiber).unwrap();
        cleanup.fiber = Some(fiber);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !probe.blocked.load(Ordering::Acquire) && Instant::now() < deadline {
            egcl_rt::poll_safepoint();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            probe.blocked.load(Ordering::Acquire),
            "original carrier must be excluded"
        );
        group.unpark(fiber).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while probe.resumed.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            egcl_rt::poll_safepoint();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_ne!(
            probe.resumed.load(Ordering::Acquire),
            0,
            "second carrier must resume while the original remains excluded"
        );
        probe.release.store(true, Ordering::Release);
        group.set_idle_hook(None);
        let result = group.finish();
        cleanup.finished = true;
        assert_eq!(
            result.unwrap(),
            vec![EgclVal::from_fixnum(0)],
            "exact credits/reloads after resume"
        );
        assert_ne!(probe.original.load(Ordering::Acquire), 0);
        assert_ne!(probe.resumed.load(Ordering::Acquire), 0);
        assert_ne!(
            probe.original.load(Ordering::Acquire),
            probe.resumed.load(Ordering::Acquire),
            "must change actual carrier, not just metadata"
        );
        assert_eq!(
            probe.context_before.load(Ordering::Acquire),
            probe.context_after.load(Ordering::Acquire)
        );
        assert_eq!(
            probe.selected_before.load(Ordering::Acquire),
            probe.selected_after.load(Ordering::Acquire),
            "generated cache word itself must survive migration"
        );
        assert_eq!(
            probe.fallback.load(Ordering::Acquire),
            probe.cache_word.load(Ordering::Acquire) + 8
        );
        assert_eq!(
            probe.remaining_before.load(Ordering::Acquire),
            if real { 3 } else { 1 }
        );
        assert_eq!(
            probe.remaining_after.load(Ordering::Acquire),
            if real { 3 } else { 1 }
        );
        assert_eq!(
            probe.selected_after.load(Ordering::Acquire),
            if real {
                probe.expected_cell.load(Ordering::Acquire)
            } else {
                probe.fallback.load(Ordering::Acquire)
            }
        );
        assert_eq!(
            other.heat().counter(id).unwrap().load(Ordering::Relaxed),
            4,
            "foreign execution countdown must remain untouched"
        );
    }
}
