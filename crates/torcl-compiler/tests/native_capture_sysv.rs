#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use torcl_compiler::t2::deopt::{LoweredScope, Rebox, SlotDescriptor};
use torcl_compiler::t2::ir::Inst;
use torcl_compiler::t2::mach::{Location, PhysReg, RegClass, StackSlot};
use torcl_compiler::t2::native_transfer::{
    SysvTransferCapture, emit_capture_stub, emit_helper_veneer,
};
use torcl_compiler::t2::transfer_map::TransferCaptureMap;
use torcl_compiler::t2::transfer_sites::{SysvSiteSnapshot, SysvTransferSite, SysvTransferTable};
use torcl_rt::jit::JitBuffer;
use torcl_rt::native_transfer::{self, NativeExit, NativeOutcome, NativeSegment};
use torcl_rt::value::TorclVal;
use torcl_rt::{Collector, HeapCollector, TorclStack};

#[repr(C)]
struct Request {
    anchor: *mut NativeSegment,
    snapshot: *mut u8,
    observed: [u64; 6],
    return_pc: usize,
    stack_bits: u64,
    helper_drops: usize,
    prepare_drops: usize,
    cold_registers: [u64; 6],
    rebuilt_float: f64,
    shadows: *mut TorclVal,
    helper_before_gc: u64,
    helper_after_gc: u64,
    cold_spill: u64,
    exit: NativeExit,
    table: *const SysvTransferTable,
    code_base: usize,
    origin_bcp: u32,
    call_adjust: usize,
}
struct Finished<'a>(&'a mut usize);
impl Drop for Finished<'_> {
    fn drop(&mut self) {
        *self.0 += 1;
    }
}

unsafe extern "C" fn helper(request: *mut u8, out: *mut NativeOutcome) {
    let request = unsafe { &mut *request.cast::<Request>() };
    let exit = if request.helper_drops == 0 {
        NativeExit::Returned
    } else {
        request.exit
    };
    let _drop = Finished(&mut request.helper_drops);
    request.anchor = native_transfer::current_segment();
    if exit != NativeExit::Returned {
        request.helper_before_gc = unsafe { request.shadows.read().to_raw() };
        HeapCollector::new().minor_gc().unwrap();
        request.helper_after_gc = unsafe { request.shadows.read().to_raw() };
    }
    unsafe {
        out.write(NativeOutcome {
            value: request.shadows.read(),
            exit,
        });
    }
}
fn double(value: f64) -> TorclVal {
    let body = torcl_rt::alloc_typed(8, torcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe {
        body.cast::<f64>().write(value);
        TorclVal::from_heap_ptr(body.sub(8))
    }
}
unsafe extern "C" fn prepare(capture: *mut SysvTransferCapture) {
    let capture = unsafe { &mut *capture };
    let request = unsafe { &mut *capture.request.cast::<Request>() };
    let _drop = Finished(&mut request.prepare_drops);
    torcl_rt::rooted!(payload = capture.value);
    request.observed = capture.preserved;
    assert_ne!(
        capture.preserved[0],
        unsafe { request.shadows.read().to_raw() },
        "helper GC left the saved native register stale before shadow restoration"
    );
    request.return_pc = capture.return_pc as usize;
    let table = unsafe { &*request.table };
    request.origin_bcp = table
        .lookup(request.code_base, capture.return_pc as usize)
        .expect("exact captured return PC")
        .map()
        .origin_bcp;
    let snapshot = unsafe { &mut *request.snapshot.cast::<SysvSiteSnapshot<'_>>() };
    unsafe {
        let activation = std::slice::from_raw_parts(request.shadows, 2);
        snapshot
            .capture_from_activation(request.code_base, capture, activation)
            .unwrap();
    }
    torcl_rt::rooted!(
        frames = snapshot
            .reconstruct(|value| {
                HeapCollector::new().minor_gc().unwrap();
                double(value)
            })
            .unwrap()
    );
    // Publish the relocated value back to the register image before native code
    // resumes. The fixture has no other heap roots or cleanup obligations.
    unsafe {
        snapshot.write_back(request.code_base, capture).unwrap();
    }
    for word in capture.preserved.iter_mut().skip(1) {
        *word += 200; // prove the stub reloads every updated nonvolatile
    }
    request.rebuilt_float = frames[0].locals[1].as_double_float();
    capture.value = *payload;
}
#[unsafe(naked)]
unsafe extern "C" fn dispatch(_request: *mut u8, _value: u64, _exit: NativeExit) -> ! {
    core::arch::naked_asm!(
        "endbr64", "lea rax, [rdi + {observed}]",
        "mov [rax], rbx", "mov [rax + 8], rbp", "mov [rax + 16], r12",
        "mov [rax + 24], r13", "mov [rax + 32], r14", "mov [rax + 40], r15",
        "mov rcx, [rdi + {adjust}]",
        "mov rax, [rsp + rcx + 8]", "mov [rdi + {float_spill}], rax",
        "mov rax, [rsp + rcx + 16]", "mov [rdi + {spill}], rax",
        "mov rdi, [rdi]", "jmp {leave}",
        observed = const std::mem::offset_of!(Request, cold_registers),
        spill = const std::mem::offset_of!(Request, cold_spill),
        float_spill = const std::mem::offset_of!(Request, stack_bits),
        adjust = const std::mem::offset_of!(Request, call_adjust),
        leave = sym native_transfer::leave_native_segment,
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn emitted_capture_saves_real_registers_and_stack_before_moving_gc() {
    assert!(
        native_transfer::is_supported(),
        "native capture gate unavailable"
    );
    for exit in [NativeExit::Transfer, NativeExit::Deopt] {
        for call_adjust in [0, 16] {
            exercise_capture(exit, call_adjust);
        }
    }
}

fn exercise_capture(exit: NativeExit, call_adjust: u32) {
    let reg = Location::Register(PhysReg {
        class: RegClass::Gpr,
        encoding: 9,
    }); // allocator RBX
    let spill = Location::Stack(StackSlot(0));
    let heap_spill = Location::Stack(StackSlot(1));
    let map = TransferCaptureMap {
        call: Inst(0),
        machine_inst: 0,
        origin_bcp: 17,
        control_scopes: vec![],
        roots: vec![reg, heap_spill],
        frames: vec![LoweredScope {
            function: 0,
            resume_pc: 17,
            num_locals: 3,
            live_ref_bitmap: vec![true, false, true],
            slots: vec![
                SlotDescriptor::InLocation(reg, Rebox::None),
                SlotDescriptor::InLocation(spill, Rebox::ReboxF64),
                SlotDescriptor::InLocation(heap_spill, Rebox::None),
            ],
        }],
    };
    let capture = JitBuffer::new(&emit_capture_stub(prepare, dispatch as *const u8)).unwrap();
    let veneer = JitBuffer::new(&emit_helper_veneer(helper, capture.as_ptr())).unwrap();
    torcl_rt::rooted!(expected = double(101.0));
    // Model the emitter's activation shadow roots: GC updates these while the
    // Rust helper runs, before cold capture can read the stale native homes.
    torcl_rt::rooted!(shadows = vec![*expected, *expected]);
    let original = expected.to_raw();
    let values = [original, 102, 103, 104, 105, 106];
    let mut code = vec![0xf3, 0x0f, 0x1e, 0xfa, 0x48, 0x83, 0xec, 24];
    code.extend_from_slice(&[0x48, 0xb8]);
    code.extend_from_slice(&3.25f64.to_bits().to_le_bytes());
    code.extend_from_slice(&[0x48, 0x89, 0x04, 0x24]); // caller spill at rsp
    code.extend_from_slice(&[0x48, 0xb8]);
    code.extend_from_slice(&original.to_le_bytes());
    code.extend_from_slice(&[0x48, 0x89, 0x44, 0x24, 8]); // tagged spill at rsp+8
    for (register, value) in [3u8, 5, 12, 13, 14, 15].into_iter().zip(values) {
        code.extend_from_slice(&[0x48 | (register >> 3), 0xb8 | (register & 7)]);
        code.extend_from_slice(&value.to_le_bytes());
    }
    code.extend_from_slice(&[0x48, 0x83, 0xec, call_adjust as u8]);
    code.extend_from_slice(&[0x48, 0xb8]);
    code.extend_from_slice(&(veneer.as_ptr() as u64).to_le_bytes());
    code.extend_from_slice(&[0xff, 0xd0]);
    let first_return_offset = code.len();
    // The first helper returns normally. The caller has no status test before
    // the second call, which takes the exceptional route through its own site.
    code.extend_from_slice(&[0x48, 0xb8]);
    code.extend_from_slice(&(veneer.as_ptr() as u64).to_le_bytes());
    code.extend_from_slice(&[0xff, 0xd0]);
    let return_offset = code.len();
    code.extend_from_slice(&[0x0f, 0x0b]); // exceptional call must not return here
    let caller = JitBuffer::new(&code).unwrap();
    let mut first_map = map.clone();
    first_map.call = Inst(1);
    first_map.origin_bcp = 9;
    first_map.frames[0].resume_pc = 9;
    let table = SysvTransferTable::new(
        code.len(),
        vec![
            SysvTransferSite {
                return_offset: first_return_offset as u32,
                stack_slots: 2,
                call_stack_adjust: call_adjust,
                activation_slots: 2,
                shadow_roots: vec![(reg, 0), (heap_spill, 1)],
                map: first_map,
            },
            SysvTransferSite {
                return_offset: return_offset as u32,
                stack_slots: 2,
                call_stack_adjust: call_adjust,
                activation_slots: 2,
                shadow_roots: vec![(reg, 0), (heap_spill, 1)],
                map,
            },
        ],
    )
    .unwrap();
    let site = table
        .lookup(
            caller.as_ptr() as usize,
            caller.as_ptr() as usize + return_offset,
        )
        .unwrap();
    let mut snapshot = site.reserve_snapshot().unwrap();
    let mut request = Request {
        anchor: std::ptr::null_mut(),
        snapshot: (&mut snapshot as *mut SysvSiteSnapshot<'_>).cast(),
        observed: [0; 6],
        return_pc: 0,
        stack_bits: 0,
        helper_drops: 0,
        prepare_drops: 0,
        cold_registers: [0; 6],
        rebuilt_float: 0.0,
        shadows: shadows.as_mut_ptr(),
        helper_before_gc: 0,
        helper_after_gc: 0,
        cold_spill: 0,
        exit,
        table: &table,
        code_base: caller.as_ptr() as usize,
        origin_bcp: 0,
        call_adjust: call_adjust as usize,
    };
    let stack = TorclStack::new(64 * 1024);
    let outcome = unsafe {
        native_transfer::invoke_native_segment(
            caller.as_ptr(),
            (&mut request as *mut Request).cast(),
            &stack,
        )
    }
    .unwrap();
    assert_eq!(outcome.exit, exit);
    assert_eq!(outcome.value, *expected, "transfer payload remains rooted");
    assert_eq!(request.observed, values);
    assert_eq!(request.return_pc, caller.as_ptr() as usize + return_offset);
    assert_eq!(request.origin_bcp, 17);
    assert_ne!(
        request.helper_before_gc, request.helper_after_gc,
        "helper relocated the root before capture"
    );
    assert_eq!(request.stack_bits, 3.25f64.to_bits());
    assert_eq!(request.rebuilt_float, 3.25);
    assert_ne!(
        expected.to_raw(),
        original,
        "the actual register root moved"
    );
    assert_eq!(
        request.cold_registers[0],
        expected.to_raw(),
        "reload relocated register before dispatch"
    );
    assert_eq!((request.helper_drops, request.prepare_drops), (2, 1));
    assert_eq!(&request.cold_registers[1..], &[302, 303, 304, 305, 306]);
    assert_eq!(
        request.cold_spill,
        expected.to_raw(),
        "publish relocated caller stack root"
    );
    assert!(native_transfer::current_segment().is_null());
}
