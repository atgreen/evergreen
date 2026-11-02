// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Explicit unbinding before a local exit (bliss-3k0xm).
//!
//! A `RETURN-FROM` or `GO` to a BLOCK or TAGBODY of the same function that
//! leaves special bindings or captured-variable environments behind made the
//! runtime undo them while unwinding, and native code has no such unwinding
//! for a direct exit: the T2 builder declined the whole function ("direct exit
//! requires dynamic unwinding"). cl-ppcre's scanner returns from a BLOCK
//! around ten special bindings, so it stayed in the interpreter.
//!
//! When everything the exit leaves between it and its target is a BLOCK, a
//! TAGBODY, a special binding or an environment, the lowerer now emits the
//! `UnbindSpecial`/`PopEnvChild` the normal path would run, just before the
//! exit. The exit then crosses only BLOCK/TAGBODY scopes, which every tier
//! handles directly. An exit that crosses anything else -- a handler,
//! UNWIND-PROTECT, CATCH, a running cleanup -- is unchanged, since a cleanup
//! or handler must still see the bindings made outside it.
//!
//! The scope state at the exit comes from replaying the instructions emitted
//! so far. The lowerer emits each construct's entry and exit in order, so a
//! completed construct balances, and code after an exit runs in the state
//! before it; the inserted instructions are skipped for that reason. Anything
//! unexpected abandons the insertion.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Function,
    Block(u32),
    Tagbody(u32),
    Opaque,
}

/// The target of a local exit.
#[derive(Clone, Copy)]
pub(super) enum ExitTarget {
    Block(u32),
    Tagbody(u32),
}

impl Lowerer<'_> {
    /// `(special bindings, environments)` a local exit to `target` emitted
    /// next would leave, when only blocks and tagbodies lie between.
    fn exit_unwind_counts(&self, target: ExitTarget) -> Option<(u32, u32)> {
        // (kind, special bindings, environments) per handler level.
        let mut levels: Vec<(Level, u32, u32)> = vec![(Level::Function, 0, 0)];
        let mut inserted = self.exit_unwind_sites.iter().copied().peekable();
        for (index, instruction) in self.code.iter().enumerate() {
            if inserted.peek() == Some(&index) {
                inserted.next();
                continue;
            }
            let top = levels.last_mut()?;
            match instruction {
                Instr::BindSpecial(_) => top.1 += 1,
                Instr::UnbindSpecial(count) => {
                    top.1 = top.1.checked_sub(u32::from(*count))?;
                }
                Instr::PushEnvChild => top.2 += 1,
                Instr::PopEnvChild => top.2 = top.2.checked_sub(1)?,
                Instr::PushBlock { block_id, .. } => levels.push((Level::Block(*block_id), 0, 0)),
                Instr::PushTag { tagbody_id, .. } => {
                    levels.push((Level::Tagbody(*tagbody_id), 0, 0))
                }
                Instr::PushCatch { .. }
                | Instr::PushUnwind { .. }
                | Instr::PushHandlerCase { .. }
                | Instr::PushHandlerBind { .. }
                | Instr::PushRestartCase { .. }
                | Instr::EnterCleanupNormal { .. } => levels.push((Level::Opaque, 0, 0)),
                Instr::PopHandler
                | Instr::PopHandlerCase
                | Instr::PopHandlerBind
                | Instr::PopRestartCase
                | Instr::CleanupReturn => {
                    let (kind, specials, environments) = levels.pop()?;
                    if kind == Level::Function || specials != 0 || environments != 0 {
                        return None;
                    }
                }
                _ => {}
            }
        }
        let (mut specials, mut environments) = (0u32, 0u32);
        for &(kind, level_specials, level_environments) in levels.iter().rev() {
            specials += level_specials;
            environments += level_environments;
            match (kind, target) {
                (Level::Block(id), ExitTarget::Block(wanted)) if id == wanted => {
                    return Some((specials, environments));
                }
                (Level::Tagbody(id), ExitTarget::Tagbody(wanted)) if id == wanted => {
                    return Some((specials, environments));
                }
                (Level::Block(_) | Level::Tagbody(_), _) => {}
                (Level::Function | Level::Opaque, _) => return None,
            }
        }
        None
    }

    /// Before a local exit to `target`, undo the bindings and environments it
    /// would leave, if only blocks and tagbodies lie between.
    pub(super) fn emit_exit_unwind(&mut self, target: ExitTarget) {
        if std::env::var_os("EGCL_NO_EXIT_UNWIND").is_some() {
            return;
        }
        let Some((specials, environments)) = self.exit_unwind_counts(target) else {
            return;
        };
        let Ok(specials) = u16::try_from(specials) else {
            return;
        };
        if specials > 0 {
            self.exit_unwind_sites.push(self.code.len());
            self.emit(Instr::UnbindSpecial(specials));
        }
        for _ in 0..environments {
            self.exit_unwind_sites.push(self.code.len());
            self.emit(Instr::PopEnvChild);
        }
    }
}
