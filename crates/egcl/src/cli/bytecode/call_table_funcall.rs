// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native FUNCALL dispatch. Recent callables form a weak, execution-local cache;
//! compiled entries are shared by code version rather than closure instance.
//! A bounded set of recent code versions (and their constants) is retained
//! strongly, along with active versions. Cached identity never roots an instance.
use super::call_table_native::{NativeEntry, immediate, memory};
use super::*;
use std::cell::Cell;

struct CachedCallable {
    function: Cell<u64>,
    target: [Cell<usize>; 2],
}

impl CachedCallable {
    fn clear(&self) {
        self.function.set(NIL.0);
        self.target.iter().for_each(|target| target.set(0));
    }
}

pub(super) struct Cache {
    callables: [CachedCallable; 4],
    next_callable: usize,
    revision: Option<u64>,
    dispatch: Vec<egcl_rt::jit::JitBuffer>,
    #[allow(clippy::vec_box)] // native code embeds stable descriptor addresses
    versions: Vec<Box<NativeEntry>>,
}

impl Cache {
    pub(super) fn new(fallback: [usize; 2]) -> Option<Box<Self>> {
        let mut cache = Box::new(Self {
            callables: std::array::from_fn(|_| CachedCallable {
                function: Cell::new(NIL.0),
                target: std::array::from_fn(|_| Cell::new(0)),
            }),
            next_callable: 0,
            revision: None,
            dispatch: Vec::new(),
            versions: Vec::new(),
        });
        for (slice, fallback) in [false, true].into_iter().zip(fallback) {
            let mut a = Asm::new();
            let slow = a.label();
            // rdi=FUNCALL cell, rsi=count including designator, rdx=arg0/slice.
            a.extend_from_slice(&[0x48, 0x85, 0xf6]); // test rsi,rsi
            a.jcc(Cc::E, slow);
            if slice {
                memory(&mut a, 0x8b, 0, 2, 0);
            } else {
                a.extend_from_slice(&[0x48, 0x89, 0xd0]);
            } // mov rax,rdx
            for callable in &cache.callables {
                let next = a.label();
                immediate(&mut a, 10, callable.function.as_ptr() as u64);
                memory(&mut a, 0x3b, 0, 10, 0); // cmp rax,[r10]
                a.jcc(Cc::Ne, next);
                immediate(
                    &mut a,
                    11,
                    callable.target[usize::from(slice)].as_ptr() as u64,
                );
                memory(&mut a, 0x8b, 11, 11, 0);
                a.extend_from_slice(&[0x4d, 0x85, 0xdb]); // test r11,r11
                a.jcc(Cc::E, next);
                a.extend_from_slice(&[0x48, 0x89, 0xc7, 0x48, 0xff, 0xce]); // function->rdi; dec rsi
                if slice {
                    a.extend_from_slice(&[0x48, 0x83, 0xc2, 8]); // skip designator in slice
                } else {
                    a.extend_from_slice(&[0x48, 0x89, 0xca, 0x4c, 0x89, 0xc1]); // rcx->rdx; r8->rcx
                }
                a.extend_from_slice(&[0x41, 0xff, 0xe3]); // jmp r11: native callback entry
                a.bind(next);
            }
            a.bind(slow);
            immediate(&mut a, 0, fallback as u64);
            a.extend_from_slice(&[0xff, 0xe0]);
            cache
                .dispatch
                .push(egcl_rt::jit::JitBuffer::new(&a.finish()?)?);
        }
        Some(cache)
    }

    pub(super) fn entries(&self) -> [usize; 2] {
        std::array::from_fn(|i| self.dispatch[i].as_ptr() as usize)
    }

    pub(super) fn synchronize_revision(&mut self, revision: u64) {
        if self.revision != Some(revision) {
            // An invalidated global FUNCALL cell may be refilled by any of its
            // callers. No identity from the previous revision may survive that
            // refill, even if its callable was not the one used on this miss.
            self.clear_callables();
            self.revision = Some(revision);
        }
    }

    pub(super) fn select(&mut self, function: EgclVal) -> bool {
        let previous = self
            .callables
            .iter()
            .position(|slot| slot.function.get() == function.0);
        if let Some(index) = previous {
            self.callables[index].clear();
        }
        if !egcl_rt::function::is_interpreted_function(function) {
            return false;
        }
        let Some(symbol) = egcl_rt::function::name(function).symbol_index() else {
            return false;
        };
        if is_profile_pinned(symbol) || !registered_function_matches(symbol, function) {
            return false;
        }
        let Some(body) = registry_get(symbol) else {
            return false;
        };
        let owner = CLOSURE_COMPILATION
            .with(|cache| {
                cache
                    .borrow()
                    .get(&(Arc::as_ptr(&body) as usize))
                    .map(|state| state.owner)
            })
            .unwrap_or(symbol);
        let Some(code) = NATIVE_REGISTRY
            .with(|registry| registry.borrow().get(&owner).cloned())
            .filter(|code| code.is_t2 && code.body.as_ref().is_some_and(|b| Arc::ptr_eq(b, &body)))
        else {
            return false;
        };
        let entries = if let Some(index) =
            self.versions.iter().position(|version| version.owns(&code))
        {
            // Moving the Box keeps the addresses embedded in native code stable.
            let version = self.versions.remove(index);
            let entries = version.entries();
            self.versions.push(version);
            entries
        } else {
            let Some(fallback) = fallback_entries() else {
                return false;
            };
            let Some(version) = NativeEntry::new_dynamic(symbol, function, code, fallback) else {
                return false;
            };
            let entries = version.entries();
            self.versions.push(version);
            entries
        };
        let index = previous.unwrap_or_else(|| {
            let index = self.next_callable;
            self.next_callable = (index + 1) % self.callables.len();
            index
        });
        let callable = &self.callables[index];
        callable.function.set(function.0);
        for (target, entry) in callable.target.iter().zip(entries) {
            target.set(entry);
        }
        self.trim();
        true
    }

    pub(super) fn scan(&mut self) {
        // Function objects are pinned, but collectable. Never retain one merely
        // because a call site last used it. Its active frame is its strong root.
        self.clear_callables();
        self.trim();
    }

    fn clear_callables(&mut self) {
        self.callables.iter().for_each(CachedCallable::clear);
        self.next_callable = 0;
    }

    fn trim(&mut self) {
        // A global FUNCALL slot sees multiple callback bodies in normal library
        // code. Keep a small LRU instead of remapping executable adapters on each
        // switch. Active versions remain owned until their native return.
        const MAX_IDLE_ENTRIES: usize = 16;
        let mut idle = self
            .versions
            .iter()
            .filter(|version| !version.is_active())
            .count();
        self.versions.retain(|version| {
            if version.is_active() || idle <= MAX_IDLE_ENTRIES {
                true
            } else {
                idle -= 1;
                let entries = version.entries();
                for callable in &self.callables {
                    if callable.target[0].get() == entries[0] {
                        callable.clear();
                    }
                }
                false
            }
        });
    }
}

pub(super) fn is_builtin(symbol: u32) -> bool {
    egcl_rt::symbols::find_index("FUNCALL") == Some(symbol)
        && !egcl_rt::symbols::symbol_function(symbol)
            .is_some_and(egcl_rt::function::is_interpreted_function)
        && super::super::global_fn("FUNCALL").is_none()
}

fn fallback_entries() -> Option<[usize; 2]> {
    static ENTRIES: std::sync::OnceLock<Option<[egcl_rt::jit::JitBuffer; 2]>> =
        std::sync::OnceLock::new();
    ENTRIES
        .get_or_init(|| {
            let mut buffers = Vec::new();
            for callback in [
                fallback_register as *const () as u64,
                fallback_slice as *const () as u64,
            ] {
                let bytes = egcl_compiler::t2::emit::emit_native_call_bridge(
                    callback,
                    c2i_set_native_sigsegv_recovery as *const () as u64,
                )
                .ok()?;
                buffers.push(egcl_rt::jit::JitBuffer::new(&bytes)?);
            }
            buffers.try_into().ok()
        })
        .as_ref()
        .map(|buffers| std::array::from_fn(|i| buffers[i].as_ptr() as usize))
}
extern "C" fn fallback_register(function: u64, n: u64, a: u64, b: u64, c: u64, _: u64) -> u64 {
    if n > 3 {
        return NIL.0;
    }
    egcl_rt::rooted!(
        args = [
            if n > 0 { EgclVal(a) } else { NIL },
            if n > 1 { EgclVal(b) } else { NIL },
            if n > 2 { EgclVal(c) } else { NIL },
        ]
    );
    fallback(EgclVal(function), &args[..n as usize])
}
extern "C" fn fallback_slice(function: u64, n: u64, args: *const EgclVal, _: u64) -> u64 {
    if n != 0 && args.is_null() {
        return NIL.0;
    }
    let args = if n == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(args, n as usize) }
    };
    fallback(EgclVal(function), args)
}
fn fallback(function: EgclVal, args: &[EgclVal]) -> u64 {
    egcl_rt::rooted!(function = function);
    finish_c2i_call(guard_c2i(|| {
        let env = NATIVE_ENV.with(|slot| slot.get());
        if env.is_null() {
            return Ok(NIL);
        }
        apply_function(*function, args, unsafe { &mut *env })
    }))
}

/// Lives in reserved native-stack storage; helpers never allocate Lisp objects.
/// The linked scanner retains the caller's environment while it is suspended.
pub(super) struct Context {
    previous: *mut Context,
    saved_environment: Option<Arc<SharedCell<EnvFrame>>>,
    saved_blocks: Vec<(String, String)>,
    saved_tags: Vec<(String, String)>,
}
static CONTEXT: egcl_rt::execution_local::ExecutionLocal<Cell<*mut Context>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(std::ptr::null_mut())) };
pub(super) unsafe fn scan_contexts(visit: &mut dyn FnMut(*mut EgclVal)) {
    unsafe {
        CONTEXT.scan(|slot| {
            let mut context = slot.get();
            while let Some(current) = context.as_ref() {
                if let Some(frame) = &current.saved_environment {
                    super::super::visit_env_frame_roots(
                        frame,
                        &mut super::super::EnvRootVisitState::default(),
                        visit,
                    );
                }
                context = current.previous;
            }
        });
    }
}
pub(super) extern "C" fn enter_context(context: *mut Context, function: u64) {
    let symbol = egcl_rt::function::name(EgclVal(function)).as_symbol_index();
    let capture = closure_envs().borrow().get(&symbol).cloned();
    let (blocks, tags) = closure_controls()
        .borrow()
        .get(&symbol)
        .cloned()
        .unwrap_or_default();
    let env = NATIVE_ENV.with(|slot| slot.get());
    // The caller has published its frame and retained this code before entry.
    unsafe {
        context.write(Context {
            previous: CONTEXT.with(|slot| slot.get()),
            saved_environment: NATIVE_ENV_FRAME.with(|slot| slot.replace(capture)),
            saved_blocks: std::mem::replace(&mut (*env).block_stack, blocks),
            saved_tags: std::mem::replace(&mut (*env).tag_stack, tags),
        });
    }
    CONTEXT.with(|slot| slot.set(context));
}
pub(super) extern "C" fn leave_context(context: *mut Context) {
    let saved = unsafe { context.read() };
    CONTEXT.with(|slot| slot.set(saved.previous));
    NATIVE_ENV_FRAME.with(|slot| slot.replace(saved.saved_environment));
    let env = NATIVE_ENV.with(|slot| slot.get());
    unsafe {
        (*env).block_stack = saved.saved_blocks;
        (*env).tag_stack = saved.saved_tags;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternating_callback_bodies_reuse_native_adapters() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        egcl_rt::rooted!(functions = Vec::new());
        let mut codes = Vec::new();
        for value in [17, 23] {
            egcl_rt::rooted!(
                form = super::super::super::vec_to_list(&[EgclVal::from_fixnum(value)])
            );
            let body = Arc::new(
                compile_function("ALTERNATING-CALLBACK", NIL, *form, &env, false, false).unwrap(),
            );
            let function = make_bytecode_closure(&body, None);
            functions.push(function);
            let symbol = egcl_rt::function::name(function).as_symbol_index();
            let input = snapshot_t2_input(symbol, 0).unwrap();
            let generation = input.generation;
            let input = egcl_rt::CrossThreadRoot::new(input);
            let artifact = input.with_gc_stable(compile_t2_artifact).unwrap();
            codes.push(
                install_t2_completion(T2Completion {
                    sym: symbol,
                    generation,
                    artifact: Some(artifact),
                    input,
                })
                .unwrap(),
            );
        }
        let mut cache = Cache::new([0, 0]).unwrap();
        for _ in 0..10 {
            assert!(cache.select(functions[0]));
            assert!(cache.select(functions[1]));
            assert!(
                cache.versions.iter().any(|entry| entry.owns(&codes[0])),
                "switching callbacks must not discard and regenerate the previous adapter"
            );
            assert_eq!(cache.versions.len(), 2);
        }
    }

    #[test]
    fn suspended_context_roots_move_and_restore_in_nested_order() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        egcl_rt::rooted!(form = super::super::super::vec_to_list(&[NIL]));
        let body =
            Arc::new(compile_function("CONTEXT-ROOT", NIL, *form, &env, false, false).unwrap());
        egcl_rt::rooted!(function = make_bytecode_closure(&body, None));
        egcl_rt::gc::full_gc().unwrap();
        // Allocate after pinned callable construction and collection, so this
        // reference really moves rather than sharing a retained nursery region.
        egcl_rt::rooted!(original = arena_cons(EgclVal::from_fixnum(73), NIL));
        let original_bits = original.0;
        let frame = Arc::new(SharedCell::new(EnvFrame {
            vars: Default::default(),
            symbol_vars: Default::default(),
            parent: None,
        }));
        frame
            .borrow_mut()
            .vars
            .insert("VALUE".to_owned(), EgclVal(original_bits));
        let previous_env = NATIVE_ENV.with(|slot| slot.replace(&mut env));
        let previous_frame = NATIVE_ENV_FRAME.with(|slot| slot.replace(Some(Arc::clone(&frame))));
        let mut first = std::mem::MaybeUninit::<Context>::uninit();
        let mut second = std::mem::MaybeUninit::<Context>::uninit();
        enter_context(first.as_mut_ptr(), function.0);
        enter_context(second.as_mut_ptr(), function.0);
        drop(original); // only the suspended Context may retain it during GC
        let collected = egcl_rt::gc::collect_t0_minor();
        leave_context(second.as_mut_ptr());
        let inner_restored = NATIVE_ENV_FRAME.with(|slot| slot.borrow().is_none());
        leave_context(first.as_mut_ptr());
        let outer_restored = NATIVE_ENV_FRAME.with(|slot| {
            slot.borrow()
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &frame))
        });
        NATIVE_ENV_FRAME.with(|slot| slot.replace(previous_frame));
        NATIVE_ENV.with(|slot| slot.set(previous_env));
        collected.unwrap();
        let moved = *frame.borrow().vars.get("VALUE").unwrap();
        assert_ne!(
            moved.0, original_bits,
            "the probe must actually relocate its only saved root"
        );
        assert_eq!(cp(moved).0, EgclVal::from_fixnum(73));
        assert!(inner_restored && outer_restored);
    }
}
