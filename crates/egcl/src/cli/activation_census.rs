// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Optional census of managed native activation preparations and retirements.
//! EGCL_ACTIVATION_CENSUS names the output file; SPROF-START resets the table,
//! and SPROF-STOP writes <path>.window. Counts describe lifecycle events, not
//! CPU time. Counts cover `prepare_reusing` entries and general state recycle
//! attempts, not direct `prepare` (including OSR reservations) or native-child
//! bridge construction and retirement. Shape fields describe code
//! capabilities, not the entry path taken. Keys contain scalar metadata only;
//! no Lisp objects are retained.
//!
//! Capacity and busy-lane refusals are written separately to <path>.refusals
//! and <path>.refusals.window. These count only those two bridge returns, not
//! all root entries or all refusals. Their probe runs only on refusal branches.
//! The first five fields are scalar columns; display names use quoted Rust
//! string syntax so whitespace and control characters stay unambiguous.

//! EGCL_ACTIVATION_CENSUS_ORIGINS=1 adds <path>.origins[.window] without
//! changing the original schema. Rows count preparations only. Their sum
//! should equal the original preparation total. Unknown ingress is explicit;
//! ordinary-entry capability never implies that an activation was avoidable.
//! active_ingresses is a bitset of explicitly entered scopes (Ingress repr).
//! A nearest helper may sit inside a Rust outer or T0 scope; keep those bits.
//! <path>.bridge-leases[.window] counts successful native bridge context
//! construction separately (ParentCached vs UncachedOverflow). Checkout of an
//! initialized context adds no event; storage_reused describes backing reuse
//! only when a context is constructed.
//! These events are NOT added to managed totals, and do not count every call
//! through a reused context or claim that all per-call setup was eliminated.
//! Missing caller identity means unknown, never infer it from CAPTURE. Names
//! stay in the original table; companion keys use exact scalar owner IDs.

#![cfg_attr(
    not(all(target_arch = "x86_64", target_os = "linux")),
    allow(dead_code)
)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Shape {
    pub owner: Option<u32>,
    pub t2: bool,
    pub ordinary: bool,
    pub captured: bool,
    pub self_calls: bool,
    pub osr: bool,
    pub variadic: bool,
}

static ENABLED: AtomicU8 = AtomicU8::new(0);
// Keep both views in one transaction, so concurrent snapshots cannot report
// different preparation totals. No second census lock is added.
#[derive(Default)]
struct Counters {
    counts: HashMap<Shape, [u64; 2]>,
    origins: HashMap<OriginKey, u64>,
    bridge_leases: HashMap<BridgeLeaseKey, u64>,
}
impl Counters {
    fn note(&mut self, shape: Shape, recycle: bool, provenance: Option<Provenance>) {
        self.counts.entry(shape).or_default()[usize::from(recycle)] += 1;
        if let Some(provenance) = provenance {
            *self
                .origins
                .entry(OriginKey { shape, provenance })
                .or_default() += 1;
        }
    }
}
#[derive(Default)]
struct Snapshot {
    counts: Vec<(Shape, [u64; 2])>,
    origins: Vec<(OriginKey, u64)>,
    bridge_leases: Vec<(BridgeLeaseKey, u64)>,
}
static COUNTS: Mutex<Option<Counters>> = Mutex::new(None);

// A separate opt-in keeps the existing census's cost/schema unchanged.
static ORIGINS_ENABLED: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum PrepareSite {
    #[default]
    Unknown,
    NestedChild,
    LeanRoot,
    GeneralRoot,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum Ingress {
    #[default]
    Unknown,
    BytecodeNamed,
    OrdinaryNamed,
    OrdinaryCallable,
    MappedNamed,
    MappedFuncall,
    CheckedCompatibility,
    RustOuter,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Receiver {
    #[default]
    Unknown,
    HeapFunction,
    HeapClosure,
    Cons,
    Symbol,
    Other,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Provenance {
    site: PrepareSite,
    ingress: Ingress,
    // Bits mean explicit active scopes, not a reconstructed call stack.
    active_ingresses: u16,
    caller: Option<u32>,
    receiver: Receiver,
}
static PROVENANCE: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<Provenance>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| std::cell::Cell::new(Provenance::default()))
};
/// Successful context construction, not a managed preparation or a call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum BridgeLeaseKind {
    #[default]
    ParentCached,
    UncachedOverflow,
}
static BRIDGE_KIND: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<BridgeLeaseKind>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| {
        std::cell::Cell::new(BridgeLeaseKind::ParentCached)
    })
};
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct BridgeLeaseKey {
    shape: Shape,
    provenance: Provenance,
    kind: BridgeLeaseKind,
    storage_reused: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct OriginKey {
    shape: Shape,
    provenance: Provenance,
}

pub(crate) fn origins_enabled() -> bool {
    if !enabled() {
        return false;
    }
    match ORIGINS_ENABLED.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = std::env::var_os("EGCL_ACTIVATION_CENSUS_ORIGINS")
                .is_some_and(|value| value == "1");
            ORIGINS_ENABLED.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

/// Scalar execution-owned state. Restore through the current execution after
/// suspension/migration; never retain a carrier-local cache pointer.
pub(crate) struct OriginScope {
    previous: Provenance,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl OriginScope {
    fn replace(next: Provenance) -> Self {
        Self {
            previous: PROVENANCE.with(|slot| slot.replace(next)),
            _not_send: std::marker::PhantomData,
        }
    }
}
impl Drop for OriginScope {
    fn drop(&mut self) {
        PROVENANCE.with(|slot| slot.set(self.previous));
    }
}

/// The callback is never evaluated when disabled: no new execution-local
/// access, locks, receiver inspection or owner resolution on that branch.
pub(crate) fn scope(
    metadata: impl FnOnce() -> (Ingress, Option<u32>, Option<egcl_rt::value::EgclVal>),
) -> Option<OriginScope> {
    scope_when(origins_enabled(), metadata)
}
fn scope_when(
    enabled: bool,
    metadata: impl FnOnce() -> (Ingress, Option<u32>, Option<egcl_rt::value::EgclVal>),
) -> Option<OriginScope> {
    if !enabled {
        return None;
    }
    let (ingress, caller, function) = metadata();
    let receiver = function.map_or(Receiver::Unknown, |function| {
        if egcl_rt::function::is_interpreted_function(function) {
            if egcl_rt::function::is_closure_instance(function) {
                Receiver::HeapClosure
            } else {
                Receiver::HeapFunction
            }
        } else if function.is_cons() {
            Receiver::Cons
        } else if function.is_symbol() {
            Receiver::Symbol
        } else {
            Receiver::Other
        }
    });
    let old = PROVENANCE.with(std::cell::Cell::get);
    Some(OriginScope::replace(Provenance {
        ingress,
        active_ingresses: old.active_ingresses | (1 << ingress as u8),
        caller,
        receiver,
        ..old
    }))
}

pub(crate) fn at_preparation<T>(site: PrepareSite, run: impl FnOnce() -> T) -> T {
    let _scope = origins_enabled().then(|| {
        let old = PROVENANCE.with(std::cell::Cell::get);
        OriginScope::replace(Provenance { site, ..old })
    });
    run()
}

/// Override only while constructing an actual uncached overflow lease.
pub(crate) fn uncached_bridge_lease<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(BridgeLeaseKind);
    impl Drop for Restore {
        fn drop(&mut self) {
            BRIDGE_KIND.with(|slot| slot.set(self.0));
        }
    }
    let _scope = origins_enabled()
        .then(|| Restore(BRIDGE_KIND.with(|slot| slot.replace(BridgeLeaseKind::UncachedOverflow))));
    run()
}

/// Count actual context construction, excluding retained-context checkout.
/// Active lease and body-root acquisition can still occur without this event.
pub(crate) fn bridge_lease(storage_reused: bool, shape: impl FnOnce() -> Shape) {
    if !origins_enabled() {
        return;
    }
    let key = BridgeLeaseKey {
        shape: shape(),
        provenance: PROVENANCE.with(std::cell::Cell::get),
        kind: BRIDGE_KIND.with(std::cell::Cell::get),
        storage_reused,
    };
    if let Ok(mut counts) = COUNTS.lock() {
        *counts
            .get_or_insert_with(Counters::default)
            .bridge_leases
            .entry(key)
            .or_default() += 1;
    }
}

fn render_bridge_leases(mut rows: Vec<(BridgeLeaseKey, u64)>) -> String {
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out = String::from(
        "# leases kind storage_reused site ingress active_ingresses caller_owner_id receiver tier has_ordinary_entry has_env has_self_calls has_osr_entries variadic callee_owner_id\n",
    );
    for (
        BridgeLeaseKey {
            shape,
            provenance: p,
            kind,
            storage_reused,
        },
        count,
    ) in rows
    {
        let id = |value: Option<u32>| value.map_or_else(|| "none".to_owned(), |v| v.to_string());
        out.push_str(&format!(
            "{count} {kind:?} {storage_reused} {:?} {:?} {} {} {:?} {} {} {} {} {} {} {}\n",
            p.site,
            p.ingress,
            p.active_ingresses,
            id(p.caller),
            p.receiver,
            if shape.t2 { "T2" } else { "T1" },
            shape.ordinary,
            shape.captured,
            shape.self_calls,
            shape.osr,
            shape.variadic,
            id(shape.owner)
        ));
    }
    out
}

fn snapshot() -> Snapshot {
    COUNTS
        .lock()
        .ok()
        .and_then(|counts| {
            counts.as_ref().map(|counts| Snapshot {
                counts: counts
                    .counts
                    .iter()
                    .map(|(key, count)| (*key, *count))
                    .collect(),
                origins: counts
                    .origins
                    .iter()
                    .map(|(key, count)| (*key, *count))
                    .collect(),
                bridge_leases: counts
                    .bridge_leases
                    .iter()
                    .map(|(key, count)| (*key, *count))
                    .collect(),
            })
        })
        .unwrap_or_default()
}

fn render_origins(mut rows: Vec<(OriginKey, u64)>) -> String {
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out = String::from(
        "# preparations site ingress active_ingresses caller_owner_id receiver tier has_ordinary_entry has_env has_self_calls has_osr_entries variadic callee_owner_id\n",
    );
    for (
        OriginKey {
            shape,
            provenance: p,
        },
        count,
    ) in rows
    {
        let id = |value: Option<u32>| value.map_or_else(|| "none".to_owned(), |v| v.to_string());
        out.push_str(&format!(
            "{count} {:?} {:?} {} {} {:?} {} {} {} {} {} {} {}\n",
            p.site,
            p.ingress,
            p.active_ingresses,
            id(p.caller),
            p.receiver,
            if shape.t2 { "T2" } else { "T1" },
            shape.ordinary,
            shape.captured,
            shape.self_calls,
            shape.osr,
            shape.variadic,
            id(shape.owner)
        ));
    }
    out
}

#[cfg(test)]
pub(crate) fn migration_scope_for_test() -> OriginScope {
    let old = PROVENANCE.with(std::cell::Cell::get);
    OriginScope::replace(Provenance {
        ingress: Ingress::RustOuter,
        active_ingresses: old.active_ingresses | (1 << Ingress::RustOuter as u8),
        caller: Some(u32::MAX),
        ..old
    })
}
#[cfg(test)]
pub(crate) fn migration_scope_live_for_test() -> bool {
    let current = PROVENANCE.with(std::cell::Cell::get);
    current.active_ingresses & (1 << Ingress::RustOuter as u8) != 0
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    not(all(target_arch = "x86_64", target_os = "linux")),
    allow(dead_code)
)]
pub(crate) enum Refusal {
    Capacity,
    Busy,
}

impl Refusal {
    fn label(self) -> &'static str {
        match self {
            Self::Capacity => "capacity",
            Self::Busy => "busy",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct RefusalKey {
    reason: Refusal,
    parent: Option<u32>,
    callee: Option<u32>,
    t2: bool,
}

static REFUSALS: Mutex<Option<HashMap<RefusalKey, u64>>> = Mutex::new(None);

// No probe on successful entries. Scalar metadata is collected only after
// one of the unchanged capacity/busy guards has decided to decline.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[cold]
pub(crate) fn note_refusal(reason: Refusal, parent: Option<u32>, callee: Option<u32>, t2: bool) {
    if enabled()
        && let Ok(mut counts) = REFUSALS.lock()
    {
        *counts
            .get_or_insert_with(HashMap::new)
            .entry(RefusalKey {
                reason,
                parent,
                callee,
                t2,
            })
            .or_default() += 1;
    }
}

#[inline]
pub(crate) fn enabled() -> bool {
    match ENABLED.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => initialize(),
    }
}

#[cold]
fn initialize() -> bool {
    let enabled = std::env::var_os("EGCL_ACTIVATION_CENSUS").is_some();
    ENABLED.store(if enabled { 2 } else { 1 }, Ordering::Relaxed);
    enabled
}

// The caller checks enabled before collecting the shape.
// Managed native activation entries exist only on this backend.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[cold]
pub(crate) fn note(shape: Shape, recycle: bool) {
    let provenance = (!recycle && origins_enabled()).then(|| PROVENANCE.with(std::cell::Cell::get));
    if let Ok(mut counts) = COUNTS.lock() {
        let counts = counts.get_or_insert_with(Counters::default);
        counts.note(shape, recycle, provenance);
    }
}

pub(crate) fn reset() {
    if enabled() {
        clear(&COUNTS);
        clear(&REFUSALS);
    }
}

fn clear<T>(state: &Mutex<Option<T>>) {
    if let Ok(mut state) = state.lock() {
        *state = None;
    }
}

fn refusal_snapshot(state: &Mutex<Option<HashMap<RefusalKey, u64>>>) -> Vec<(RefusalKey, u64)> {
    state
        .lock()
        .ok()
        .and_then(|state| {
            state
                .as_ref()
                .map(|rows| rows.iter().map(|(key, n)| (*key, *n)).collect())
        })
        .unwrap_or_default()
}

fn render_refusals(
    mut rows: Vec<(RefusalKey, u64)>,
    mut name: impl FnMut(Option<u32>) -> String,
) -> String {
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out = String::from(
        "# count reason callee_tier parent_owner_id callee_owner_id parent_name callee_name\n",
    );
    for (key, count) in rows {
        let id = |owner: Option<u32>| {
            owner
                .map(|id| id.to_string())
                .unwrap_or_else(|| "none".to_owned())
        };
        out.push_str(&format!(
            "{count} {} {} {} {} {:?} {:?}\n",
            key.reason.label(),
            if key.t2 { "T2" } else { "T1" },
            id(key.parent),
            id(key.callee),
            name(key.parent),
            name(key.callee),
        ));
    }
    out
}

fn render(mut rows: Vec<(Shape, [u64; 2])>) -> String {
    // Snapshotting released the lock before resolving names; no name or
    // heap lookup belongs inside the counter critical section.
    rows.sort_by(|a, b| {
        b.1[0]
            .cmp(&a.1[0])
            .then(b.1[1].cmp(&a.1[1]))
            .then(a.0.cmp(&b.0))
    });
    let mut out = String::from(
        "# prepare_reusing recycle_attempts tier has_ordinary_entry has_env has_self_calls has_osr_entries variadic owner_id name\n",
    );
    for (shape, [prepare, recycle]) in rows {
        let name = shape
            .owner
            .map(|owner| {
                super::sym_name_uncached(egcl_rt::value::EgclVal::from_symbol_index(owner))
            })
            .unwrap_or_else(|| "<uninstalled>".to_owned());
        out.push_str(&format!(
            "{prepare} {recycle} {} {} {} {} {} {} {} {name}\n",
            if shape.t2 { "T2" } else { "T1" },
            shape.ordinary,
            shape.captured,
            shape.self_calls,
            shape.osr,
            shape.variadic,
            shape
                .owner
                .map(|owner| owner.to_string())
                .unwrap_or_else(|| "none".to_owned())
        ));
    }
    out
}

fn write(window: bool) {
    if !enabled() {
        return;
    }
    if let Ok(base) = std::env::var("EGCL_ACTIVATION_CENSUS") {
        let (path, refusal_path) = output_paths(&base, window);
        let Snapshot {
            counts,
            origins,
            bridge_leases,
        } = snapshot();
        if origins_enabled() {
            let lease_path = format!(
                "{base}.bridge-leases{}",
                if window { ".window" } else { "" }
            );
            if let Err(error) = std::fs::write(&lease_path, render_bridge_leases(bridge_leases)) {
                eprintln!("egcl: could not write native bridge leases to {lease_path}: {error}");
            }
            let companion = format!("{base}.origins{}", if window { ".window" } else { "" });
            if let Err(error) = std::fs::write(&companion, render_origins(origins)) {
                eprintln!("egcl: could not write activation origins to {companion}: {error}");
            }
        }
        if let Err(error) = std::fs::write(&path, render(counts)) {
            eprintln!("egcl: could not write EGCL_ACTIVATION_CENSUS to {path}: {error}");
        }
        // Snapshot scalars and release the census lock before name lookup.
        let rows = refusal_snapshot(&REFUSALS);
        let output = render_refusals(rows, |owner| {
            owner
                .map(|index| {
                    super::sym_name_uncached(egcl_rt::value::EgclVal::from_symbol_index(index))
                })
                .unwrap_or_else(|| "<uninstalled>".to_owned())
        });
        if let Err(error) = std::fs::write(&refusal_path, output) {
            eprintln!("egcl: could not write activation refusals to {refusal_path}: {error}");
        }
    }
}

fn output_paths(base: &str, window: bool) -> (String, String) {
    let suffix = if window { ".window" } else { "" };
    (
        format!("{base}{suffix}"),
        format!("{base}.refusals{suffix}"),
    )
}

pub(crate) fn write_window() {
    write(true);
}
pub fn maybe_write_on_exit() {
    write(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(reason: Refusal, callee: Option<u32>, t2: bool) -> RefusalKey {
        RefusalKey {
            reason,
            parent: Some(7),
            callee,
            t2,
        }
    }

    #[test]
    fn refusal_schema_keeps_reason_tier_and_exact_owner_ids_separate() {
        let rendered = render_refusals(
            vec![
                (key(Refusal::Capacity, Some(9), false), 1),
                (key(Refusal::Busy, Some(8), true), 3),
                (key(Refusal::Capacity, None, true), 1),
            ],
            |_| "same-name".to_owned(),
        );
        assert_eq!(
            rendered,
            "# count reason callee_tier parent_owner_id callee_owner_id parent_name callee_name\n\
             3 busy T2 7 8 \"same-name\" \"same-name\"\n\
             1 capacity T2 7 none \"same-name\" \"same-name\"\n\
             1 capacity T1 7 9 \"same-name\" \"same-name\"\n"
        );
        assert_eq!(
            render_refusals(Vec::new(), |_| unreachable!()),
            rendered.lines().next().unwrap().to_owned() + "\n"
        );
        let escaped = render_refusals(vec![(key(Refusal::Capacity, Some(9), false), 1)], |owner| {
            if owner == Some(7) {
                "(SETF A)\t".to_owned()
            } else {
                "a\"b\n".to_owned()
            }
        });
        assert_eq!(
            escaped.lines().nth(1),
            Some(r#"1 capacity T1 7 9 "(SETF A)\t" "a\"b\n""#)
        );
    }

    #[test]
    fn refusal_snapshot_releases_lock_before_names_and_reset_preserves_snapshot() {
        let state = Mutex::new(Some(HashMap::from([
            (key(Refusal::Capacity, Some(9), false), 2),
            (key(Refusal::Busy, Some(9), false), 1),
        ])));
        let rows = refusal_snapshot(&state);
        let output = render_refusals(rows.clone(), |owner| {
            assert!(
                state.try_lock().is_ok(),
                "name lookup must not hold census lock"
            );
            owner.unwrap().to_string()
        });
        assert!(output.contains("2 capacity T1 7 9 \"7\" \"9\"\n"));
        clear(&state);
        assert!(refusal_snapshot(&state).is_empty());
        assert!(state.lock().unwrap().is_none());
        assert_eq!(
            render_refusals(rows, |owner| owner.unwrap().to_string()),
            output
        );
    }

    #[test]
    fn companion_paths_preserve_lifecycle_exit_and_window_paths() {
        assert_eq!(
            output_paths("/tmp/counts", false),
            ("/tmp/counts".to_owned(), "/tmp/counts.refusals".to_owned())
        );
        assert_eq!(
            output_paths("/tmp/counts", true),
            (
                "/tmp/counts.window".to_owned(),
                "/tmp/counts.refusals.window".to_owned()
            )
        );
    }
}

/// Parent-cached fresh/reused, then uncached fresh/reused. No Lisp roots.
#[cfg(test)]
pub(crate) fn bridge_lease_counts_for_test(owners: &[u32]) -> [u64; 4] {
    let mut result = [0; 4];
    for (key, count) in snapshot().bridge_leases {
        if key.shape.owner.is_some_and(|owner| owners.contains(&owner)) {
            let base = match key.kind {
                BridgeLeaseKind::ParentCached => 0,
                BridgeLeaseKind::UncachedOverflow => 2,
            };
            result[base + usize::from(key.storage_reused)] += count;
        }
    }
    result
}

#[cfg(test)]
pub(crate) fn scope_snapshot_for_test() -> (Ingress, u16, Option<u32>) {
    let current = PROVENANCE.with(std::cell::Cell::get);
    (current.ingress, current.active_ingresses, current.caller)
}

#[cfg(test)]
mod origin_tests {
    #[test]
    fn bridge_context_schema_does_not_add_to_managed_preparations() {
        let shape = super::Shape {
            owner: Some(42),
            t2: false,
            ordinary: true,
            captured: false,
            self_calls: false,
            osr: false,
            variadic: false,
        };
        let mut counts = super::Counters::default();
        counts.note(shape, false, Some(super::Provenance::default()));
        let key = super::BridgeLeaseKey {
            shape,
            provenance: super::Provenance::default(),
            kind: super::BridgeLeaseKind::UncachedOverflow,
            storage_reused: true,
        };
        counts.bridge_leases.insert(key, 99);
        counts.bridge_leases.insert(
            super::BridgeLeaseKey {
                kind: super::BridgeLeaseKind::ParentCached,
                storage_reused: false,
                ..key
            },
            16,
        );
        assert_eq!(counts.counts.get(&shape), Some(&[1, 0]));
        assert_eq!(counts.origins.values().sum::<u64>(), 1);
        let output = super::render_bridge_leases(counts.bridge_leases.into_iter().collect());
        let rows: Vec<_> = output.lines().skip(1).collect();
        assert_eq!(
            rows[0],
            "99 UncachedOverflow true Unknown Unknown 0 none Unknown T1 true false false false false 42"
        );
        assert!(rows[1].starts_with("16 ParentCached false"));
        assert!(rows.iter().all(|row| row.split_whitespace().count() == 15));
    }

    use super::*;

    #[test]
    fn disabled_origin_scope_never_evaluates_metadata() {
        assert!(scope_when(false, || panic!("disabled metadata callback")).is_none());
    }

    #[test]
    fn explicit_ingress_scopes_restore_after_result_error_and_panic() {
        let before = PROVENANCE.with(std::cell::Cell::get);
        let outer = scope_when(true, || (Ingress::BytecodeNamed, Some(31), None));
        let expected = PROVENANCE.with(std::cell::Cell::get);
        fn checked_fallback() -> Result<(), &'static str> {
            let _inner = scope_when(true, || (Ingress::RustOuter, None, None));
            let current = PROVENANCE.with(std::cell::Cell::get);
            assert_eq!(current.ingress, Ingress::RustOuter);
            assert_ne!(
                current.active_ingresses & (1 << Ingress::BytecodeNamed as u8),
                0
            );
            assert_ne!(
                current.active_ingresses & (1 << Ingress::RustOuter as u8),
                0
            );
            assert_eq!(current.caller, None, "never inherit an unrelated caller ID");
            Err("checked fallback error")
        }
        let result = checked_fallback();
        assert_eq!(result, Err("checked fallback error"));
        assert_eq!(PROVENANCE.with(std::cell::Cell::get), expected);
        let result = std::panic::catch_unwind(|| {
            let _inner = scope_when(true, || (Ingress::MappedFuncall, None, None));
            panic!("guarded observer panic");
        });
        assert!(result.is_err());
        assert_eq!(PROVENANCE.with(std::cell::Cell::get), expected);
        drop(outer);
        assert_eq!(PROVENANCE.with(std::cell::Cell::get), before);
    }

    #[test]
    fn origin_counts_match_preparations_without_counting_retirements() {
        let mut counts = Counters::default();
        let shape = Shape {
            owner: Some(1),
            t2: true,
            ordinary: true,
            captured: false,
            self_calls: false,
            osr: false,
            variadic: false,
        };
        for site in [
            PrepareSite::NestedChild,
            PrepareSite::LeanRoot,
            PrepareSite::GeneralRoot,
        ] {
            counts.note(
                shape,
                false,
                Some(Provenance {
                    site,
                    ..Provenance::default()
                }),
            );
            counts.note(shape, true, None);
        }
        assert_eq!(counts.counts.get(&shape), Some(&[3, 3]));
        assert_eq!(counts.origins.values().sum::<u64>(), 3);
        assert_eq!(counts.origins.len(), 3);
        counts.note(shape, false, None);
        assert_eq!(
            counts.counts.get(&shape),
            Some(&[4, 3]),
            "old counts remain available without companion"
        );
        assert_eq!(counts.origins.values().sum::<u64>(), 3);
    }

    #[test]
    fn origin_companion_schema_retains_exact_sites_ids_and_unknowns() {
        let shape = Shape {
            owner: Some(2147488049),
            t2: true,
            ordinary: true,
            captured: false,
            self_calls: false,
            osr: false,
            variadic: false,
        };
        let text = render_origins(vec![
            (
                OriginKey {
                    shape,
                    provenance: Provenance {
                        site: PrepareSite::LeanRoot,
                        ingress: Ingress::RustOuter,
                        active_ingresses: 130,
                        caller: None,
                        receiver: Receiver::HeapClosure,
                    },
                },
                87345,
            ),
            (
                OriginKey {
                    shape,
                    provenance: Provenance {
                        site: PrepareSite::NestedChild,
                        ..Provenance::default()
                    },
                },
                2,
            ),
            (
                OriginKey {
                    shape,
                    provenance: Provenance {
                        site: PrepareSite::GeneralRoot,
                        ..Provenance::default()
                    },
                },
                3,
            ),
        ]);
        let rows: Vec<_> = text.lines().skip(1).collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            "87345 LeanRoot RustOuter 130 none HeapClosure T2 true false false false false 2147488049"
        );
        assert!(rows[1].starts_with("3 GeneralRoot Unknown 0 none Unknown"));
        assert!(rows[2].starts_with("2 NestedChild Unknown 0 none Unknown"));
        assert!(
            rows.iter()
                .all(|line| line.split_whitespace().count() == 13)
        );
        assert_eq!(
            rows.iter()
                .map(|line| line
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap())
                .sum::<u64>(),
            87350
        );
    }
}
