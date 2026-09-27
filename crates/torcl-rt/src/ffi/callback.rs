//! Native inbound scalar adapters. These preserve the C ABI while converting
//! arguments to raw slots for a dispatcher; they do not themselves enter Lisp.
//! Closure rooting, runtime transitions and error containment belong to the
//! dispatcher, not to the generated machine-code boundary.

use super::{
    AlienType,
    call::{Location, Scalar, classify},
};
use crate::{TorclError, jit::JitBuffer};

/// A dispatcher receives the stable opaque context and one raw u64 slot per
/// declared argument. It returns raw result bits and MUST NOT unwind into C.
pub type Dispatcher = unsafe extern "C" fn(*mut (), *const u64) -> u64;

/// Owns one distinct W^X C-entry mapping. Foreign users must retire the entry
/// before dropping this owner; GC never controls this native lifetime.
pub struct CallbackAdapter {
    code: JitBuffer,
}

impl CallbackAdapter {
    pub fn new(
        result: &AlienType,
        arguments: &[AlienType],
        context: *mut (),
        dispatcher: Dispatcher,
    ) -> Result<Self, TorclError> {
        let result = Scalar::from_type(result)?;
        let arguments = arguments
            .iter()
            .map(Scalar::from_type)
            .collect::<Result<Vec<_>, _>>()?;
        let plan = classify(&arguments)?;
        let shadow_bytes = if cfg!(windows) { 32 } else { 0 };
        let frame_bytes = (shadow_bytes + arguments.len() * 8).div_ceil(16) * 16;
        // Incoming rsp points at the C return address. Establish an aligned
        // local slot vector without touching any argument register.
        #[cfg(unix)]
        let mut code = vec![
            0xf3, 0x0f, 0x1e, 0xfa, // endbr64
            0x55, // push rbp
            0x48, 0x89, 0xe5, // mov rbp,rsp
            0x48, 0x81, 0xec, // sub rsp,frame_bytes
        ];
        #[cfg(unix)]
        code.extend_from_slice(&(frame_bytes as u32).to_le_bytes());
        #[cfg(windows)]
        let (mut code, unwind) = super::win64::prologue(frame_bytes as u32)?;
        for (index, location) in plan.locations.iter().enumerate() {
            match location {
                Location::Integer(register) => {
                    // mov rax,argument-register; only rax is scratch until all
                    // arguments have been captured in the local slot vector.
                    code.extend_from_slice(&[
                        0x48 | ((register >> 3) << 2),
                        0x89,
                        0xc0 | ((register & 7) << 3),
                    ]);
                }
                Location::Sse(register) => {
                    code.extend_from_slice(&[0x66, 0x48, 0x0f, 0x7e, 0xc0 | (register << 3)]);
                }
                Location::Stack(offset) => {
                    code.extend_from_slice(&[0x48, 0x8b, 0x85]); // mov rax,[rbp+disp32]
                    let incoming_base = if cfg!(windows) { frame_bytes as u32 } else { 0 };
                    code.extend_from_slice(&(incoming_base + 16 + offset).to_le_bytes());
                }
            }
            // Upper argument bits are unspecified by the C ABI. Normalize each
            // slot to its declared width; Lisp signedness is applied later.
            match arguments[index] {
                Scalar::Integer { bits: 8, .. } => code.extend_from_slice(&[0x0f, 0xb6, 0xc0]),
                Scalar::Integer { bits: 16, .. } => code.extend_from_slice(&[0x0f, 0xb7, 0xc0]),
                Scalar::Integer { bits: 32, .. } | Scalar::Float => {
                    code.extend_from_slice(&[0x89, 0xc0])
                }
                _ => {}
            }
            code.extend_from_slice(&[0x48, 0x89, 0x84, 0x24]); // mov [rsp+disp32],rax
            code.extend_from_slice(&((shadow_bytes + index * 8) as u32).to_le_bytes());
        }
        // The dispatcher uses the platform C ABI too.
        code.extend_from_slice(&[0x48, if cfg!(windows) { 0xb9 } else { 0xbf }]);
        code.extend_from_slice(&(context as usize as u64).to_le_bytes());
        #[cfg(unix)]
        code.extend_from_slice(&[0x48, 0x89, 0xe6]); // mov rsi,rsp
        #[cfg(windows)]
        code.extend_from_slice(&[0x48, 0x8d, 0x54, 0x24, 32]); // lea rdx,[rsp+32]
        code.extend_from_slice(&[0x48, 0xb8]); // movabs rax,dispatcher
        code.extend_from_slice(&(dispatcher as usize as u64).to_le_bytes());
        code.extend_from_slice(&[0xff, 0xd0]); // call rax
        match result {
            Scalar::Float => code.extend_from_slice(&[0x66, 0x0f, 0x6e, 0xc0]), // movd xmm0,eax
            Scalar::Double => code.extend_from_slice(&[0x66, 0x48, 0x0f, 0x6e, 0xc0]), // movq xmm0,rax
            Scalar::Integer {
                bits: 8,
                signed: true,
            } => code.extend_from_slice(&[0x0f, 0xbe, 0xc0]),
            Scalar::Integer {
                bits: 16,
                signed: true,
            } => code.extend_from_slice(&[0x0f, 0xbf, 0xc0]),
            Scalar::Integer {
                bits: 8,
                signed: false,
            } => code.extend_from_slice(&[0x0f, 0xb6, 0xc0]),
            Scalar::Integer {
                bits: 16,
                signed: false,
            } => code.extend_from_slice(&[0x0f, 0xb7, 0xc0]),
            Scalar::Void => code.extend_from_slice(&[0x31, 0xc0]),
            _ => {}
        }
        #[cfg(unix)]
        let buffer = {
            code.extend_from_slice(&[0xc9, 0xc3]); // leave; ret
            JitBuffer::new(&code)
        };
        #[cfg(windows)]
        let buffer = super::win64::finish(code, frame_bytes as u32, &unwind);
        let code = buffer
            .ok_or_else(|| TorclError::FfiError("cannot allocate callback adapter".into()))?;
        Ok(Self { code })
    }

    /// The address has the declared foreign signature, not `Dispatcher`'s ABI.
    /// Calling it requires a live adapter and context, matching signature, and
    /// a dispatcher that obeys its own memory and no-unwind contracts.
    pub fn as_fn_ptr(&self) -> *const () {
        self.code.as_ptr().cast()
    }
}
