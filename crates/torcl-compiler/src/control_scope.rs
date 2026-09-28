//! Ordered bytecode control scopes, shared by native-transfer compiler tiers.
//!
//! This describes logical handler ownership, not native landing addresses. A
//! scope inherited at OSR entry still belongs to the interpreter activation;
//! seeing its POP or a lexical exit does not authorize erasing runtime state.
//! Logical scopes include running cleanup continuations, not only installed
//! handlers. Condition and restart clusters are still rejected. Unknown scope
//! state must never become an empty set.

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
    /// Installed UNWIND-PROTECT handler; an exit must execute its cleanup.
    Unwind {
        cleanup_bcp: u32,
    },
    /// Handler popped on the normal path, awaiting EnterCleanupNormal.
    /// Retains ownership through this handoff; it is not a runtime handler.
    PendingCleanup {
        cleanup_bcp: u32,
    },
    /// Cleanup executing with a saved normal value or pending transfer.
    /// An exit crossing this record supersedes that saved continuation.
    Cleanup {
        cleanup_bcp: u32,
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
    /// Ultimate lexical destination, not necessarily the next executed BCP.
    /// Removed Unwind records must run first and can replace the transfer.
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
    InvalidCleanup { bcp: u32 },
}

#[derive(Clone, Debug)]
pub struct ScopeMap {
    before: Vec<Option<Vec<ControlScope>>>,
    exits: HashMap<u32, ScopeExit>,
    normal_resumes: HashMap<u32, Vec<u32>>,
}

impl ScopeMap {
    pub fn before(&self, bcp: u32) -> Option<&[ControlScope]> {
        self.before.get(bcp as usize)?.as_deref()
    }

    pub fn exit_at(&self, bcp: u32) -> Option<&ScopeExit> {
        self.exits.get(&bcp)
    }

    /// Possible normal destinations of a cleanup. An unwind continuation is
    /// selected dynamically and is not a normal successor from this list.
    pub fn cleanup_resumes(&self, cleanup_bcp: u32) -> &[u32] {
        self.normal_resumes
            .get(&cleanup_bcp)
            .map_or(&[], Vec::as_slice)
    }

    pub fn analyze(code: &[Instr]) -> Result<Self, ScopeError> {
        Self::from_entry(code, 0, Vec::new(), HashMap::new())
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
        // A cleanup already running at entry can return to a normal destination
        // registered before entry. Retain those static possibilities; runtime
        // continuation state still chooses normal return versus resumed unwind.
        Self::from_entry(code, entry, inherited, normal.normal_resumes)
    }

    fn from_entry(
        code: &[Instr],
        entry: u32,
        scopes: Vec<ControlScope>,
        normal_resumes: HashMap<u32, Vec<u32>>,
    ) -> Result<Self, ScopeError> {
        let mut map = Self {
            before: vec![None; code.len()],
            exits: HashMap::new(),
            normal_resumes,
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
        let mut cleanup_returns: HashMap<u32, Vec<u32>> = HashMap::new();
        // The establishing PUSH is outside an OSR segment. Preserve its cold
        // resume edge explicitly, with only scopes outside the selected target.
        for (index, scope) in scopes.iter().enumerate() {
            if let ScopeKind::Block { resume_bcp, .. } | ScopeKind::Catch { resume_bcp } =
                scope.kind
            {
                map.merge(resume_bcp, scopes[..index].to_vec(), &mut queue)?;
            } else if let ScopeKind::Unwind { cleanup_bcp } = scope.kind {
                let mut cleanup_scopes = scopes[..index].to_vec();
                let mut cleanup = scope.clone();
                cleanup.kind = ScopeKind::Cleanup { cleanup_bcp };
                cleanup_scopes.push(cleanup);
                map.merge(cleanup_bcp, cleanup_scopes, &mut queue)?;
            }
        }
        map.merge(entry, scopes, &mut queue)?;
        while let Some(pc) = queue.pop_front() {
            let mut scopes = map.before[pc as usize].clone().expect("queued scope state");
            let mut fallthrough = true;
            if matches!(
                scopes.last().map(|s| &s.kind),
                Some(ScopeKind::PendingCleanup { .. })
            ) && !matches!(code[pc as usize], Instr::EnterCleanupNormal { .. })
            {
                return Err(ScopeError::InvalidCleanup { bcp: pc });
            }
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
                Instr::PushUnwind {
                    cleanup_bcp,
                    sp_restore,
                } => {
                    let mut cleanup_scopes = scopes.clone();
                    cleanup_scopes.push(ControlScope {
                        push_bcp: pc,
                        ownership: Ownership::Local,
                        sp_restore,
                        kind: ScopeKind::Cleanup { cleanup_bcp },
                    });
                    // A call or transfer may start unwinding here. The handler
                    // is removed before running its cleanup; the saved transfer
                    // remains live as a distinct logical scope.
                    map.merge(cleanup_bcp, cleanup_scopes, &mut queue)?;
                    Some((sp_restore, ScopeKind::Unwind { cleanup_bcp }))
                }
                Instr::PopHandler => {
                    let scope = scopes.last_mut().ok_or(ScopeError::EmptyPop { bcp: pc })?;
                    match scope.kind {
                        ScopeKind::Unwind { cleanup_bcp } => {
                            scope.kind = ScopeKind::PendingCleanup { cleanup_bcp };
                        }
                        ScopeKind::Cleanup { .. } | ScopeKind::PendingCleanup { .. } => {
                            return Err(ScopeError::InvalidCleanup { bcp: pc });
                        }
                        _ => {
                            scopes.pop();
                        }
                    }
                    None
                }
                Instr::EnterCleanupNormal {
                    cleanup_bcp,
                    resume_bcp,
                } => {
                    let scope = scopes
                        .last_mut()
                        .ok_or(ScopeError::InvalidCleanup { bcp: pc })?;
                    if scope.kind != (ScopeKind::PendingCleanup { cleanup_bcp }) {
                        return Err(ScopeError::InvalidCleanup { bcp: pc });
                    }
                    scope.kind = ScopeKind::Cleanup { cleanup_bcp };
                    if resume_bcp as usize >= code.len() {
                        return Err(ScopeError::BadTarget { bcp: resume_bcp });
                    }
                    let resumes = map.normal_resumes.entry(cleanup_bcp).or_default();
                    if !resumes.contains(&resume_bcp) {
                        resumes.push(resume_bcp);
                        // CleanupReturn may have been visited first via the
                        // exceptional edge. Revisit it when a new normal
                        // continuation becomes reachable.
                        queue.extend(
                            cleanup_returns
                                .get(&cleanup_bcp)
                                .into_iter()
                                .flatten()
                                .copied(),
                        );
                    }
                    map.merge(cleanup_bcp, scopes.clone(), &mut queue)?;
                    fallthrough = false;
                    None
                }
                Instr::CleanupReturn => {
                    let scope = scopes.pop().ok_or(ScopeError::InvalidCleanup { bcp: pc })?;
                    let ScopeKind::Cleanup { cleanup_bcp } = scope.kind else {
                        return Err(ScopeError::InvalidCleanup { bcp: pc });
                    };
                    let returns = cleanup_returns.entry(cleanup_bcp).or_default();
                    if !returns.contains(&pc) {
                        returns.push(pc);
                    }
                    for resume in map.cleanup_resumes(cleanup_bcp).to_vec() {
                        map.merge(resume, scopes.clone(), &mut queue)?;
                    }
                    // An exceptional continuation resumes the dynamic unwind;
                    // its outer targets were seeded when they were established.
                    fallthrough = false;
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
                Instr::PushHandlerCase { .. }
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
