//! T2 pipeline driver — bytecode → executable native code (spec §4.4, §4.7).
//!
//! Composes the whole T2 tier front-to-back: build SSA from bytecode (P1) →
//! verify (P2) → optional mid-end passes (P3/P4) → lower (P5) → register-allocate
//! (P6) → emit x86-64 (emit.rs) → make executable (`torcl_rt::jit`). The result is
//! a [`CompiledT2`] whose entry pointer is a callable native function.
//!
//! [`compile`] exercises the compact standalone emitter. Live interpreter
//! promotion uses `emit_framed` in the `torcl` crate, but both routes share SSA
//! lowering and regalloc2. Either route declines cleanly when allocation or
//! emission cannot represent a function (spec R4.28).

use torcl_rt::bytecode::BytecodeFunction;
use torcl_rt::jit::JitBuffer;

use crate::t2::build::{BuildError, build_from_bytecode};
use crate::t2::emit::{EmitError, emit};
use crate::t2::lower::lower;
use crate::t2::opt_dce::Dce;
use crate::t2::opt_fold::ConstFold;
use crate::t2::opt_gvn::Gvn;
use crate::t2::pass::PassManager;
use crate::t2::regalloc::allocate;
use crate::t2::verify::{VerifyError, verify};

/// Why T2 compilation did not produce runnable code. In every case the function
/// stays at its current tier (spec R4.28) — a compile failure is never fatal.
#[derive(Clone, Debug)]
pub enum CompileError {
    Build(BuildError),
    Verify(Vec<VerifyError>),
    RegAlloc(regalloc2::RegAllocError),
    Emit(EmitError),
    /// The executable buffer could not be mapped.
    Jit,
}

/// A T2-compiled function: the executable code plus its entry point. Owns the
/// `JitBuffer`, so the code stays mapped for the lifetime of this value.
pub struct CompiledT2 {
    code: JitBuffer,
    code_len: usize,
}

impl CompiledT2 {
    /// The native entry point (callable machine code).
    pub fn entry_ptr(&self) -> *const u8 {
        self.code.as_ptr()
    }

    /// Emitted code size in bytes.
    pub fn len(&self) -> usize {
        self.code_len
    }

    pub fn is_empty(&self) -> bool {
        self.code_len == 0
    }
}

/// Compile `bf` to executable T2 native code. With `opt`, runs a small mid-end
/// pass pipeline (fold → GVN → DCE) before lowering; the result is re-verified.
pub fn compile(bf: &BytecodeFunction, opt: bool) -> Result<CompiledT2, CompileError> {
    // Executable memory is available on Windows, but this standalone emitter
    // still uses SysV registers and frames. Decline until its Win64 ABI port.
    if cfg!(windows) {
        return Err(CompileError::Jit);
    }
    // Front: bytecode → SSA, verified.
    let mut f = build_from_bytecode(bf).map_err(CompileError::Build)?;
    verify(&f).map_err(CompileError::Verify)?;

    // Mid-end (optional): each pass preserves well-formedness (spec §4.10 R4.60).
    if opt {
        let mut pm = PassManager::new();
        pm.add(Box::new(ConstFold));
        pm.add(Box::new(Gvn));
        pm.add(Box::new(Dce));
        pm.run(&mut f);
        verify(&f).map_err(CompileError::Verify)?;
    }

    // Back: lower → allocate → emit → make executable.
    let mut mf = lower(&f);
    allocate(&mut mf).map_err(CompileError::RegAlloc)?;
    let code = emit(&mf).map_err(CompileError::Emit)?;
    let code_len = code.len();
    let buf = JitBuffer::new(&code).ok_or(CompileError::Jit)?;
    Ok(CompiledT2 {
        code: buf,
        code_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use torcl_rt::bytecode::Instr;
    use torcl_rt::value::TorclVal;

    fn bytecode_fn(
        name: &str,
        code: Vec<Instr>,
        constants: Vec<TorclVal>,
        n_locals: u16,
        max_stack: u16,
        arity: u16,
    ) -> BytecodeFunction {
        BytecodeFunction {
            code,
            constants,
            load_time_values: vec![],
            handler_cases: vec![],
            handler_binds: vec![],
            names: vec![],
            restart_cases: vec![],
            nested_functions: vec![],
            param_layout: vec![],
            param_types: vec![],
            has_env: false,
            n_locals,
            max_stack,
            arity,
            name: name.to_string(),
            params_form: torcl_rt::value::NIL,
            min_args: arity,
            max_args: Some(arity),
            variadic: false,
        }
    }

    #[cfg(windows)]
    #[test]
    fn declines_sysv_code_on_windows() {
        let bf = bytecode_fn(
            "answer",
            vec![Instr::Const(0), Instr::Return],
            vec![TorclVal::from_fixnum(42)],
            0,
            1,
            0,
        );
        for opt in [false, true] {
            assert!(matches!(compile(&bf, opt), Err(CompileError::Jit)));
        }
    }

    /// The end-to-end milestone: a bytecode function `(lambda () 42)` compiled
    /// through the WHOLE T2 pipeline and executed — must return the tagged
    /// fixnum 42 that pure interpretation would.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn compiles_and_runs_a_constant_function() {
        let bf = bytecode_fn(
            "answer",
            vec![Instr::Const(0), Instr::Return],
            vec![TorclVal::from_fixnum(42)],
            0,
            1,
            0,
        );

        for opt in [false, true] {
            let compiled = compile(&bf, opt).expect("T2 must compile a constant function");
            // SAFETY: emitted leaf `extern "C" fn() -> u64` returning a TorclVal.
            let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(compiled.entry_ptr()) };
            let result = TorclVal(f());
            assert!(result.is_fixnum(), "result must be a fixnum (opt={opt})");
            assert_eq!(result.as_fixnum(), 42, "T2 must return 42 (opt={opt})");
        }
    }

    /// Tiering primitive: T2 output equals interpretation for the emittable
    /// subset. (`nil` returns the tagged NIL.)
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn compiled_nil_matches_interpretation() {
        let bf = bytecode_fn(
            "givenil",
            vec![Instr::Const(0), Instr::Return],
            vec![torcl_rt::value::NIL],
            0,
            1,
            0,
        );
        let compiled = compile(&bf, false).expect("compile nil");
        let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(compiled.entry_ptr()) };
        assert_eq!(TorclVal(f()), torcl_rt::value::NIL, "T2 must return NIL");
    }

    /// A function the emitter can't handle yet (needs a call) fails cleanly —
    /// the caller keeps it at T1, never crashes (spec R4.28).
    #[test]
    fn unsupported_function_fails_cleanly() {
        // (foo) — a call; P1 lowers CallNamed → Call, which the emitter rejects.
        let bf = bytecode_fn(
            "callish",
            vec![Instr::CallNamed { sym: 1, nargs: 0 }, Instr::Return],
            vec![],
            0,
            1,
            0,
        );
        // Either build/lower/emit rejects it — but it must be an Err, not a panic.
        assert!(
            compile(&bf, false).is_err(),
            "an unsupported function must fail to compile, not crash"
        );
    }
}
