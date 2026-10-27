// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Private semantic operations called through the ordinary Lisp ABI. Their
//! permanently interned identities own stable call-table slots. Dispatch selects
//! their semantics directly, independently of public function-cell bindings.

use super::*;
use egcl_compiler::t2::emit::OrdinaryPrimitive;

#[derive(Clone, Copy)]
struct ArraySymbols {
    make_array: u32,
    element_type: u32,
    unsigned_byte: u32,
}

static ARRAY_SYMBOLS: std::sync::OnceLock<ArraySymbols> = std::sync::OnceLock::new();

impl ArraySymbols {
    fn capture() -> Self {
        Self {
            make_array: egcl_rt::symbols::intern("MAKE-ARRAY"),
            element_type: egcl_rt::symbols::intern("KEYWORD:ELEMENT-TYPE"),
            unsigned_byte: egcl_rt::symbols::intern("UNSIGNED-BYTE"),
        }
    }
}

// No allocation, interning or callback occurs while inspecting these objects.
// In particular, a heap literal/DEFCONSTANT descriptor is not assumed immutable.
fn simple_ub8_length(args: &[EgclVal], symbols: ArraySymbols) -> Option<usize> {
    if args.len() != 3
        || args[1] != EgclVal::from_symbol_index(symbols.element_type)
        || !args[2].is_cons()
    {
        return None;
    }
    let (head, tail) = cp(args[2]);
    if head != EgclVal::from_symbol_index(symbols.unsigned_byte) || !tail.is_cons() {
        return None;
    }
    let (width, end) = cp(tail);
    if width != EgclVal::from_fixnum(8) || !end.is_nil() {
        return None;
    }
    let dimension = if args[0].is_cons() {
        let (length, tail) = cp(args[0]);
        if !tail.is_nil() {
            return None;
        }
        length
    } else {
        args[0]
    };
    if !dimension.is_fixnum() || dimension.as_fixnum() < 0 {
        return None;
    }
    let length = dimension.as_fixnum() as usize;
    let bytes = length.checked_add(egcl_rt::packed_u8::DATA_OFFSET)?;
    (bytes <= u32::MAX as usize - 32 && bytes <= isize::MAX as usize - 32)
        .then_some(length)
}

fn note_array_constructor(fast: bool) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("EGCL_NATIVE_ARRAY_CONSTRUCTOR_STATS").is_some()) {
        return;
    }
    static FAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static FALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = if fast { &FAST } else { &FALLBACK };
    if counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
        eprintln!(
            "[native-array-constructor] {} count=1",
            if fast { "fast" } else { "fallback" }
        );
    }
}

#[cfg(test)]
thread_local! {
    static ARRAY_RUNTIME_ENTRIES: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn take_array_runtime_entries_for_test() -> usize {
    ARRAY_RUNTIME_ENTRIES.with(|entries| entries.replace(0))
}

fn make_array(args: &[EgclVal], env: *mut Env) -> Result<EgclVal, EgclError> {
    #[cfg(test)]
    ARRAY_RUNTIME_ENTRIES.with(|entries| entries.set(entries.get() + 1));
    let symbols = *ARRAY_SYMBOLS.get().ok_or_else(invalid_capture)?;
    if let Some(length) = simple_ub8_length(args, symbols) {
        note_array_constructor(true);
        // Decoding cannot allocate and rechecks mutable descriptors each call.
        // Only the scalar length survives this collecting allocation; no local
        // argument copy or borrowed heap object is used afterward.
        return egcl_rt::packed_u8::make(length, 0);
    }
    // Public fallback may allocate and still needs every argument afterward.
    egcl_rt::rooted!(args = args.to_vec());
    note_array_constructor(false);
    egcl_rt::rooted!(
        function = egcl_rt::symbols::symbol_function(symbols.make_array)
            .unwrap_or_else(|| EgclVal::from_symbol_index(symbols.make_array))
    );
    // This is the public function, whose body calls %MAKE-SIMPLE-ARRAY. Its
    // cell is never replaced by our private call-site adapter, so no recursion
    // through this adapter is possible on an unsupported request.
    apply_function(*function, &args, unsafe { &mut *env })
}

static SYMBOLS: std::sync::OnceLock<Vec<(OrdinaryPrimitive, u32)>> = std::sync::OnceLock::new();

fn symbols() -> &'static Vec<(OrdinaryPrimitive, u32)> {
    SYMBOLS.get_or_init(|| {
        ARRAY_SYMBOLS.get_or_init(ArraySymbols::capture);
        OrdinaryPrimitive::ALL
            .into_iter()
            .map(|primitive| {
                let target = egcl_rt::symbols::intern(&primitive.adapter_name());
                if primitive == OrdinaryPrimitive::MakeArray {
                    let symbols = *ARRAY_SYMBOLS.get().expect("captured array symbols");
                    // Only the owner of this private semantic adapter may
                    // publish its immediate identities to background emission.
                    egcl_compiler::t2::emit::register_packed_u8_adapter(
                        target,
                        egcl_compiler::t2::emit::PackedU8AdapterSymbols {
                            element_type: symbols.element_type,
                            unsigned_byte: symbols.unsigned_byte,
                        },
                    );
                }
                (primitive, target)
            })
            .collect()
    })
}

pub(super) fn lower(
    ir: &mut egcl_compiler::t2::ir::Function,
    cells: &mut Vec<(u32, Arc<egcl_rt::call_table::CallCell>)>,
) -> Option<()> {
    egcl_compiler::t2::emit::lower_ordinary_primitives(ir, |primitive| {
        let symbol = symbols().iter().find(|(kind, _)| *kind == primitive)?.1;
        if !cells.iter().any(|(existing, _)| *existing == symbol) {
            cells.push((symbol, super::super::call_table::resolve(symbol)?));
        }
        Some(symbol)
    })
    .ok()?;
    Some(())
}

pub(in crate::cli::bytecode) fn is_private(symbol: u32) -> bool {
    SYMBOLS
        .get()
        .is_some_and(|symbols| symbols.iter().any(|(_, id)| *id == symbol))
}

pub(in crate::cli::bytecode) fn invoke(
    symbol: u32,
    args: &[EgclVal],
) -> Option<Result<EgclVal, EgclError>> {
    // Do not initialize private symbols during normal named-call dispatch.
    // Lowering creates them before it publishes any primitive call sites.
    let primitive = SYMBOLS.get()?.iter().find(|(_, id)| *id == symbol)?.0;
    Some(guard_c2i(|| {
        let env = NATIVE_ENV.with(Cell::get);
        if env.is_null() {
            return Err(invalid_capture());
        }
        unsafe {
            (*env).clear_mv();
        }
        use OrdinaryPrimitive::*;
        let expected = match primitive {
            LoadGlobal | LoadFunction => 1,
            MakeArray => 3,
            _ => 2,
        };
        if args.len() != expected {
            return Err(invalid_capture());
        }
        let result = match primitive {
            LoadGlobal => EgclVal(c2i_load_global(u64::from(args[0].as_symbol_index()))),
            LoadFunction => EgclVal(c2i_load_function(u64::from(args[0].as_symbol_index()))),
            StoreGlobal => {
                c2i_store_global(u64::from(args[0].as_symbol_index()), args[1].to_raw());
                NIL
            }
            Cons => EgclVal(c2i_alloc_cons(args[0].to_raw(), args[1].to_raw())),
            MakeArray => make_array(args, env)?,
            _ => {
                let name = match primitive {
                    Add => "+",
                    Subtract => "-",
                    Multiply => "*",
                    Divide => "/",
                    Equal => "EQUAL",
                    _ => unreachable!(),
                };
                let slot = super::super::super::direct_builtin_table()
                    .iter()
                    .position(|candidate| *candidate == name)
                    .ok_or_else(invalid_capture)?;
                super::super::super::call_direct_builtin(slot as u32, args, unsafe { &mut *env })
                    .ok_or_else(invalid_capture)??
            }
        };
        Ok(result)
    }))
}

/// Resolve semantic adapters while a Lisp execution owns the symbol and call
/// registries. Workers only consult these immutable symbol/cell bindings.
#[derive(Clone)]
pub(super) struct Bindings(Vec<(OrdinaryPrimitive, u32, Arc<egcl_rt::call_table::CallCell>)>);

impl Bindings {
    pub(super) fn owners(&self) -> Vec<Arc<egcl_rt::call_table::CallCell>> {
        self.0.iter().map(|(_, _, cell)| Arc::clone(cell)).collect()
    }

    pub(super) fn capture() -> Self {
        Self(
            symbols()
                .iter()
                .filter_map(|&(kind, symbol)| {
                    Some((kind, symbol, super::super::call_table::resolve(symbol)?))
                })
                .collect(),
        )
    }

    pub(super) fn lower(
        &self,
        ir: &mut egcl_compiler::t2::ir::Function,
        cells: &mut Vec<(u32, Arc<egcl_rt::call_table::CallCell>)>,
    ) -> Option<()> {
        egcl_compiler::t2::emit::lower_ordinary_primitives(ir, |primitive| {
            let (_, symbol, cell) = self.0.iter().find(|(kind, _, _)| *kind == primitive)?;
            if !cells.iter().any(|(existing, _)| existing == symbol) {
                cells.push((*symbol, Arc::clone(cell)));
            }
            Some(*symbol)
        })
        .ok()?;
        Some(())
    }
}
