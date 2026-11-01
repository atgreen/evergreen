// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::control_scope::ScopeMap;
use egcl_compiler::control_scope::{ControlScope, Ownership, ScopeKind};
use egcl_compiler::native_unwind::{NativeUnwindStep, SelectedTarget, next_unwind_step};
use egcl_rt::bytecode::Instr;

#[test]
fn bytecode_scope_maps_keep_inherited_catches_after_local_cleanup() {
    let code = [
        Instr::Const(0),
        Instr::PushCatch {
            resume_bcp: 10,
            sp_restore: 0,
        },
        Instr::PushUnwind {
            cleanup_bcp: 7,
            sp_restore: 0,
        },
        Instr::Const(0),
        Instr::Const(1),
        Instr::Throw,
        Instr::Return,
        Instr::Const(2),
        Instr::Pop,
        Instr::CleanupReturn,
        Instr::Return,
    ];
    let map = ScopeMap::analyze(&code).unwrap();
    let target = SelectedTarget::Scope { push_bcp: 1 };
    assert_eq!(
        next_unwind_step(map.before(5).unwrap(), target),
        NativeUnwindStep::RunCleanup {
            scope_index: 1,
            handler_depth: 1
        }
    );
    assert_eq!(
        next_unwind_step(map.before(9).unwrap(), target),
        NativeUnwindStep::EnterTarget { scope_index: 0 }
    );
    let osr = ScopeMap::analyze_osr(&code, 2).unwrap();
    assert_eq!(
        next_unwind_step(osr.before(5).unwrap(), target),
        NativeUnwindStep::RunCleanup {
            scope_index: 1,
            handler_depth: 1
        }
    );
    assert_eq!(
        next_unwind_step(osr.before(9).unwrap(), target),
        NativeUnwindStep::Fallback
    );
}

fn scope(push_bcp: u32, kind: ScopeKind) -> ControlScope {
    ControlScope {
        push_bcp,
        ownership: Ownership::Local,
        sp_restore: 0,
        kind,
    }
}

#[test]
fn a_selected_catch_only_runs_cleanup_inside_its_boundary() {
    let scopes = [
        scope(1, ScopeKind::Unwind { cleanup_bcp: 90 }),
        scope(2, ScopeKind::Catch { resume_bcp: 80 }),
        scope(3, ScopeKind::Unwind { cleanup_bcp: 70 }),
    ];
    let target = SelectedTarget::Scope { push_bcp: 2 };
    assert_eq!(
        next_unwind_step(&scopes, target),
        NativeUnwindStep::RunCleanup {
            scope_index: 2,
            handler_depth: 2
        }
    );
    assert_eq!(
        next_unwind_step(&scopes[..2], target),
        NativeUnwindStep::EnterTarget { scope_index: 1 }
    );
}

#[test]
fn catch_identity_is_the_establishing_scope_not_its_resume_address() {
    let scopes = [
        scope(1, ScopeKind::Catch { resume_bcp: 80 }),
        scope(2, ScopeKind::Catch { resume_bcp: 80 }),
    ];
    assert_eq!(
        next_unwind_step(&scopes, SelectedTarget::Scope { push_bcp: 2 }),
        NativeUnwindStep::EnterTarget { scope_index: 1 }
    );
    // Crossing the unselected inner catch requires retiring its live registration.
    assert_eq!(
        next_unwind_step(&scopes, SelectedTarget::Scope { push_bcp: 1 }),
        NativeUnwindStep::RetireCatch { scope_index: 1 }
    );
    assert_eq!(
        next_unwind_step(&scopes[..1], SelectedTarget::Scope { push_bcp: 1 }),
        NativeUnwindStep::EnterTarget { scope_index: 0 }
    );
    assert_eq!(
        next_unwind_step(&scopes, SelectedTarget::OutsideFrame),
        NativeUnwindStep::RetireCatch { scope_index: 1 }
    );
}

#[test]
fn local_cleanup_runs_before_falling_back_at_inherited_osr_state() {
    let mut inherited = scope(1, ScopeKind::Catch { resume_bcp: 80 });
    inherited.ownership = Ownership::Inherited;
    let scopes = [inherited, scope(2, ScopeKind::Unwind { cleanup_bcp: 70 })];
    let target = SelectedTarget::Scope { push_bcp: 1 };
    assert_eq!(
        next_unwind_step(&scopes, target),
        NativeUnwindStep::RunCleanup {
            scope_index: 1,
            handler_depth: 1
        }
    );
    assert_eq!(
        next_unwind_step(&scopes[..1], target),
        NativeUnwindStep::Fallback
    );
    assert_eq!(
        next_unwind_step(&scopes[..1], SelectedTarget::OutsideFrame),
        NativeUnwindStep::Fallback
    );
}

#[test]
fn inherited_cleanup_cannot_be_entered_as_a_local_native_cleanup() {
    let mut inherited = scope(1, ScopeKind::Unwind { cleanup_bcp: 70 });
    inherited.ownership = Ownership::Inherited;
    assert_eq!(
        next_unwind_step(&[inherited], SelectedTarget::OutsideFrame),
        NativeUnwindStep::Fallback
    );
}

#[test]
fn crossing_dynamic_state_requires_explicit_restoration() {
    for kind in [
        ScopeKind::LexicalEnvironment,
        ScopeKind::HandlerBind { table_index: 0 },
        ScopeKind::RestartCase {
            table_index: 0,
            resume_bcp: 80,
        },
        ScopeKind::PendingCleanup { cleanup_bcp: 70 },
        // ScopeKind does not say whether NamedTag made this tagbody visible
        // to a closure, so a local ownership bit alone cannot erase it.
        ScopeKind::Tagbody { id: 1 },
    ] {
        let scopes = [
            scope(1, ScopeKind::Unwind { cleanup_bcp: 90 }),
            scope(2, kind),
        ];
        assert_eq!(
            next_unwind_step(&scopes, SelectedTarget::OutsideFrame),
            NativeUnwindStep::Fallback
        );
    }
}

#[test]
fn crossed_registered_block_retires_like_a_catch() {
    // A BLOCK registered for a closure's RETURN-FROM is a runtime record,
    // retired through the catch ledger when a transfer crosses it
    // (bliss-gg3i1.7.11).
    let scopes = [
        scope(1, ScopeKind::Unwind { cleanup_bcp: 90 }),
        scope(
            2,
            ScopeKind::Block {
                id: 1,
                resume_bcp: 80,
                register: true,
            },
        ),
    ];
    assert_eq!(
        next_unwind_step(&scopes, SelectedTarget::OutsideFrame),
        NativeUnwindStep::RetireCatch { scope_index: 1 }
    );
    assert_eq!(
        next_unwind_step(&scopes[..1], SelectedTarget::OutsideFrame),
        NativeUnwindStep::RunCleanup {
            scope_index: 0,
            handler_depth: 0
        }
    );
}

#[test]
fn special_bindings_retire_between_inner_and_outer_cleanup() {
    let mut scopes = [
        scope(1, ScopeKind::Catch { resume_bcp: 90 }),
        scope(2, ScopeKind::Unwind { cleanup_bcp: 80 }),
        scope(3, ScopeKind::SpecialBinding { symbol: 1 }),
        scope(4, ScopeKind::Unwind { cleanup_bcp: 70 }),
    ];
    let target = SelectedTarget::Scope { push_bcp: 1 };
    assert_eq!(
        next_unwind_step(&scopes, target),
        NativeUnwindStep::RunCleanup {
            scope_index: 3,
            handler_depth: 2,
        }
    );
    assert_eq!(
        next_unwind_step(&scopes[..3], target),
        NativeUnwindStep::RetireSpecial { scope_index: 2 }
    );
    assert_eq!(
        next_unwind_step(&scopes[..2], target),
        NativeUnwindStep::RunCleanup {
            scope_index: 1,
            handler_depth: 1,
        }
    );
    scopes[2].ownership = Ownership::Inherited;
    assert_eq!(
        next_unwind_step(&scopes[..3], target),
        NativeUnwindStep::Fallback
    );
}

#[test]
fn repeated_special_bindings_require_individual_retirement() {
    let scopes = [
        scope(1, ScopeKind::SpecialBinding { symbol: 7 }),
        scope(2, ScopeKind::SpecialBinding { symbol: 7 }),
    ];
    for len in (1..=2).rev() {
        assert_eq!(
            next_unwind_step(&scopes[..len], SelectedTarget::OutsideFrame),
            NativeUnwindStep::RetireSpecial {
                scope_index: len - 1
            }
        );
    }
    assert_eq!(
        next_unwind_step(&[], SelectedTarget::OutsideFrame),
        NativeUnwindStep::LeaveFrame
    );
    assert_eq!(
        next_unwind_step(&scopes, SelectedTarget::Scope { push_bcp: 2 }),
        NativeUnwindStep::Fallback
    );
}

#[test]
fn running_cleanup_is_superseded_without_becoming_an_installed_handler() {
    let scopes = [
        scope(
            1,
            ScopeKind::Block {
                id: 1,
                resume_bcp: 90,
                register: false,
            },
        ),
        scope(2, ScopeKind::Cleanup { cleanup_bcp: 80 }),
        scope(3, ScopeKind::Unwind { cleanup_bcp: 70 }),
        scope(4, ScopeKind::Cleanup { cleanup_bcp: 60 }),
    ];
    assert_eq!(
        next_unwind_step(&scopes, SelectedTarget::OutsideFrame),
        NativeUnwindStep::RunCleanup {
            scope_index: 2,
            handler_depth: 1
        }
    );
    assert_eq!(
        next_unwind_step(&scopes[..2], SelectedTarget::OutsideFrame),
        NativeUnwindStep::LeaveFrame
    );
}

#[test]
fn absent_duplicate_or_non_target_scope_is_not_a_native_destination() {
    let catch = scope(1, ScopeKind::Catch { resume_bcp: 80 });
    let target = SelectedTarget::Scope { push_bcp: 1 };
    assert_eq!(next_unwind_step(&[], target), NativeUnwindStep::Fallback);
    assert_eq!(
        next_unwind_step(&[catch.clone(), catch], target),
        NativeUnwindStep::Fallback
    );
    assert_eq!(
        next_unwind_step(&[scope(1, ScopeKind::Unwind { cleanup_bcp: 70 })], target),
        NativeUnwindStep::Fallback
    );
}

#[test]
fn selected_handler_or_restart_is_entered_before_outer_cleanup() {
    for kind in [
        ScopeKind::HandlerCase { table_index: 0 },
        ScopeKind::RestartCase {
            table_index: 0,
            resume_bcp: 80,
        },
        ScopeKind::Block {
            id: 1,
            resume_bcp: 80,
            register: true,
        },
        ScopeKind::Tagbody { id: 1 },
    ] {
        let scopes = [
            scope(1, ScopeKind::Unwind { cleanup_bcp: 90 }),
            scope(2, kind),
        ];
        assert_eq!(
            next_unwind_step(&scopes, SelectedTarget::Scope { push_bcp: 2 }),
            NativeUnwindStep::EnterTarget { scope_index: 1 }
        );
    }
}

#[test]
fn crossed_handler_clusters_retire_before_cleanup_and_selected_clause() {
    let mut scopes = [
        scope(1, ScopeKind::HandlerCase { table_index: 0 }),
        scope(2, ScopeKind::Unwind { cleanup_bcp: 70 }),
        scope(3, ScopeKind::HandlerCase { table_index: 1 }),
    ];
    let target = SelectedTarget::Scope { push_bcp: 1 };
    assert_eq!(
        next_unwind_step(&scopes, target),
        NativeUnwindStep::RetireHandler { scope_index: 2 }
    );
    assert_eq!(
        next_unwind_step(&scopes[..2], target),
        NativeUnwindStep::RunCleanup {
            scope_index: 1,
            handler_depth: 1
        }
    );
    assert_eq!(
        next_unwind_step(&scopes[..1], target),
        NativeUnwindStep::EnterTarget { scope_index: 0 }
    );
    scopes[2].ownership = Ownership::Inherited;
    assert_eq!(
        next_unwind_step(&scopes, target),
        NativeUnwindStep::Fallback
    );
}
