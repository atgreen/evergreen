// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Private FORMAT argument positions and independently owned logical POP state.
//!
//! All handles must be rooted by their operation owner before a callback or
//! safepoint. Methods snapshot RefCells and release borrows before consulting
//! printer state; no borrow is exported to a formatter/printer callback.

use super::{circle_is_shared, print_length};
use egcl_rt::error::EgclError;
use egcl_rt::gc::TraceHostRoots;
use egcl_rt::object::ConsCell;
use egcl_rt::value::{EgclVal, NIL};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FrameToken(u64);

impl FrameToken {
    pub(super) fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

pub(super) enum FormatFlow {
    Error(EgclError),
    ExitLogicalBlock(LogicalExit),
    ExitColonIteration(FrameToken),
}

impl From<EgclError> for FormatFlow {
    fn from(error: EgclError) -> Self {
        Self::Error(error)
    }
}

impl TraceHostRoots for FormatFlow {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        match self {
            Self::Error(error) => error.trace_host_roots(visit),
            Self::ExitLogicalBlock(exit) => exit.trace_host_roots(visit),
            Self::ExitColonIteration(_) => {}
        }
    }
}

pub(super) type FormatResult<T> = Result<T, FormatFlow>;

/// An exit is a typed internal operation, never a Lisp error/result sentinel.
/// The owning logical block emits the marker after the exit has crossed any
/// intervening temporary case/justification collectors.
pub(super) enum ArgumentStep {
    Value(EgclVal),
    Exit {
        owner: FrameToken,
        marker: ExitMarker,
    },
}

#[derive(Clone)]
pub(super) enum ExitMarker {
    Length,
    Dotted(EgclVal),
}

#[derive(Clone)]
pub(super) struct LogicalExit {
    pub(super) owner: FrameToken,
    pub(super) marker: ExitMarker,
}

impl TraceHostRoots for LogicalExit {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        if let ExitMarker::Dotted(value) = &mut self.marker {
            visit(value);
        }
    }
}

impl TraceHostRoots for ArgumentStep {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        match self {
            Self::Value(value)
            | Self::Exit {
                marker: ExitMarker::Dotted(value),
                ..
            } => visit(value),
            _ => {}
        }
    }
}

/// Keys are rebuilt after relocation, as for the existing CircleTable. A raw
/// address HashSet cannot be kept across a traversal's interrupt/GC poll.
#[derive(Default)]
struct IdentitySet(HashSet<EgclVal>);

impl TraceHostRoots for IdentitySet {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        let previous = std::mem::take(&mut self.0);
        for mut value in previous {
            visit(&mut value);
            self.0.insert(value);
        }
    }
}

#[derive(Default)]
pub(super) struct IterationGuard(IdentitySet);

impl TraceHostRoots for IterationGuard {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.0.trace_host_roots(visit);
    }
}

impl IterationGuard {
    pub(super) fn check(&mut self, arguments: &Arguments) -> Result<(), EgclError> {
        if let Some(key) = arguments.cycle_key()
            && !self.0.0.insert(key)
        {
            return Err(EgclError::ControlError(
                "unbounded FORMAT iteration revisits a circular argument position".into(),
            ));
        }
        Ok(())
    }
}

fn cons_fields(value: EgclVal) -> (EgclVal, EgclVal) {
    debug_assert!(value.is_cons());
    // SAFETY: the caller checked the cons tag and roots its source. No poll,
    // Lisp allocation or callback occurs while reading these two fields.
    unsafe {
        let cell = &*value.as_ptr().cast::<ConsCell>();
        (cell.car, cell.cdr)
    }
}

/// This cursor does not mutate source or POP state. It observes current links
/// and either reaches NIL exactly, or reports a nonfinite/improper count.
fn proper_count(head: EgclVal) -> Result<usize, EgclError> {
    egcl_rt::rooted!(cursor = head);
    egcl_rt::rooted!(visited = IdentitySet::default());
    let mut count = 0usize;
    while cursor.is_cons() {
        if !visited.0.insert(*cursor) {
            return Err(EgclError::ControlError(
                "circular FORMAT argument list has no finite remaining count".into(),
            ));
        }
        count = count.checked_add(1).ok_or_else(|| {
            EgclError::ControlError("FORMAT argument count exceeds host range".into())
        })?;
        *cursor = cons_fields(*cursor).1;
        if count.is_multiple_of(256) {
            egcl_rt::poll_safepoint();
        }
    }
    if !cursor.is_nil() {
        return Err(EgclError::TypeError {
            datum: *cursor,
            expected: "LIST".into(),
        });
    }
    Ok(count)
}

/// An exact positional checkpoint for ~@[. Restoring it does not rewind the
/// separate logical POP state and does not follow possibly mutated head links.
struct PositionCheckpoint {
    current: Option<EgclVal>,
    position: usize,
    exhausted_nonfinite: bool,
}

impl TraceHostRoots for PositionCheckpoint {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.current.trace_host_roots(visit);
    }
}

/// FORMAT's positional source is distinct from logical PPRINT-POP state.
/// List links are reread on consumption and seek; values are not frozen in a
/// flattened cache. Consumed/peeked values remain roots for the active format
/// operation, as they previously did in the flat argument vector.
enum Source {
    Values {
        values: Vec<EgclVal>,
        position: usize,
    },
    List {
        head: EgclVal,
        current: EgclVal,
        position: usize,
        exhausted_nonfinite: bool,
    },
}

struct Position {
    source: Source,
    retained: Vec<EgclVal>,
}

impl TraceHostRoots for Position {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        match &mut self.source {
            Source::Values { values, .. } => values.trace_host_roots(visit),
            Source::List { head, current, .. } => {
                visit(head);
                visit(current);
            }
        }
        self.retained.trace_host_roots(visit);
    }
}

struct LogicalPop {
    remaining: Option<EgclVal>,
    count: usize,
    owner: FrameToken,
}

impl TraceHostRoots for LogicalPop {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        if let Some(remaining) = &mut self.remaining {
            visit(remaining);
        }
    }
}

#[derive(Clone)]
pub(super) struct Arguments {
    position: Rc<RefCell<Position>>,
    logical: Option<Rc<RefCell<LogicalPop>>>,
}

impl TraceHostRoots for Arguments {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.position.borrow_mut().trace_host_roots(visit);
        if let Some(logical) = &self.logical {
            logical.borrow_mut().trace_host_roots(visit);
        }
    }
}

impl Arguments {
    pub(super) fn values(values: &[EgclVal]) -> Self {
        Self {
            position: Rc::new(RefCell::new(Position {
                source: Source::Values {
                    values: values.to_vec(),
                    position: 0,
                },
                retained: Vec::new(),
            })),
            logical: None,
        }
    }

    pub(super) fn list(head: EgclVal) -> Self {
        Self {
            position: Rc::new(RefCell::new(Position {
                source: Source::List {
                    head,
                    current: head,
                    position: 0,
                    exhausted_nonfinite: false,
                },
                retained: Vec::new(),
            })),
            logical: None,
        }
    }

    pub(super) fn logical_list(head: EgclVal, owner: FrameToken) -> Self {
        let mut arguments = Self::list(head);
        arguments.logical = Some(Rc::new(RefCell::new(LogicalPop {
            remaining: Some(head),
            count: 0,
            owner,
        })));
        arguments
    }

    pub(super) fn with_logical_pop(&self, owner: FrameToken) -> Self {
        let remaining = match &self.position.borrow().source {
            Source::List { current, .. } => Some(*current),
            Source::Values { .. } => None,
        };
        Self {
            position: self.position.clone(),
            logical: Some(Rc::new(RefCell::new(LogicalPop {
                remaining,
                count: 0,
                owner,
            }))),
        }
    }

    pub(super) fn current_list(&self) -> Option<EgclVal> {
        match &self.position.borrow().source {
            Source::List { current, .. } => Some(*current),
            Source::Values { .. } => None,
        }
    }

    pub(super) fn has_logical_pop(&self) -> bool {
        self.logical.is_some()
    }

    pub(super) fn cycle_key(&self) -> Option<EgclVal> {
        // A logical POP already guarantees termination through its circle or
        // finite-length check. Otherwise an unlimited iteration must detect a
        // repeated source position itself instead of growing retained roots.
        if self.logical.is_some() && (super::circle_active() || print_length().is_some()) {
            return None;
        }
        match &self.position.borrow().source {
            Source::List { current, .. } if current.is_cons() => Some(*current),
            _ => None,
        }
    }

    /// String ~@? shares its argument continuation, but resets its popper.
    pub(super) fn without_logical_pop(&self) -> Self {
        Self {
            position: self.position.clone(),
            logical: None,
        }
    }

    pub(super) fn position(&self) -> usize {
        match &self.position.borrow().source {
            Source::Values { position, .. } | Source::List { position, .. } => *position,
        }
    }

    pub(super) fn has_remaining(&self) -> bool {
        match &self.position.borrow().source {
            Source::Values { values, position } => *position < values.len(),
            Source::List { current, .. } => !current.is_nil(),
        }
    }

    pub(super) fn peek(&self) -> Option<EgclVal> {
        let mut state = self.position.borrow_mut();
        let value = match &state.source {
            Source::Values { values, position } => values.get(*position).copied(),
            Source::List { current, .. } if current.is_cons() => Some(cons_fields(*current).0),
            Source::List { current, .. } if !current.is_nil() => Some(*current),
            _ => None,
        };
        if let Some(value) = value {
            state.retained.push(value);
        }
        value
    }

    pub(super) fn remaining_count(&self) -> Result<usize, EgclError> {
        let head = {
            let state = self.position.borrow();
            match &state.source {
                Source::Values { values, position } => {
                    return Ok(values.len().saturating_sub(*position));
                }
                Source::List { current, .. } => *current,
            }
        };
        proper_count(head)
    }

    pub(super) fn pop(&self) -> Result<ArgumentStep, EgclError> {
        if !self.has_remaining() {
            return Err(EgclError::ControlError("too few FORMAT arguments".into()));
        }
        if let Some(logical) = &self.logical {
            let (remaining, count, owner) = {
                let state = logical.borrow();
                (state.remaining, state.count, state.owner)
            };
            egcl_rt::rooted!(remaining = remaining);
            if let Some(value) = *remaining {
                if !value.is_nil() && !value.is_cons() {
                    return Ok(ArgumentStep::Exit {
                        owner,
                        marker: ExitMarker::Dotted(value),
                    });
                }
            }
            if print_length().is_some_and(|limit| count >= limit) {
                return Ok(ArgumentStep::Exit {
                    owner,
                    marker: ExitMarker::Length,
                });
            }
            if let Some(value) = *remaining
                && count > 0
                && circle_is_shared(value)
            {
                return Ok(ArgumentStep::Exit {
                    owner,
                    marker: ExitMarker::Dotted(value),
                });
            }
            let next = (*remaining).map(|value| {
                if value.is_cons() {
                    cons_fields(value).1
                } else {
                    NIL
                }
            });
            let mut state = logical.borrow_mut();
            state.remaining = next;
            state.count = state.count.saturating_add(1);
        }
        let mut state = self.position.borrow_mut();
        let value = match &mut state.source {
            Source::Values { values, position } => {
                let value = values[*position];
                *position += 1;
                value
            }
            Source::List {
                current, position, ..
            } => {
                if !current.is_cons() {
                    return Err(EgclError::TypeError {
                        datum: *current,
                        expected: "LIST".into(),
                    });
                }
                let (value, next) = cons_fields(*current);
                *current = next;
                *position += 1;
                value
            }
        };
        state.retained.push(value);
        Ok(ArgumentStep::Value(value))
    }

    /// A ~@[ truth test consumes a logical POP even when its true clause uses
    /// the same positional argument again. Only that position is restored.
    pub(super) fn pop_conditional_test(&self) -> Result<ArgumentStep, EgclError> {
        let checkpoint = {
            let state = self.position.borrow();
            match &state.source {
                Source::Values { position, .. } => PositionCheckpoint {
                    current: None,
                    position: *position,
                    exhausted_nonfinite: false,
                },
                Source::List {
                    current,
                    position,
                    exhausted_nonfinite,
                    ..
                } => PositionCheckpoint {
                    current: Some(*current),
                    position: *position,
                    exhausted_nonfinite: *exhausted_nonfinite,
                },
            }
        };
        egcl_rt::rooted!(checkpoint = checkpoint);
        let step = self.pop()?;
        if matches!(&step, ArgumentStep::Value(value) if !value.is_nil()) {
            let mut state = self.position.borrow_mut();
            match &mut state.source {
                Source::Values { position, .. } => *position = checkpoint.position,
                Source::List {
                    current,
                    position,
                    exhausted_nonfinite,
                    ..
                } => {
                    *current = checkpoint.current.expect("list checkpoint");
                    *position = checkpoint.position;
                    *exhausted_nonfinite = checkpoint.exhausted_nonfinite;
                }
            }
        }
        // Restoring this Rust state allocates nothing and invokes no callback;
        // the returned value is also retained by the caller-rooted source.
        Ok(step)
    }

    /// Snapshot finite APPLY arguments without consuming a logical POP. Actual
    /// FORMATTER consumption still uses the forwarded original continuation.
    /// APPLY requires a proper argument list; a circular rest list is rejected
    /// deliberately rather than flattened into an invented finite argument set.
    pub(super) fn remaining_values(&self) -> Result<Vec<EgclVal>, EgclError> {
        let head = {
            let state = self.position.borrow();
            match &state.source {
                Source::Values { values, position } => return Ok(values[*position..].to_vec()),
                Source::List { current, .. } => *current,
            }
        };
        egcl_rt::rooted!(cursor = head);
        egcl_rt::rooted!(visited = IdentitySet::default());
        egcl_rt::rooted!(values = Vec::<EgclVal>::new());
        while cursor.is_cons() {
            if !visited.0.insert(*cursor) {
                return Err(EgclError::ControlError(
                    "FORMAT function control requires a finite proper argument list".into(),
                ));
            }
            let (value, next) = cons_fields(*cursor);
            values.push(value);
            *cursor = next;
            if values.len().is_multiple_of(256) {
                egcl_rt::poll_safepoint();
            }
        }
        if !cursor.is_nil() {
            return Err(EgclError::TypeError {
                datum: *cursor,
                expected: "LIST".into(),
            });
        }
        Ok(std::mem::take(&mut *values))
    }

    pub(super) fn remaining_tail(&self) -> EgclVal {
        {
            let state = self.position.borrow();
            if let Source::List { current, .. } = &state.source {
                return *current;
            }
        }
        // Values sources are finite. Release the source borrow and root the
        // copied values BEFORE building a Lisp result list.
        let values = {
            let state = self.position.borrow();
            match &state.source {
                Source::Values { values, position } => values[*position..].to_vec(),
                _ => unreachable!(),
            }
        };
        egcl_rt::rooted!(values = values);
        crate::sequences::build_list(&values)
    }

    /// Reusing an earlier argument (for example ~:P) does not itself perform
    /// another logical POP. Reread the current CAR through original links.
    pub(super) fn value_at(&self, requested: usize) -> Result<Option<EgclVal>, EgclError> {
        let head = {
            let state = self.position.borrow();
            match &state.source {
                Source::Values { values, .. } => return Ok(values.get(requested).copied()),
                Source::List { head, .. } => *head,
            }
        };
        egcl_rt::rooted!(cursor = head);
        for index in 0..requested {
            if !cursor.is_cons() {
                if cursor.is_nil() {
                    return Ok(None);
                }
                return Err(EgclError::TypeError {
                    datum: *cursor,
                    expected: "LIST".into(),
                });
            }
            *cursor = cons_fields(*cursor).1;
            if index.is_multiple_of(256) {
                egcl_rt::poll_safepoint();
            }
        }
        if !cursor.is_cons() {
            if cursor.is_nil() {
                return Ok(None);
            }
            return Err(EgclError::TypeError {
                datum: *cursor,
                expected: "LIST".into(),
            });
        }
        let value = cons_fields(*cursor).0;
        self.position.borrow_mut().retained.push(value);
        Ok(Some(value))
    }

    /// Opaque function controls report their unconsumed tail. Position changes
    /// resulting from that contract must not impersonate logical POPs inside
    /// the opaque function, or double-advance a forwarded FORMATTER cursor.
    pub(super) fn advance_without_pop(&self, amount: usize) -> Result<(), EgclError> {
        let current = {
            let mut state = self.position.borrow_mut();
            match &mut state.source {
                Source::Values { values, position } => {
                    *position = position.saturating_add(amount).min(values.len());
                    return Ok(());
                }
                Source::List { current, .. } => *current,
            }
        };
        egcl_rt::rooted!(cursor = current);
        let mut advanced = 0;
        while advanced < amount && cursor.is_cons() {
            *cursor = cons_fields(*cursor).1;
            advanced += 1;
            if advanced.is_multiple_of(256) {
                egcl_rt::poll_safepoint();
            }
        }
        if advanced < amount && !cursor.is_nil() {
            return Err(EgclError::TypeError {
                datum: *cursor,
                expected: "LIST".into(),
            });
        }
        if let Source::List {
            current, position, ..
        } = &mut self.position.borrow_mut().source
        {
            *current = *cursor;
            *position = position.saturating_add(advanced);
        }
        Ok(())
    }

    /// Opening-at-sign logical blocks consume the remaining continuation even
    /// when their body exits before using every value. A nonfinite/improper
    /// data spine can still finish via logical POP; record that its exhausted
    /// absolute ordinal has no finite answer instead of inventing one.
    pub(super) fn exhaust_remaining(&self) {
        let finite_remaining = self.remaining_count().ok();
        let mut state = self.position.borrow_mut();
        match &mut state.source {
            Source::Values { values, position } => *position = values.len(),
            Source::List {
                current,
                position,
                exhausted_nonfinite,
                ..
            } => {
                *current = NIL;
                if let Some(count) = finite_remaining {
                    *position = position.saturating_add(count);
                } else {
                    *exhausted_nonfinite = true;
                }
            }
        }
    }

    pub(super) fn rewind(&self, amount: usize) -> Result<(), EgclError> {
        if matches!(
            &self.position.borrow().source,
            Source::List {
                exhausted_nonfinite: true,
                ..
            }
        ) {
            return Err(EgclError::ControlError(
                "cannot rewind from an exhausted nonfinite FORMAT argument ordinal".into(),
            ));
        }
        self.seek(self.position().saturating_sub(amount))
    }

    /// Reposition the FORMAT argument continuation without rewinding POP count.
    /// Read current links from the original head; a callback may have mutated
    /// links since a previous visit, so old checkpoints cannot answer this.
    pub(super) fn seek(&self, requested: usize) -> Result<(), EgclError> {
        let head = {
            let mut state = self.position.borrow_mut();
            match &mut state.source {
                Source::Values { values, position } => {
                    *position = requested.min(values.len());
                    return Ok(());
                }
                Source::List { head, .. } => *head,
            }
        };
        egcl_rt::rooted!(cursor = head);
        let mut position = 0;
        while position < requested && cursor.is_cons() {
            *cursor = cons_fields(*cursor).1;
            position += 1;
            if position.is_multiple_of(256) {
                egcl_rt::poll_safepoint();
            }
        }
        if position < requested && !cursor.is_nil() {
            return Err(EgclError::TypeError {
                datum: *cursor,
                expected: "LIST".into(),
            });
        }
        if let Source::List {
            current,
            position: actual,
            exhausted_nonfinite,
            ..
        } = &mut self.position.borrow_mut().source
        {
            *current = *cursor;
            *actual = position;
            *exhausted_nonfinite = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ineligible_colon_exit_does_not_consume_its_v_parameter() {
        let _operation = super::super::FormatOperationScope::enter();
        egcl_rt::rooted!(arguments = Arguments::values(&[EgclVal::from_fixnum(0)]));
        let mut output = super::super::LayoutDoc::default();
        let result = super::super::format_arguments("~v:^", &arguments, &mut output);
        assert!(matches!(
            result,
            Err(FormatFlow::Error(EgclError::ControlError(_)))
        ));
        assert_eq!(
            arguments.position(),
            0,
            "eligibility precedes V extraction/POP"
        );
    }

    #[test]
    fn argument_sources_retained_values_and_identity_sets_survive_relocation() {
        const CHILD: &str = "EGCL_FORMAT_ARGUMENT_RELOCATION_CHILD";
        const TEST: &str = "format::arguments::tests::argument_sources_retained_values_and_identity_sets_survive_relocation";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(CHILD, "1")
                .env("EGCL_GC_POISON", "1")
                .env_remove("EGCL_GC_STRESS")
                .env_remove("EGCL_GC_STRESS_AT")
                .env_remove("EGCL_GC_STRESS_SKIP")
                .env_remove("EGCL_GC_STRESS_AFTER_INIT")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        for name in ["*PRINT-LENGTH*", "*PRINT-READABLY*", "*PRINT-CIRCLE*"] {
            let index = egcl_rt::symbols::intern(name);
            egcl_rt::symbols::set_symbol_value(index, NIL);
        }
        egcl_rt::collect_t0_minor().unwrap();
        // Only the argument source owns these young Lisp values once this
        // initializer returns; no extra source/head/value root masks a gap.
        let arguments = {
            egcl_rt::rooted!(first = crate::streams::make_lisp_string_fresh("first"));
            egcl_rt::rooted!(
                head = crate::sequences::build_list(&[
                    *first,
                    EgclVal::from_fixnum(7),
                    EgclVal::from_fixnum(9),
                ])
            );
            Arguments::list(*head)
        };
        egcl_rt::rooted!(arguments = arguments);
        let old_head = arguments.current_list().unwrap();
        let old_first = match arguments.pop().unwrap() {
            ArgumentStep::Value(value) => value,
            _ => panic!("ordinary argument POP must return a value"),
        };
        let old_next = arguments.current_list().unwrap();
        // A callback may remove the already-consumed value from its source.
        // The source's retained-value roots must still preserve it.
        unsafe {
            (*old_head.as_ptr().cast::<ConsCell>()).car = NIL;
        }
        egcl_rt::rooted!(seen = IdentitySet::default());
        seen.0.insert(old_next);
        egcl_rt::collect_t0_minor().unwrap();
        let next = arguments.current_list().unwrap();
        assert_ne!(next, old_next, "unconsumed source must physically move");
        assert!(
            seen.0.contains(&next),
            "identity keys must rehash after moving"
        );
        let (head, first) = {
            let position = arguments.position.borrow();
            let Source::List { head, .. } = &position.source else {
                unreachable!()
            };
            (*head, position.retained[0])
        };
        assert_ne!(head, old_head, "original seek head must physically move");
        assert_ne!(
            first, old_first,
            "consumed retained value must physically move"
        );
        assert_eq!(first.as_string(), "first");
        assert_eq!(arguments.remaining_count().unwrap(), 2);
        assert_eq!(arguments.position(), 1, "counting must not advance POP");
        arguments.seek(0).unwrap();
        match arguments.pop().unwrap() {
            ArgumentStep::Value(value) => assert!(value.is_nil()),
            _ => panic!("rewind must reread the mutated CAR"),
        }
        match arguments.pop().unwrap() {
            ArgumentStep::Value(value) => assert_eq!(value.as_fixnum(), 7),
            _ => panic!("next POP must use the relocated original remainder"),
        }
    }
}
