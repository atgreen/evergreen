// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Opt-in layout relaxation of explicitly recorded x86 label branches.
//! Raw instruction bytes are copied unchanged, never decoded or guessed.

use super::{Asm, FixupKind};

/// Translation from the original emission layout to the final layout.
/// Unchanged bytes and code-end are mapped. A shortened branch maps only its
/// start and end; every old interior byte is rejected rather than ambiguously
/// authorizing a metadata position in a rewritten instruction.
#[derive(Clone, Debug)]
pub struct OffsetMap {
    offsets: Vec<Option<usize>>,
}

impl OffsetMap {
    pub fn translate(&self, old: usize) -> Option<usize> {
        self.offsets.get(old).copied().flatten()
    }

    fn new(length: usize, branches: &[(usize, usize)], short: &[bool]) -> Option<Self> {
        let mut offsets = vec![None; length.checked_add(1)?];
        let mut old = 0;
        let mut new = 0;
        for (&(start, size), &selected) in branches.iter().zip(short) {
            if !selected {
                continue;
            }
            let end = start.checked_add(size)?;
            if start < old || end > length {
                return None;
            }
            while old <= start {
                offsets[old] = Some(new);
                old += 1;
                new += 1;
            }
            // The loop already mapped START and advanced past it.
            new += 1;
            old = end;
        }
        while old <= length {
            offsets[old] = Some(new);
            old += 1;
            new += 1;
        }
        Some(Self { offsets })
    }
}

impl Asm {
    /// Resolve this x86 buffer with fixed-point near-to-short JMP/Jcc layout.
    /// CALL and RIP-relative LEA retain their encodings, but every recorded
    /// displacement is repatched against translated locations. Clients must
    /// translate all externally recorded offsets with the returned map.
    ///
    /// This cannot repair raw hand-coded PC-relative instructions or alignment
    /// padding. Callers opting in must establish that such spans contain no
    /// shrinkable branch, or use label fixups instead. No instruction decoding
    /// or speculative patching of unknown bytes occurs here.
    pub fn finish_relaxed(self) -> Option<(Vec<u8>, OffsetMap)> {
        let branches = &self.relaxable_branches;
        let mut short = vec![false; branches.len()];
        let mut branch_fixups = Vec::with_capacity(branches.len());
        for &(start, size) in branches {
            // Consult final bytes, including deliberate condition-code patches.
            match size {
                5 if self.code.get(start) == Some(&0xe9) => {}
                6 if self.code.get(start) == Some(&0x0f)
                    && self
                        .code
                        .get(start + 1)
                        .is_some_and(|op| (0x80..=0x8f).contains(op)) => {}
                _ => return None,
            }
            let site = start.checked_add(size - 4)?;
            let mut matches = self
                .fixups
                .iter()
                .filter(|fx| fx.site == site && matches!(fx.kind, FixupKind::Rel32));
            branch_fixups.push(matches.next()?);
            if matches.next().is_some() {
                return None;
            }
        }
        let map = loop {
            let map = OffsetMap::new(self.code.len(), branches, &short)?;
            let mut changed = false;
            for (i, (&(start, size), fixup)) in branches.iter().zip(&branch_fixups).enumerate() {
                if short[i] {
                    continue;
                }
                let old_target = (*self.labels.get(fixup.label.0)?)?;
                let mut target = map.translate(old_target)?;
                let source = map.translate(start)?;
                // Account for this candidate's own shrink before testing it.
                if old_target >= start + size {
                    target = target.checked_sub(size - 2)?;
                }
                let displacement = target as i128 - (source as i128 + 2);
                if i8::try_from(displacement).is_ok() {
                    short[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break map;
            }
        };
        // Even an otherwise-unused bound label must not name rewritten bytes.
        for offset in self.labels.iter().flatten() {
            map.translate(*offset)?;
        }
        let mut code = Vec::with_capacity(map.translate(self.code.len())?);
        let mut copied = 0;
        for (i, &(start, size)) in branches.iter().enumerate() {
            if !short[i] {
                continue;
            }
            code.extend_from_slice(self.code.get(copied..start)?);
            code.push(if size == 5 {
                0xeb
            } else {
                self.code[start + 1] - 0x10
            });
            let target = map.translate((*self.labels.get(branch_fixups[i].label.0)?)?)?;
            let displacement = target as i128 - (code.len() as i128 + 1);
            code.push(i8::try_from(displacement).ok()? as u8);
            copied = start + size;
        }
        code.extend_from_slice(self.code.get(copied..)?);
        for fixup in &self.fixups {
            if branch_fixups
                .iter()
                .enumerate()
                .any(|(i, branch)| short[i] && std::ptr::eq(*branch, fixup))
            {
                continue;
            }
            let site = map.translate(fixup.site)?;
            let end = map.translate(fixup.site.checked_add(4)?)?;
            if end != site.checked_add(4)? {
                return None;
            }
            let target = map.translate((*self.labels.get(fixup.label.0)?)?)?;
            let base = match fixup.kind {
                FixupKind::Rel32 => end,
                FixupKind::LabelDifference(label) => {
                    map.translate((*self.labels.get(label.0)?)?)?
                }
            };
            let displacement = i32::try_from(target as i128 - base as i128).ok()?;
            code.get_mut(site..end)?
                .copy_from_slice(&displacement.to_le_bytes());
        }
        (code.len() == map.translate(self.code.len())?).then_some((code, map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::Cc;

    fn displacement(code: &[u8], site: usize) -> i32 {
        i32::from_le_bytes(code[site..site + 4].try_into().unwrap())
    }

    #[test]
    fn signed_rel8_boundaries_and_legacy_finish_are_exact() {
        for conditional in [false, true] {
            for padding in [127, 128] {
                let mut a = Asm::new();
                let target = a.label();
                if conditional {
                    a.jcc(Cc::E, target);
                } else {
                    a.jmp(target);
                }
                a.extend_from_slice(&vec![0x90; padding]);
                a.bind(target);
                a.push(0xc3);
                let legacy = a.clone().finish().unwrap();
                assert_eq!(legacy[0], if conditional { 0x0f } else { 0xe9 });
                let (code, map) = a.finish_relaxed().unwrap();
                if padding == 127 {
                    assert_eq!(&code[..2], &[if conditional { 0x74 } else { 0xeb }, 127]);
                    assert_eq!(map.translate(1), None);
                } else {
                    assert_eq!(code, legacy);
                }
                assert_eq!(map.translate(legacy.len()), Some(code.len()));
                assert_eq!(map.translate(legacy.len() + 1), None);
            }
            for padding in [126, 127] {
                let mut a = Asm::new();
                let top = a.label();
                a.bind(top);
                a.extend_from_slice(&vec![0x90; padding]);
                if conditional {
                    a.jcc(Cc::Ne, top);
                } else {
                    a.jmp(top);
                }
                let (code, _) = a.finish_relaxed().unwrap();
                assert_eq!(
                    code[padding],
                    if padding == 126 {
                        if conditional { 0x75 } else { 0xeb }
                    } else if conditional {
                        0x0f
                    } else {
                        0xe9
                    }
                );
                if padding == 126 {
                    assert_eq!(code[padding + 1], 0x80);
                }
            }
        }
    }

    #[test]
    fn shortening_reaches_a_fixed_point_and_maps_every_surviving_boundary() {
        let mut a = Asm::new();
        let end = a.label();
        a.jmp(end); // Initially 128 bytes away; needs the second branch shortened.
        a.jmp(end);
        a.extend_from_slice(&[0x90; 123]);
        a.bind(end);
        let (code, map) = a.finish_relaxed().unwrap();
        assert_eq!(&code[..4], &[0xeb, 125, 0xeb, 123]);
        assert_eq!(code.len(), 127);
        assert_eq!(map.translate(0), Some(0));
        assert_eq!(map.translate(5), Some(2));
        assert_eq!(map.translate(10), Some(4));
        assert_eq!(map.translate(133), Some(127));
        for offset in [1, 2, 3, 4, 6, 7, 8, 9] {
            assert_eq!(map.translate(offset), None);
        }
        for offset in 10..=133 {
            assert_eq!(map.translate(offset), Some(offset - 6));
        }
    }

    #[test]
    fn calls_rip_addresses_and_label_difference_tables_are_repatched() {
        let mut a = Asm::new();
        let base = a.label();
        let target = a.label();
        a.bind(base);
        a.call(target);
        a.lea_rip_label(0, target);
        a.jmp(target);
        a.relative_i32(target, base);
        a.bind(target);
        a.push(0xc3);
        let legacy = a.clone().finish().unwrap();
        assert_eq!(displacement(&legacy, 17), 21);
        let (code, map) = a.finish_relaxed().unwrap();
        assert_eq!(displacement(&code, 1), 13);
        assert_eq!(displacement(&code, 8), 6);
        assert_eq!(&code[12..14], &[0xeb, 4]);
        assert_eq!(displacement(&code, 14), 18);
        assert_eq!(map.translate(17), Some(14));
        assert_eq!(map.translate(21), Some(18));
    }

    #[test]
    fn final_patched_condition_is_used_and_unknown_relative_bytes_are_unchanged() {
        let mut a = Asm::new();
        // A raw short skip stays in a branch-free instruction-local span.
        a.extend_from_slice(&[0x75, 2, 0x90, 0x90]);
        let target = a.label();
        a.jcc(Cc::E, target);
        a.patch_u8(5, 0x87); // Existing emitter practice: change JE to JA.
        a.bind(target);
        a.push(0xc3);
        let (code, _) = a.finish_relaxed().unwrap();
        assert_eq!(code, [0x75, 2, 0x90, 0x90, 0x77, 0, 0xc3]);
    }

    #[test]
    fn malformed_or_interior_targets_fail_closed() {
        let mut a = Asm::new();
        let missing = a.label();
        a.jmp(missing);
        assert!(a.finish_relaxed().is_none());
        let mut a = Asm::new();
        let target = a.label();
        a.jmp(target);
        a.bind(target);
        a.labels[target.0] = Some(2);
        assert!(a.finish_relaxed().is_none());
        let mut a = Asm::new();
        let target = a.label();
        a.jmp(target);
        a.bind(target);
        a.patch_u8(0, 0x90);
        assert!(a.finish_relaxed().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn relaxed_native_direct_call_and_conditional_branch_execute() {
        let mut a = Asm::new();
        let leaf = a.label();
        let result = a.label();
        a.extend_from_slice(&[0x48, 0x83, 0xec, 8]); // SysV call-site alignment
        a.call(leaf);
        a.extend_from_slice(&[0x48, 0x83, 0xc4, 8]);
        a.push(0xc3);
        a.bind(leaf);
        a.extend_from_slice(&[0x31, 0xc0]); // xor eax,eax: ZF=1
        a.jcc(Cc::E, result);
        a.extend_from_slice(&[0xb8, 99, 0, 0, 0, 0xc3]);
        a.bind(result);
        a.extend_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
        let (code, _) = a.finish_relaxed().unwrap();
        let executable = crate::jit::JitBuffer::new(&code).unwrap();
        // SAFETY: this leaf/caller pair takes no arguments and returns EAX.
        let call: unsafe extern "C" fn() -> u64 =
            unsafe { std::mem::transmute(executable.as_ptr()) };
        assert_eq!(unsafe { call() }, 42);
    }
}
