//! Execute frame shapes and independently ask the OS to unwind every decoded
//! instruction boundary, including stack probes and temporary helper storage.
use super::*;
#[cfg(windows)]
use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind, Register};
use egcl_rt::jit::JitBuffer;
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::Debug::*;

#[test]
fn windows_t2_compiled_entry_preserves_overlapping_arguments() {
    use crate::t2::ir::{
        AuxData, Function, IRType, InstData, InstFlags, Opcode, ValueRepresentation,
    };
    #[cfg(windows)]
    use egcl_rt::jit::WindowsUnwindInfo;
    use egcl_rt::value::EgclVal;
    let inst = |opcode, args, aux| InstData {
        opcode,
        args,
        results: vec![],
        aux,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    };
    for arity in 2..=4 {
        for rotation in 0..arity {
            for reverse in [false, true] {
                let mut order: Vec<_> = (0..arity).map(|i| (i + rotation) % arity).collect();
                if reverse {
                    order.reverse();
                }
                let mut f = Function::new("asymmetric-entry");
                let entry = f.entry();
                let params: Vec<_> = (0..arity)
                    .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
                    .collect();
                let mut sum = None;
                let mut expected = 0;
                for (position, &arg) in order.iter().enumerate() {
                    let factor = position as i64 + 1;
                    let (_, c) = f.push_inst(
                        entry,
                        inst(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(factor)),
                        &[(IRType::TOP, ValueRepresentation::Tagged)],
                    );
                    let (_, product) = f.push_inst(
                        entry,
                        inst(Opcode::FixnumMul, vec![params[arg], c[0]], AuxData::None),
                        &[(IRType::TOP, ValueRepresentation::Tagged)],
                    );
                    sum = Some(match sum {
                        None => product[0],
                        Some(previous) => {
                            f.push_inst(
                                entry,
                                inst(Opcode::FixnumAdd, vec![previous, product[0]], AuxData::None),
                                &[(IRType::TOP, ValueRepresentation::Tagged)],
                            )
                            .1[0]
                        }
                    });
                    expected += (arg as i64 + 1) * 10 * factor;
                }
                f.set_terminator(
                    entry,
                    inst(Opcode::Return, vec![sum.unwrap()], AuxData::None),
                );
                let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, 0, 0, None).unwrap();
                assert_ne!(framed.compiled_entry, 0);
                #[cfg(windows)]
                let ranges: Vec<_> = framed
                    .windows_unwind
                    .iter()
                    .map(|range| WindowsUnwindInfo {
                        begin: range.begin,
                        end: range.end,
                        unwind_info: &range.unwind_info,
                    })
                    .collect();
                #[cfg(windows)]
                let code =
                    unsafe { JitBuffer::new_with_windows_unwind_ranges(&framed.code, &ranges) }
                        .unwrap();
                #[cfg(not(windows))]
                let code = JitBuffer::new(&framed.code).unwrap();
                let args: Vec<_> = (0..arity)
                    .map(|i| EgclVal::from_fixnum((i as i64 + 1) * 10).0)
                    .collect();
                let result =
                    call_compiled_entry(unsafe { code.as_ptr().add(framed.compiled_entry) }, &args);
                assert_eq!(
                    EgclVal(result).as_fixnum(),
                    expected,
                    "arity={arity}, order={order:?}"
                );
            }
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_t2_large_precise_deopt_buffer_preserves_frame_values() {
    use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use crate::t2::ir::{Opcode, ValueRepresentation};
    use egcl_rt::jit::WindowsUnwindInfo;
    use egcl_rt::value::NIL;
    const LOCALS: usize = 4096;
    extern "C" fn reconstruct(scopes: u64, words: u64, data: *const u64, reserved: u64) -> u64 {
        // Return a failure value instead of unwinding a Rust assertion across
        // the generated frame if the ABI/serialization itself is broken.
        if scopes != 2 || words != (2 * (LOCALS + 4)) as u64 || reserved != 0 {
            return 0;
        }
        let data = unsafe { std::slice::from_raw_parts(data, words as usize) };
        for (i, frame) in data.chunks_exact(LOCALS + 4).enumerate() {
            if frame[..4] != [7 + i as u64, 9, LOCALS as u64, 0]
                || frame[4..].iter().any(|&value| value != NIL.0)
            {
                return 0;
            }
        }
        77
    }
    let mut f = super::tests::build_mul_ranged(5, None);
    let entry = f.entry();
    let value = f.block(entry).params[0];
    let fs = f.frame_states.add(FrameState {
        scopes: (0..2)
            .map(|i| FrameScope {
                function: 7 + i,
                bcp: 9,
                locals: vec![
                    ValueSource::Value {
                        value,
                        repr: ValueRepresentation::Tagged
                    };
                    LOCALS
                ],
                stack: vec![],
            })
            .collect(),
        remat: vec![],
    });
    let mul = *f
        .block(entry)
        .insts
        .iter()
        .find(|&&inst| f.inst(inst).opcode == Opcode::FixnumMul)
        .unwrap();
    f.inst_mut(mul).frame_state = Some(fs);
    let framed = emit_framed(
        &f,
        0,
        reconstruct as *const () as u64,
        0,
        0,
        0,
        0,
        0,
        0,
        None,
    )
    .unwrap();
    let ranges: Vec<_> = framed
        .windows_unwind
        .iter()
        .map(|range| WindowsUnwindInfo {
            begin: range.begin,
            end: range.end,
            unwind_info: &range.unwind_info,
        })
        .collect();
    let code = unsafe { JitBuffer::new_with_windows_unwind_ranges(&framed.code, &ranges) }.unwrap();
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(code.as_ptr()) };
    let mut slots = [NIL.0];
    assert_eq!(run(slots.as_mut_ptr()), 77);
}

#[cfg(windows)]
fn register_index(reg: Register) -> usize {
    match reg {
        Register::RAX => 0,
        Register::RCX => 1,
        Register::RDX => 2,
        Register::RBX => 3,
        Register::RSP => 4,
        Register::RBP => 5,
        Register::RSI => 6,
        Register::RDI => 7,
        Register::R8 => 8,
        Register::R9 => 9,
        Register::R10 => 10,
        Register::R11 => 11,
        Register::R12 => 12,
        Register::R13 => 13,
        Register::R14 => 14,
        Register::R15 => 15,
        _ => panic!("unexpected frame register: {reg:?}"),
    }
}

#[cfg(windows)]
#[test]
fn windows_t2_frame_unwinds_at_every_instruction_boundary() {
    use egcl_rt::jit::WindowsUnwindInfo;
    for saved in [&[][..], &[3, 12][..], &[3, 12, 13, 14, 15][..]] {
        // Exercise no allocation, small/large encoding boundaries, multi-page
        // probing, and the unscaled 32-bit UWOP_ALLOC_LARGE representation.
        for spills in [0, 16, 128, 512, 8192, 600_000] {
            let frame = WindowsFrame::new(saved, spills).unwrap();
            let mut asm = Asm::new();
            let body = asm.label();
            let unwind = frame.prologue(&mut asm);
            asm.bind(body);
            for &reg in &frame.saved {
                if reg != 5 {
                    mov_imm64(&mut asm, reg, 0xDEAD);
                }
            }
            // Runtime helper reserve and a nested deopt buffer: RBP must remain
            // the unwind anchor even when RSP is below the fixed spill homes.
            asm.extend_from_slice(&[0x48, 0x83, 0xEC, 112, 0x90]);
            asm.extend_from_slice(&[0x48, 0x83, 0xEC, 48, 0x90]);
            asm.extend_from_slice(&[0x48, 0x83, 0xC4, 48]);
            asm.extend_from_slice(&[0x48, 0x83, 0xC4, 112]);
            mov_imm64(&mut asm, RAX, 42);
            frame.epilogue(&mut asm);
            asm.push(0xC3);
            let alternate = asm.here();
            let alternate_unwind = frame.prologue(&mut asm);
            jump_to_shared_body(&mut asm, body);
            let bytes = asm.finish().unwrap();
            let ranges = [
                WindowsUnwindInfo {
                    begin: 0,
                    end: alternate as u32,
                    unwind_info: &unwind,
                },
                WindowsUnwindInfo {
                    begin: alternate as u32,
                    end: bytes.len() as u32,
                    unwind_info: &alternate_unwind,
                },
            ];
            let code =
                unsafe { JitBuffer::new_with_windows_unwind_ranges(&bytes, &ranges) }.unwrap();
            let run: extern "C" fn() -> u64 = unsafe { std::mem::transmute(code.as_ptr()) };
            assert_eq!(run(), 42, "{saved:?}, spills={spills}");
            let enter_alternate: extern "C" fn() -> u64 =
                unsafe { std::mem::transmute(code.as_ptr().add(alternate)) };
            assert_eq!(
                enter_alternate(),
                42,
                "alternate entry: {saved:?}, spills={spills}"
            );

            // Simulate only machine state relevant to unwinding, from decoded
            // instructions rather than deriving it from the unwind records.
            let mut stack = vec![0u64; spills / 8 + 128];
            let entry_sp = unsafe { stack.as_mut_ptr().add(stack.len() - 16) } as u64;
            let mut regs: [u64; 16] = std::array::from_fn(|i| 0x1000 + i as u64);
            regs[4] = entry_sp;
            let original = regs;
            const RETURN_IP: u64 = 0x1234_5678;
            unsafe { (entry_sp as *mut u64).write(RETURN_IP) };
            let address = code.as_ptr() as u64;
            let mut decoder = Decoder::with_ip(64, &bytes, address, DecoderOptions::NONE);
            while decoder.can_decode() {
                let ins = decoder.decode();
                assert!(!ins.is_invalid());
                let mut context: CONTEXT = unsafe { std::mem::zeroed() };
                context.Rip = ins.ip();
                context.Rsp = regs[4];
                context.Rbx = regs[3];
                context.Rbp = regs[5];
                context.Rsi = regs[6];
                context.Rdi = regs[7];
                context.R12 = regs[12];
                context.R13 = regs[13];
                context.R14 = regs[14];
                context.R15 = regs[15];
                let mut base = 0;
                unsafe {
                    let entry = RtlLookupFunctionEntry(ins.ip(), &mut base, std::ptr::null_mut());
                    assert!(!entry.is_null());
                    let mut data = std::ptr::null_mut();
                    let mut establisher = 0;
                    RtlVirtualUnwind(
                        0,
                        base,
                        ins.ip(),
                        entry,
                        &mut context,
                        &mut data,
                        &mut establisher,
                        std::ptr::null_mut(),
                    );
                }
                let offset = ins.ip() - address;
                assert_eq!(
                    (context.Rip, context.Rsp),
                    (RETURN_IP, entry_sp + 8),
                    "spills={spills}, saved={saved:?}, offset={offset}"
                );
                assert_eq!(
                    [
                        context.Rbx,
                        context.Rbp,
                        context.Rsi,
                        context.Rdi,
                        context.R12,
                        context.R13,
                        context.R14,
                        context.R15
                    ],
                    [3, 5, 6, 7, 12, 13, 14, 15].map(|r| original[r]),
                    "nonvolatile regs: spills={spills}, offset={offset}"
                );
                match ins.mnemonic() {
                    Mnemonic::Push => {
                        regs[4] -= 8;
                        unsafe {
                            (regs[4] as *mut u64).write(regs[register_index(ins.op0_register())])
                        };
                    }
                    Mnemonic::Pop => {
                        regs[register_index(ins.op0_register())] =
                            unsafe { (regs[4] as *const u64).read() };
                        regs[4] += 8;
                    }
                    Mnemonic::Sub | Mnemonic::Add if ins.op0_register() == Register::RSP => {
                        let n = ins.immediate(1);
                        if ins.mnemonic() == Mnemonic::Sub {
                            regs[4] -= n;
                        } else {
                            regs[4] += n;
                        }
                    }
                    Mnemonic::Lea if ins.op0_register() == Register::RSP => {
                        regs[4] = regs[register_index(ins.memory_base())]
                            .wrapping_add(ins.memory_displacement64());
                    }
                    Mnemonic::Mov
                        if (Register::RAX..=Register::R15).contains(&ins.op0_register()) =>
                    {
                        let dst = register_index(ins.op0_register());
                        regs[dst] = if ins.op1_kind() == OpKind::Register {
                            regs[register_index(ins.op1_register())]
                        } else {
                            ins.immediate(1)
                        };
                    }
                    // Probe-loop branches/arithmetic don't change RSP or any
                    // nonvolatile state, so one linear visit covers every PC.
                    _ => {}
                }
            }
        }
    }
}

// Bridge the host ABI to the compiler's four-register convention.
fn call_compiled_entry(entry: *const u8, args: &[u64]) -> u64 {
    let mut wrapper = Asm::new();
    #[cfg(windows)]
    let frame = WindowsFrame::new(&[], 0).unwrap();
    #[cfg(windows)]
    let unwind = frame.prologue(&mut wrapper);
    #[cfg(not(windows))]
    push_reg(&mut wrapper, 5); // align RSP before the call
    for (&r, &value) in [1, 8, 9, 10].iter().zip(args) {
        mov_imm64(&mut wrapper, r, value as i64);
    }
    mov_imm64(&mut wrapper, RAX, entry as i64);
    wrapper.extend_from_slice(&[0xFF, 0xD0]);
    #[cfg(windows)]
    frame.epilogue(&mut wrapper);
    #[cfg(not(windows))]
    pop_reg(&mut wrapper, 5);
    wrapper.push(0xC3);
    let bytes = wrapper.finish().unwrap();
    #[cfg(windows)]
    let wrapper = unsafe { JitBuffer::new_with_windows_unwind(&bytes, &unwind) }.unwrap();
    #[cfg(not(windows))]
    let wrapper = JitBuffer::new(&bytes).unwrap();
    let run: extern "C" fn() -> u64 = unsafe { std::mem::transmute(wrapper.as_ptr()) };
    run()
}

#[cfg(windows)]
#[test]
fn windows_t2_stack_probe_preserves_four_compiled_arguments() {
    let frame = WindowsFrame::new(&[], 8192).unwrap();
    let mut asm = Asm::new();
    let unwind = frame.prologue(&mut asm);
    mov_rr(&mut asm, RAX, 1);
    for reg in [8, 9, 10] {
        alu_rr(&mut asm, 0x01, RAX, reg);
    }
    frame.epilogue(&mut asm);
    asm.push(0xC3);
    let code =
        unsafe { JitBuffer::new_with_windows_unwind(&asm.finish().unwrap(), &unwind) }.unwrap();
    let args = [0x1000_0001, 0x2000_0002, 0x4000_0004, 0x8000_0008];
    assert_eq!(call_compiled_entry(code.as_ptr(), &args), args.iter().sum());
}
