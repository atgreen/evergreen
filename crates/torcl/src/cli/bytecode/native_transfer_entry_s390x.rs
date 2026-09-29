//! s390x native-segment entry for the first safe rollout slice.
//!
//! The x86 installer owns the complete native cleanup/capture protocol.  s390x
//! uses a separate ELF64 machine adapter, so it starts with bodies whose T2
//! code cannot call back into Lisp or cross a control scope.  Such a body can
//! enter and leave the native segment directly; every other shape declines to
//! the existing checked native ABI.

#![allow(dead_code)]

use super::*;
use std::cell::RefCell;

use torcl_compiler::t2::build::build_from_bytecode;
use torcl_compiler::t2::emit_s390x::{RuntimeCalls, emit_framed_with_runtime};
use torcl_rt::jit::JitBuffer;
use torcl_rt::native_transfer;

thread_local! {
    static SEGMENT_CACHE: RefCell<std::collections::HashMap<usize, Option<std::rc::Rc<S390xCode>>>> =
        RefCell::new(std::collections::HashMap::new());
}

struct S390xCode {
    body: std::sync::Arc<BytecodeFunction>,
    code: JitBuffer,
    code_len: usize,
    slots: u16,
}

pub(super) fn try_run(
    body: std::sync::Arc<BytecodeFunction>,
    args: &[TorclVal],
    env: &mut Env,
) -> Option<Result<TorclVal, TorclError>> {
    if std::env::var_os("TORCL_NATIVE_TRANSFER") != Some(std::ffi::OsString::from("1"))
        || !native_transfer::is_supported()
        || !native_transfer::current_segment().is_null()
    {
        return None;
    }
    let key = std::sync::Arc::as_ptr(&body) as usize;
    let code = SEGMENT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache
            .entry(key)
            .or_insert_with(|| {
                S390xCode::compile(std::sync::Arc::clone(&body)).map(std::rc::Rc::new)
            })
            .clone()
    });
    code.map(|code| code.run(args, env))
}

impl S390xCode {
    fn compile(body: std::sync::Arc<BytecodeFunction>) -> Option<Self> {
        if body.variadic
            || body.has_env
            || !body.handler_cases.is_empty()
            || !body.handler_binds.is_empty()
            || !body.restart_cases.is_empty()
            || body.code.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instr::CallNamed { .. }
                        | Instr::PushCatch { .. }
                        | Instr::PushHandlerCase { .. }
                        | Instr::PopHandlerCase
                        | Instr::PushHandlerBind { .. }
                        | Instr::PopHandlerBind
                        | Instr::PushRestartCase { .. }
                        | Instr::PopRestartCase
                        | Instr::PushUnwind { .. }
                        | Instr::CleanupReturn
                        | Instr::Throw
                        | Instr::Go { .. }
                        | Instr::PushBlock { .. }
                        | Instr::PushTag { .. }
                        | Instr::ReturnFrom { .. }
                        | Instr::ReturnFromNamed { .. }
                        | Instr::NamedTag { .. }
                        | Instr::GoNamed { .. }
                        | Instr::EnterCleanupNormal { .. }
                )
            })
        {
            return None;
        }
        let function = match build_from_bytecode(&body) {
            Ok(function) => function,
            Err(_) => return None,
        };
        if torcl_compiler::t2::verify::verify(&function).is_err() {
            return None;
        }
        let slots = body.num_slots();
        // The bytecode builder inserts ClearMv at function entry.  The direct
        // caller already establishes a fresh multiple-value context, but the
        // emitter still needs the ordinary nonallocating helper for any later
        // ClearMv instruction.  No other runtime adapter is installed in this
        // narrow segment slice, so calls and allocating operations still decline
        // before reaching this point.
        let runtime = RuntimeCalls {
            multiple_values: super::c2i_clear_mv as extern "C" fn() as usize as u64,
            ..RuntimeCalls::default()
        };
        let emitted = match emit_framed_with_runtime(&function, 0, slots, runtime) {
            Ok(emitted) => emitted,
            Err(_) => return None,
        };
        // The adapter-free entry has no s390x deoptimization continuation yet.
        // Keep guarded arithmetic and other speculative bodies on the checked
        // bytecode/native path until their deopt state is installed.
        if emitted.has_deopt {
            return None;
        }
        let code_len = emitted.code.len();
        let code = JitBuffer::new(&emitted.code)?;
        Some(Self {
            body,
            code,
            code_len,
            slots,
        })
    }

    fn run(&self, args: &[TorclVal], env: &mut Env) -> Result<TorclVal, TorclError> {
        torcl_rt::rooted!(args = args.to_vec());
        torcl_rt::rooted_ref!(_env = &mut *env);
        if args.len() != usize::from(self.body.arity) {
            return Err(TorclError::ProgramError(
                "native transfer entry: wrong argument count".into(),
            ));
        }
        validate_declared_args(&self.body, &args)?;
        env.clear_mv();
        NATIVE_DEPTH.with(|depth| depth.set(depth.get() + 1));
        let _depth = NativeDepthGuard;
        let stack = torcl_rt::current_stack();
        let frame = stack
            .push_frame(NIL, std::ptr::null(), self.slots, FLAG_CALL)
            .ok_or_else(|| {
                TorclError::StackOverflow(
                    torcl_rt::current_fiber_id()
                        .unwrap_or_else(|| torcl_rt::FiberId(torcl_rt::current_thread_id().0)),
                )
            })?;
        struct FrameGuard;
        impl Drop for FrameGuard {
            fn drop(&mut self) {
                torcl_rt::current_stack().pop_frame();
            }
        }
        let _frame = FrameGuard;
        bind_params(&self.body, frame, &args, None);
        let slots = unsafe { frame.add(1).cast::<u64>() };
        if std::env::var_os("TORCL_NATIVE_TRANSFER_DEBUG").is_some() {
            eprintln!(
                "[native-transfer/s390x] direct segment: {} slots, {} bytes",
                self.slots, self.code_len
            );
        }
        let saved_env = super::NATIVE_ENV.with(|env_slot| env_slot.replace(env as *mut Env));
        let outcome =
            unsafe { native_transfer::invoke_native_segment(self.code.as_ptr(), slots, stack) }
                .map_err(|_| TorclError::Internal("s390x native segment unavailable".into()));
        super::NATIVE_ENV.with(|env_slot| env_slot.set(saved_env));
        let outcome = outcome?;
        if outcome.exit != native_transfer::NativeExit::Returned {
            return Err(TorclError::Internal(
                "s390x direct segment returned an unsupported exit".into(),
            ));
        }
        Ok(outcome.value)
    }
}
