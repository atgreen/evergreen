// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Generic-function calls from compiled call cells (bliss-gg3i1.7.5).
//!
//! A generic function has no function object, so its call cell never warmed:
//! every compiled call re-entered the cold path, resolved the callee by name,
//! re-checked the arguments against the lambda list and only then consulted
//! the effective-method cache. A cell whose symbol names a generic function now
//! publishes a generic target (`call_table::Target::Generic`) that calls
//! [`generic_call`].
//!
//! [`generic_call`] keeps a PCL-style cache per generic: a few entries keyed on
//! what decides applicability -- each specialized argument's class identity and
//! built-in type bits, or its own identity when it is an immediate EQL
//! specializer value (see `gf_dispatch_key`) -- and holding the cached
//! effective method. A hit runs that effective method directly; the single
//! compiled primary method shape calls its native body. Everything else, and
//! every miss, takes the full call through `apply_function`, which also fills
//! the shared effective-method cache that a later miss copies its entry from.
//!
//! Soundness. An entry is copied only from a decision the full call made at
//! the same dispatch generation, after that same argument count passed the
//! generic's arity and accessor checks, so a hit needs neither check again.
//! Keyword arguments depend on values and are still checked when the
//! generic's lambda list has &KEY. Every entry is stamped with the dispatch
//! generation (bumped by DEFMETHOD, ADD-METHOD, REMOVE-METHOD, DEFGENERIC,
//! REINITIALIZE-INSTANCE of a generic, a lambda-list change, and class
//! definition or redefinition) and with the direct-call generation (bumped
//! when a function binding changes), and is used only while the name still
//! dispatches straight to its generic function (no function, SETF writer or
//! global definition in the way). CHANGE-CLASS gives an instance a different
//! class, and so a different key.
//!
//! GC safety. Keys hold only immediates (class identities are name symbols or
//! fixnums; EQL keys are immediate values), the effective method holds method
//! ids, which are immediate meta handles, and the receiver profile is a leaked
//! static. Nothing here is a movable heap reference, so the cache needs no
//! tracing. No cache borrow is held across a call that can allocate.

use super::*;

/// Entries kept per generic before the oldest is replaced. A megamorphic
/// site still hits the shared effective-method cache on a miss here.
const SITE_ENTRIES: usize = 8;

struct SiteEntry {
    key: GfKey,
    effective: Rc<GfEffective>,
}

/// Everything about one generic that its calls share for one generation.
struct Site {
    gf_generation: u64,
    call_generation: u64,
    /// Whether calling the name reaches its generic function's dispatch.
    /// When false every call takes the full path.
    direct: bool,
    /// The generic's dispatch identity, as `invoke_generic_function` finds it
    /// from the name: the `gf` of every dispatch-cache key.
    gf: u32,
    name: Rc<str>,
    meta: GfMeta,
    profile: Option<&'static bytecode::ReceiverTypeProfile>,
    entries: Vec<SiteEntry>,
    next: usize,
}

static SITES: egcl_rt::execution_local::ExecutionLocal<
    RefCell<HashMap<u32, Site, egcl_rt::fxhash::FxBuildHasher>>,
> = unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(HashMap::default())) };

/// Whether `symbol` names a generic function its compiled call cells may call
/// through [`generic_call`]. Only a hint: [`generic_call`] revalidates.
pub(super) fn names_dispatchable_generic(symbol: u32) -> bool {
    if installed_function(symbol).is_some() || egcl_rt::symbols::is_uninterned(symbol) {
        return false;
    }
    let name = sym_name_rc(EgclVal::from_symbol_index(symbol));
    generic_binding::current(&name) && setf_writer_replacement(&name).is_none()
}

/// A hit's cached work. Cloned out of the cache before anything runs.
struct Hit {
    gf: u32,
    effective: Rc<GfEffective>,
    name: Rc<str>,
    precedence: Rc<[usize]>,
    keyword_check: bool,
    profile: Option<&'static bytecode::ReceiverTypeProfile>,
}

#[inline]
fn lookup(symbol: u32, args: &[EgclVal], gf_generation: u64, call_generation: u64) -> Option<Hit> {
    SITES.with(|sites| {
        let sites = sites.borrow();
        let site = sites.get(&symbol)?;
        if !site.direct
            || site.gf_generation != gf_generation
            || site.call_generation != call_generation
        {
            return None;
        }
        let key = gf_dispatch_key(site.gf, args, &site.meta)?;
        let entry = site.entries.iter().find(|entry| entry.key == key)?;
        Some(Hit {
            gf: site.gf,
            effective: Rc::clone(&entry.effective),
            name: Rc::clone(&site.name),
            precedence: Rc::clone(&site.meta.precedence),
            keyword_check: site.meta.keyword_check,
            profile: site.profile,
        })
    })
}

/// Call the generic function named by `symbol` on `args` from a compiled call
/// cell (see the module comment). `args` are rooted by the caller.
pub(super) fn generic_call(
    symbol: u32,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    let gf_generation = GF_DISPATCH_GENERATION.load(AtomicOrdering::Acquire);
    let call_generation = bytecode::direct_call_gen();
    if let Some(hit) = lookup(symbol, args, gf_generation, call_generation) {
        let _depth = CallDepthGuard::enter()?;
        if let (Some(profile), Some(receiver)) = (hit.profile, args.first())
            && !bytecode::profiling_disabled()
        {
            profile.record(egcl_stdlib::class_of(*receiver).0);
        }
        // An accessor fill is requested only for a cell with an accessor
        // target, never for this one; a stale request belongs to nobody.
        ACCESSOR_FILL_REQUEST.with(|request| request.set(None));
        let result = (|| {
            if hit.keyword_check {
                check_generic_call_arguments(
                    env,
                    Some(hit.gf),
                    &hit.name,
                    args,
                    Some(&mut hit.effective.all_ids()),
                )?;
            }
            match run_cached_effective(
                env,
                &hit.name,
                args,
                &hit.effective,
                &hit.precedence,
                gf_generation,
                false,
            ) {
                Some(result) => result,
                None => apply_function(EgclVal::from_symbol_index(symbol), args, env),
            }
        })();
        if let Err(error) = &result {
            calltrace_note(&hit.name, error);
        }
        return result;
    }
    miss(symbol, args, env, gf_generation, call_generation)
}

#[cold]
fn miss(
    symbol: u32,
    args: &[EgclVal],
    env: &mut Env,
    gf_generation: u64,
    call_generation: u64,
) -> Result<EgclVal, EgclError> {
    let symbol_val = EgclVal::from_symbol_index(symbol);
    let current = SITES.with(|sites| {
        sites.borrow().get(&symbol).is_some_and(|site| {
            site.gf_generation == gf_generation && site.call_generation == call_generation
        })
    });
    if !current {
        refresh_site(symbol, env, gf_generation, call_generation);
    }
    let result = apply_function(symbol_val, args, env);
    if result.is_ok() {
        record(symbol, args, gf_generation, call_generation);
    }
    result
}

/// Rebuild `symbol`'s site for the current generations. Reads only
/// definitions; may allocate (symbol names), but holds no cache borrow then.
fn refresh_site(symbol: u32, env: &Env, gf_generation: u64, call_generation: u64) {
    let name = sym_name_rc(EgclVal::from_symbol_index(symbol));
    // The order `apply_function_impl` resolves a symbol in: only when nothing
    // else is called first does the call reach this generic's dispatch.
    let gf = egcl_rt::symbols::find_index(&name);
    let direct = gf.is_some()
        && installed_function(symbol).is_none()
        && setf_writer_replacement(&name).is_none()
        && named_call_target(env, &name, Some(symbol), true).is_none()
        && generic_binding::current(&name)
        && (env.generics.contains_key(&name) || env.methods.contains_key(&name));
    let meta = gf_meta(env, &name, gf, gf_generation);
    let profile = (direct && !bytecode::profiling_disabled())
        .then(|| bytecode::generic_receiver_profile(&name));
    let site = Site {
        gf_generation,
        call_generation,
        direct,
        gf: gf.unwrap_or(symbol),
        name,
        meta,
        profile,
        entries: Vec::new(),
        next: 0,
    };
    SITES.with(|sites| {
        sites.borrow_mut().insert(symbol, site);
    });
}

/// After a successful full call, copy its effective method from the shared
/// cache into the site, if the decision is still current. Does not allocate
/// Lisp objects.
fn record(symbol: u32, args: &[EgclVal], gf_generation: u64, call_generation: u64) {
    if GF_DISPATCH_GENERATION.load(AtomicOrdering::Acquire) != gf_generation
        || bytecode::direct_call_gen() != call_generation
    {
        return;
    }
    let key = SITES.with(|sites| {
        let sites = sites.borrow();
        let site = sites.get(&symbol)?;
        if !site.direct
            || site.gf_generation != gf_generation
            || site.call_generation != call_generation
        {
            return None;
        }
        gf_dispatch_key(site.gf, args, &site.meta)
    });
    let Some(key) = key else {
        return;
    };
    let Some(effective) = GF_DISPATCH_CACHE.with(|cache| {
        cache
            .borrow()
            .get(&key)
            .filter(|entry| entry.generation == gf_generation)
            .map(|entry| Rc::clone(&entry.methods))
    }) else {
        return;
    };
    SITES.with(|sites| {
        let mut sites = sites.borrow_mut();
        let Some(site) = sites.get_mut(&symbol) else {
            return;
        };
        if site.entries.iter().any(|entry| entry.key == key) {
            return;
        }
        let entry = SiteEntry { key, effective };
        if site.entries.len() < SITE_ENTRIES {
            site.entries.push(entry);
        } else {
            let slot = site.next % SITE_ENTRIES;
            site.entries[slot] = entry;
            site.next = slot + 1;
        }
    });
}
