//! Shared Win64 adapter frames: shadow space, guard-page probing and unwind data.
use crate::{EgclError, jit::JitBuffer};

pub(super) const ARGUMENT_REGISTERS: [u8; 4] = [1, 2, 8, 9];

/// Start a fixed frame without touching incoming argument registers. RBP
/// points at the bottom of the allocation; RAX and R11 are scratch.
pub(super) fn prologue(frame: u32) -> Result<(Vec<u8>, Vec<u8>), EgclError> {
    if frame < 32 || frame % 16 != 0 || frame > i32::MAX as u32 {
        return Err(EgclError::FfiError(
            "invalid Win64 adapter frame size".into(),
        ));
    }
    let mut code = vec![0xf3, 0x0f, 0x1e, 0xfa, 0x55]; // endbr64; push rbp
    if frame >= 4096 {
        // Touch each guard page before moving RSP. Keeping RSP unchanged
        // throughout the loop makes every probe unwindable from one push.
        code.extend_from_slice(&[0x48, 0x89, 0xe0]); // mov rax,rsp
        code.extend_from_slice(&[0x41, 0xbb]); // mov r11d,frame
        code.extend_from_slice(&frame.to_le_bytes());
        let probe = code.len();
        code.extend_from_slice(&[0x48, 0x2d, 0, 0x10, 0, 0]); // sub rax,4096
        code.extend_from_slice(&[0xf6, 0x00, 0]); // test byte [rax],0
        code.extend_from_slice(&[0x49, 0x81, 0xeb, 0, 0x10, 0, 0]); // sub r11,4096
        code.extend_from_slice(&[0x49, 0x81, 0xfb, 0, 0x10, 0, 0]); // cmp r11,4096
        let displacement = (probe as isize - (code.len() + 2) as isize) as i8;
        code.extend_from_slice(&[0x73, displacement as u8]); // jae probe
        code.extend_from_slice(&[0x4c, 0x29, 0xd8, 0xf6, 0x00, 0]); // sub rax,r11; touch tail
    }
    code.extend_from_slice(&[0x48, 0x81, 0xec]); // sub rsp,frame
    code.extend_from_slice(&frame.to_le_bytes());
    let allocation_end = code.len() as u8;
    code.extend_from_slice(&[0x48, 0x89, 0xe5]); // mov rbp,rsp
    let prologue_end = code.len() as u8;

    // Version 1, frame register RBP with offset zero. Unwind operations are
    // sorted by descending instruction offset, including multi-slot operands.
    let mut unwind = vec![1, prologue_end, 0, 5, prologue_end, 3]; // UWOP_SET_FPREG
    if frame <= 128 {
        unwind.extend_from_slice(&[allocation_end, (((frame - 8) / 8) as u8) << 4 | 2]);
    } else if frame / 8 <= u16::MAX as u32 {
        unwind.extend_from_slice(&[allocation_end, 1]); // UWOP_ALLOC_LARGE, scaled u16
        unwind.extend_from_slice(&((frame / 8) as u16).to_le_bytes());
    } else {
        unwind.extend_from_slice(&[allocation_end, 0x11]); // unscaled u32 form
        unwind.extend_from_slice(&frame.to_le_bytes());
    }
    unwind.extend_from_slice(&[5, 0x50]); // UWOP_PUSH_NONVOL RBP
    unwind[2] = ((unwind.len() - 4) / 2) as u8;
    unwind.resize(unwind.len().div_ceil(4) * 4, 0);
    Ok((code, unwind))
}

pub(super) fn finish(mut code: Vec<u8>, frame: u32, unwind: &[u8]) -> Option<JitBuffer> {
    // Windows recognizes this epilogue when unwinding inside it. LEAVE is
    // deliberately not used: it is not a permitted Win64 unwind epilogue.
    code.extend_from_slice(&[0x48, 0x81, 0xc4]); // add rsp,frame
    code.extend_from_slice(&frame.to_le_bytes());
    code.extend_from_slice(&[0x5d, 0xc3]); // pop rbp; ret
    // SAFETY: prologue/finish emit the exact operations described by `unwind`;
    // adapter bodies do not change RSP or nonvolatile registers.
    unsafe { JitBuffer::new_with_windows_unwind(&code, unwind) }
}
