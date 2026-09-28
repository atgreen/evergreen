//! Ordered bytecode control scopes, shared by native-transfer compiler tiers.
//!
//! This describes logical handler ownership, not native landing addresses. A
//! scope inherited at OSR entry still belongs to the interpreter activation;
//! seeing its POP or a lexical exit does not authorize erasing runtime state.
//! Cleanup, condition and restart clusters are rejected until their continuation
//! edges can be represented. Unknown scope state must never become an empty set.

use std::collections::{HashMap, HashSet, VecDeque};
use torcl_rt::bytecode::Instr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ownership {
    Local,
    Inherited,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScopeKind {
    Block {
        id: u32,
        resume_bcp: u32,
        register: bool,
    },
    Tagbody {
        id: u32,
    },
    Catch {
        resume_bcp: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlScope {
    pub push_bcp: u32,
    pub ownership: Ownership,
    pub sp_restore: u16,
    pub kind: ScopeKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeExit {
    pub target_bcp: u32,
    pub sp_restore: u16,
    /// Innermost first. GO retains its target TAGBODY; RETURN-FROM removes BLOCK.
    pub removed: Vec<ControlScope>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScopeError {
    BadTarget { bcp: u32 },
    EmptyPop { bcp: u32 },
    InactiveTarget { bcp: u32 },
    InconsistentJoin { bcp: u32 },
    DuplicateIdentity { bcp: u32 },
    UnsupportedScope { bcp: u32 },
}

#[derive(Clone, Debug)]
pub struct ScopeMap {
    before: Vec<Option<Vec<ControlScope>>>,
    exits: HashMap<u32, ScopeExit>,
}

impl ScopeMap {
    pub fn before(&self, bcp: u32) -> Option<&[ControlScope]> {
        self.before.get(bcp as usize)?.as_deref()
    }

    pub fn exit_at(&self, bcp: u32) -> Option<&ScopeExit> {
        self.exits.get(&bcp)
    }

    pub fn analyze(code: &[Instr]) -> Result<Self, ScopeError> {
        Self::from_entry(code, 0, Vec::new())
    }

    /// Seed the alternate entry with the normal-entry scope state, marking those
    /// records inherited. A scope pushed after entry is local, even when nested
    /// inside inherited scopes. Ownership disagreement at a join is a refusal.
    pub fn analyze_osr(code: &[Instr], entry: u32) -> Result<Self, ScopeError> {
        let normal = Self::analyze(code)?;
        let inherited = normal
            .before(entry)
            .ok_or(ScopeError::BadTarget { bcp: entry })?
            .iter()
            .cloned()
            .map(|mut scope| {
                scope.ownership = Ownership::Inherited;
                scope
            })
            .collect();
        Self::from_entry(code, entry, inherited)
    }

    fn from_entry(
        code: &[Instr],
        entry: u32,
        scopes: Vec<ControlScope>,
    ) -> Result<Self, ScopeError> {
        let mut map = Self {
            before: vec![None; code.len()],
            exits: HashMap::new(),
        };
        if code.is_empty() {
            return Ok(map);
        }
        let mut identities = HashSet::new();
        for (pc, instr) in code.iter().enumerate() {
            let id = match instr {
                Instr::PushBlock { block_id, .. } => Some((0, *block_id)),
                Instr::PushTag { tagbody_id, .. } => Some((1, *tagbody_id)),
                _ => None,
            };
            if id.is_some_and(|id| !identities.insert(id)) {
                return Err(ScopeError::DuplicateIdentity { bcp: pc as u32 });
            }
        }
        let mut queue = VecDeque::new();
        // The establishing PUSH is outside an OSR segment. Preserve its cold
        // resume edge explicitly, with only scopes outside the selected target.
        for (index, scope) in scopes.iter().enumerate() {
            if let ScopeKind::Block { resume_bcp, .. } | ScopeKind::Catch { resume_bcp } =
                scope.kind
            {
                map.merge(resume_bcp, scopes[..index].to_vec(), &mut queue)?;
            }
        }
        map.merge(entry, scopes, &mut queue)?;
        while let Some(pc) = queue.pop_front() {
            let mut scopes = map.before[pc as usize].clone().expect("queued scope state");
            let mut fallthrough = true;
            let push = match code[pc as usize] {
                Instr::PushBlock {
                    block_id,
                    resume_bcp,
                    sp_restore,
                    register,
                    ..
                } => {
                    // Escaping calls may select this block even if no local
                    // RETURN-FROM is visible. Its resume is outside the block.
                    map.merge(resume_bcp, scopes.clone(), &mut queue)?;
                    Some((
                        sp_restore,
                        ScopeKind::Block {
                            id: block_id,
                            resume_bcp,
                            register,
                        },
                    ))
                }
                Instr::PushTag {
                    tagbody_id,
                    sp_restore,
                } => Some((sp_restore, ScopeKind::Tagbody { id: tagbody_id })),
                Instr::PushCatch {
                    resume_bcp,
                    sp_restore,
                } => {
                    map.merge(resume_bcp, scopes.clone(), &mut queue)?;
                    Some((sp_restore, ScopeKind::Catch { resume_bcp }))
                }
                Instr::PopHandler => {
                    scopes.pop().ok_or(ScopeError::EmptyPop { bcp: pc })?;
                    None
                }
                Instr::ReturnFrom { block_id } => {
                    let index = scopes
                        .iter()
                        .rposition(
                            |s| matches!(s.kind, ScopeKind::Block { id, .. } if id == block_id),
                        )
                        .ok_or(ScopeError::InactiveTarget { bcp: pc })?;
                    let ScopeKind::Block { resume_bcp, .. } = scopes[index].kind else {
                        unreachable!()
                    };
                    map.exits.insert(
                        pc,
                        ScopeExit {
                            target_bcp: resume_bcp,
                            sp_restore: scopes[index].sp_restore,
                            removed: scopes[index..].iter().rev().cloned().collect(),
                        },
                    );
                    scopes.truncate(index);
                    map.merge(resume_bcp, scopes.clone(), &mut queue)?;
                    fallthrough = false;
                    None
                }
                Instr::Go {
                    tagbody_id,
                    target_bcp,
                } => {
                    let index = scopes
                        .iter()
                        .rposition(
                            |s| matches!(s.kind, ScopeKind::Tagbody { id } if id == tagbody_id),
                        )
                        .ok_or(ScopeError::InactiveTarget { bcp: pc })?;
                    map.exits.insert(
                        pc,
                        ScopeExit {
                            target_bcp,
                            sp_restore: scopes[index].sp_restore,
                            removed: scopes[index + 1..].iter().rev().cloned().collect(),
                        },
                    );
                    scopes.truncate(index + 1);
                    map.merge(target_bcp, scopes.clone(), &mut queue)?;
                    fallthrough = false;
                    None
                }
                Instr::Br(target) | Instr::BrIfFalse(target) | Instr::BrIfTrue(target) => {
                    map.merge(target, scopes.clone(), &mut queue)?;
                    fallthrough = !matches!(code[pc as usize], Instr::Br(_));
                    None
                }
                Instr::Return
                | Instr::Throw
                | Instr::ReturnFromNamed { .. }
                | Instr::GoNamed { .. } => {
                    fallthrough = false;
                    None
                }
                Instr::CallNamed { sym, .. } if is_never_returning_call(sym) => {
                    fallthrough = false;
                    None
                }
                Instr::PushUnwind { .. }
                | Instr::EnterCleanupNormal { .. }
                | Instr::CleanupReturn
                | Instr::PushHandlerCase { .. }
                | Instr::PopHandlerCase
                | Instr::PushHandlerBind { .. }
                | Instr::PopHandlerBind
                | Instr::PushRestartCase { .. }
                | Instr::PopRestartCase => return Err(ScopeError::UnsupportedScope { bcp: pc }),
                _ => None,
            };
            if let Some((sp_restore, kind)) = push {
                scopes.push(ControlScope {
                    push_bcp: pc,
                    ownership: Ownership::Local,
                    sp_restore,
                    kind,
                });
            }
            if fallthrough && (pc as usize + 1) < code.len() {
                map.merge(pc + 1, scopes, &mut queue)?;
            }
        }
        Ok(map)
    }

    fn merge(
        &mut self,
        pc: u32,
        incoming: Vec<ControlScope>,
        queue: &mut VecDeque<u32>,
    ) -> Result<(), ScopeError> {
        let state = self
            .before
            .get_mut(pc as usize)
            .ok_or(ScopeError::BadTarget { bcp: pc })?;
        match state {
            Some(existing) if *existing != incoming => {
                Err(ScopeError::InconsistentJoin { bcp: pc })
            }
            Some(_) => Ok(()),
            None => {
                *state = Some(incoming);
                queue.push_back(pc);
                Ok(())
            }
        }
    }
}

/// ERROR has no normal successor; SIGNAL/CERROR/WARN may return normally.
pub(crate) fn is_never_returning_call(sym: u32) -> bool {
    matches!(
        crate::reader::symbol_name(sym)
            .as_deref()
            .map(|n| n.rsplit(':').next().unwrap_or(n)),
        Some("ERROR")
    )
}
